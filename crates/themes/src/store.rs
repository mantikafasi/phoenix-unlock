//! Disk storage for Sunbird's `themes/` folder and `themes.json` config.

use std::{
   fs,
   io::{
      self,
      ErrorKind,
   },
   path::Path,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   css::ThemeMeta,
   spec::ThemeSpec,
};

/// Persisted theme configuration in `<sunbird-home>/themes.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeConfig {
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub active:    Option<String>,
   #[serde(default, skip_serializing_if = "String::is_empty")]
   pub quick_css: String,
}

/// One installed `.theme.css` or `.theme.json` file discovered in `themes/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledTheme {
   pub file: String,
   pub meta: ThemeMeta,
}

fn strip_theme_ext(file: &str) -> Option<&str> {
   let lower = file.to_ascii_lowercase();
   if lower.ends_with(".theme.css") {
      Some(&file[..file.len() - ".theme.css".len()])
   } else if lower.ends_with(".theme.json") {
      Some(&file[..file.len() - ".theme.json".len()])
   } else {
      None
   }
}

/// Returns whether `file` is a safe single-segment theme filename.
#[must_use]
pub fn valid_theme_file(file: &str) -> bool {
   if file.is_empty()
      || file.contains(['/', '\\', ':', '\0'])
      || file.starts_with('.')
      || file.contains("..")
   {
      return false;
   }
   strip_theme_ext(file).is_some_and(|stem| !stem.is_empty())
}

/// Reads a validated theme file inside `dir`, canonicalizing both `dir` and
/// the target path so symlinks cannot escape the themes directory.
pub fn read_contained_theme(dir: &Path, file: &str) -> Result<String, String> {
   if !valid_theme_file(file) {
      return Err(format!("invalid theme file {file}"));
   }
   let canonical_dir =
      fs::canonicalize(dir).map_err(|err| format!("reading themes dir: {err}"))?;
   let canonical_file =
      fs::canonicalize(dir.join(file)).map_err(|_| format!("no theme {file}"))?;
   if !canonical_file.starts_with(&canonical_dir) || !canonical_file.is_file() {
      return Err(format!("no theme {file}"));
   }
   fs::read_to_string(&canonical_file).map_err(|err| format!("reading {file}: {err}"))
}

/// Lists all valid `*.theme.css` and `*.theme.json` files in `dir`, sorted by
/// filename.
#[must_use]
pub fn list_themes(dir: &Path) -> Vec<InstalledTheme> {
   let Ok(canonical_dir) = fs::canonicalize(dir) else {
      return Vec::new();
   };
   let mut out: Vec<InstalledTheme> = fs::read_dir(&canonical_dir)
      .into_iter()
      .flatten()
      .flatten()
      .filter_map(|entry| {
         let file = entry.file_name().into_string().ok()?;
         if !valid_theme_file(&file) {
            return None;
         }
         let canonical_file = fs::canonicalize(entry.path()).ok()?;
         if !canonical_file.starts_with(&canonical_dir) || !canonical_file.is_file() {
            return None;
         }
         let stem = strip_theme_ext(&file)?.to_owned();
         let mut meta = fs::read_to_string(&canonical_file)
            .ok()
            .and_then(|raw| ThemeSpec::parse_file(&file, &raw).ok())
            .map_or_default(|spec| spec.meta);
         if meta.name.is_empty() {
            meta.name = stem;
         }
         Some(InstalledTheme { file, meta })
      })
      .collect();
   out.sort_by(|a, b| a.file.cmp(&b.file));
   out
}

/// Reads `themes.json`, returning defaults when missing or unreadable.
#[must_use]
pub fn load_config(path: &Path) -> ThemeConfig {
   match fs::read_to_string(path) {
      Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
      Err(err) if err.kind() == ErrorKind::NotFound => ThemeConfig::default(),
      Err(_) => ThemeConfig::default(),
   }
}

/// Atomically writes `themes.json` via a same-directory temp file + sync +
/// rename so a crash or concurrent launch cannot leave a truncated config.
pub fn save_config(path: &Path, config: &ThemeConfig) -> io::Result<()> {
   use std::io::Write as _;

   if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
   }
   let json = serde_json::to_string_pretty(config)
      .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
   let stamp = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
   let tmp_name = format!(
      ".{}.{}.{stamp}.tmp",
      path.file_name()
         .and_then(|n| n.to_str())
         .unwrap_or("themes.json"),
      std::process::id(),
   );
   let tmp = path.with_file_name(tmp_name);
   let write_res = (|| -> io::Result<()> {
      let mut file = fs::File::create(&tmp)?;
      file.write_all(json.as_bytes())?;
      file.sync_all()?;
      Ok(())
   })();
   if let Err(err) = write_res {
      let _ = fs::remove_file(&tmp);
      return Err(err);
   }
   if let Err(err) = fs::rename(&tmp, path) {
      let _ = fs::remove_file(&tmp);
      return Err(err);
   }
   Ok(())
}

/// Loads the active theme from `themes_dir` and layers `config.quick_css` on
/// top if non-empty. Returns `None` when neither produces any active overrides.
#[must_use]
pub fn load_active_spec(themes_dir: &Path, config: &ThemeConfig) -> Option<ThemeSpec> {
   let base = config
      .active
      .as_deref()
      .and_then(|file| {
         let raw = read_contained_theme(themes_dir, file).ok()?;
         ThemeSpec::parse_file(file, &raw).ok()
      });
   let quick = (!config.quick_css.trim().is_empty())
      .then(|| ThemeSpec::parse_css(&config.quick_css));
   let combined = match (base, quick) {
      (Some(base), Some(quick)) => base.merge(&quick),
      (Some(base), None) => base,
      (None, Some(quick)) => quick,
      (None, None) => return None,
   };
   combined.is_active().then_some(combined)
}

#[cfg(test)]
mod tests {
   use std::time::{
      SystemTime,
      UNIX_EPOCH,
   };

   use super::*;
   use crate::{
      color::{
         Color,
         parse_css_color,
      },
      css::ThemeMode,
   };

   fn temp_dir(tag: &str) -> std::path::PathBuf {
      let stamp = SystemTime::now()
         .duration_since(UNIX_EPOCH)
         .expect("clock")
         .as_nanos();
      let dir = std::env::temp_dir().join(format!(
         "phoenix-themes-{tag}-{}-{stamp}",
         std::process::id()
      ));
      fs::create_dir_all(&dir).expect("mkdir");
      dir
   }

   #[test]
   fn parses_css_colors_and_wcag_contrast() {
      assert_eq!(parse_css_color("#f0a"), Some(Color::rgb(0xff, 0x00, 0xaa)));
      assert_eq!(
         parse_css_color("#f0a8"),
         Some(Color::rgba(0xff, 0x00, 0xaa, 0x88))
      );
      assert_eq!(
         parse_css_color("#1e1e2e"),
         Some(Color::rgb(0x1e, 0x1e, 0x2e))
      );
      assert_eq!(
         parse_css_color("rgb(30 30 46 / 50%)"),
         Some(Color::rgba(30, 30, 46, 128))
      );
      assert_eq!(
         parse_css_color("rgba(122, 162, 247, 0.5)"),
         Some(Color::rgba(122, 162, 247, 128))
      );
      assert_eq!(parse_css_color("transparent"), Some(Color::TRANSPARENT));
      let hsl = parse_css_color("hsl(240deg, 100%, 50%)").expect("hsl");
      assert_eq!(hsl, Color::rgb(0, 0, 255));
      assert_eq!(Color::rgb(20, 20, 30).readable_foreground(), Color::WHITE);
      assert_eq!(
         Color::rgb(245, 240, 120).readable_foreground(),
         Color::DARK_INK
      );
   }

   #[test]
   fn resolves_scoped_vars_cycles_and_direct_precedence() {
      let css = r"
         /**
          * @name Catppuccin Dual
          * @author Sunbird
          * @version 1.0.0
          * @description Adaptive dark and light palette
          */
         :root {
            --accent-base: #cba6f7;
            --accent-color: var(--accent-base);
            --cycle-a: var(--cycle-b, #112233);
            --cycle-b: var(--cycle-a);
            --radius-scale: 125%;
         }
         .theme-dark {
            --bg-primary: #1e1e2e;
            --background-base-low: #191928 !important;
         }
         .theme-light {
            --bg-primary: #eff1f5;
            --accent-color: var(--missing, #8839ef);
         }
      ";

      let spec = ThemeSpec::parse_css(css);
      assert_eq!(spec.meta.name, "Catppuccin Dual");
      assert_eq!(spec.meta.author.as_deref(), Some("Sunbird"));
      assert_eq!(spec.meta.version.as_deref(), Some("1.0.0"));
      assert_eq!(spec.radius_scale, Some(1.25));

      // Direct `--background-base-low` beats derived `--bg-primary`.
      let dark = spec.resolve(ThemeMode::Dark);
      assert_eq!(dark.mode, ThemeMode::Dark);
      assert_eq!(
         dark.tokens.get("background-base-low"),
         Some(&Color::rgb(0x19, 0x19, 0x28))
      );
      assert_eq!(
         dark.tokens.get("background-brand"),
         Some(&Color::rgb(0xcb, 0xa6, 0xf7))
      );
      // Light mode uses `.theme-light` + fallback `var()`.
      let light = spec.resolve(ThemeMode::Light);
      assert_eq!(light.mode, ThemeMode::Light);
      assert_eq!(
         light.tokens.get("background-base-low"),
         Some(&Color::rgb(0xef, 0xf1, 0xf5))
      );
      assert_eq!(
         light.tokens.get("background-brand"),
         Some(&Color::rgb(0x88, 0x39, 0xef))
      );
      // Light mode derives darker secondary than primary; dark mode derives
      // darker secondary than primary too, while elevated moves opposite.
      let dark_elev = dark.tokens["background-surface-high"].to_hsla().l;
      let dark_prim = Color::rgb(0x1e, 0x1e, 0x2e).to_hsla().l;
      assert!(dark_elev > dark_prim);
      let light_elev = light.tokens["background-surface-high"].to_hsla().l;
      let light_prim = light.tokens["background-base-low"].to_hsla().l;
      assert!(light_elev < light_prim);
   }

   #[test]
   fn parses_json_manifest_and_quick_css_merge() {
      let json = r##"{
         "name": "Midnight JSON",
         "author": "coco",
         "mode": "dark",
         "tokens": {
            "bg-primary": "#141825",
            "accent-color": "#7289da"
         }
      }"##;
      let base = ThemeSpec::parse_json(json).expect("manifest");
      let quick = ThemeSpec::parse_css("--text-brand: #ff79c6;");
      let merged = base.merge(&quick);
      let resolved = merged.resolve(ThemeMode::Light);
      // Explicit `mode: "dark"` overrides Discord's light base mode.
      assert_eq!(resolved.mode, ThemeMode::Dark);
      assert_eq!(
         resolved.tokens.get("background-base-low"),
         Some(&Color::rgb(0x14, 0x18, 0x25))
      );
      assert_eq!(
         resolved.tokens.get("text-brand"),
         Some(&Color::rgb(0xff, 0x79, 0xc6))
      );
   }

   #[test]
   fn validates_filenames_and_persists_store() {
      assert!(valid_theme_file("dracula.theme.css"));
      assert!(valid_theme_file("nord.theme.json"));
      assert!(!valid_theme_file(".theme.css"));
      assert!(!valid_theme_file("../escape.theme.css"));
      assert!(!valid_theme_file("sub/a.theme.css"));
      assert!(!valid_theme_file("plain.css"));

      let dir = temp_dir("store");
      let themes_dir = dir.join("themes");
      fs::create_dir_all(&themes_dir).expect("mkdir themes");
      fs::write(
         themes_dir.join("dracula.theme.css"),
         "/* @name Dracula */\n:root { --bg-primary: #282a36; }",
      )
      .expect("write css");
      fs::write(themes_dir.join("ignored.txt"), "nope").expect("write txt");

      let listed = list_themes(&themes_dir);
      assert_eq!(listed.len(), 1);
      assert_eq!(listed[0].file, "dracula.theme.css");
      assert_eq!(listed[0].meta.name, "Dracula");

      let cfg_path = dir.join("themes.json");
      let mut cfg = ThemeConfig {
         active:    Some("dracula.theme.css".to_owned()),
         quick_css: "--accent-color: #bd93f9;".to_owned(),
      };
      save_config(&cfg_path, &cfg).expect("save");
      let loaded = load_config(&cfg_path);
      assert_eq!(loaded, cfg);

      let spec = load_active_spec(&themes_dir, &loaded).expect("active spec");
      let palette = spec.resolve(ThemeMode::Dark);
      assert_eq!(
         palette.tokens.get("background-base-low"),
         Some(&Color::rgb(0x28, 0x2a, 0x36))
      );
      assert_eq!(
         palette.tokens.get("background-brand"),
         Some(&Color::rgb(0xbd, 0x93, 0xf9))
      );

      cfg.active = None;
      cfg.quick_css.clear();
      assert!(load_active_spec(&themes_dir, &cfg).is_none());

      let _ = fs::remove_dir_all(&dir);
   }

   #[test]
   fn handles_light_hierarchy_css_edge_cases_and_fanout_budget() {
      use std::fmt::Write as _;

      // Light mode: secondary and tertiary must darken, never wash out to white.
      let light_only = ThemeSpec::parse_css(":root { --bg-primary: #eff1f5; --accent: red; }");
      let light = light_only.resolve(ThemeMode::Light);
      let l_primary = light.tokens["background-base-low"].to_hsla().l;
      let l_secondary = light.tokens["background-base-lower"].to_hsla().l;
      let l_tertiary = light.tokens["background-base-lowest"].to_hsla().l;
      assert!(l_secondary < l_primary);
      assert!(l_tertiary < l_secondary);
      assert_eq!(
         light.tokens.get("background-brand"),
         Some(&Color::rgb(255, 0, 0))
      );

      // Selector lists, media queries, component exclusion, VAR(), quoted !important.
      let css = r#"
         :root, .theme-light {
            --base: white;
            --bg-primary: VAR(--base);
            --quoted-var: "Not !important Value";
         }
         .sidebar {
            --bg-primary: #ff0000;
         }
         @media (prefers-color-scheme: light) {
            :root { --accent-color: blue; }
         }
         @media (prefers-color-scheme: dark) {
            :root { --accent-color: lime; }
         }
      "#;
      let spec = ThemeSpec::parse_css(css);
      let dark = spec.resolve(ThemeMode::Dark);
      assert_eq!(dark.tokens["background-base-low"], Color::WHITE);
      assert_eq!(dark.tokens["background-brand"], Color::rgb(0, 255, 0));
      assert_eq!(
         spec.root.get("--quoted-var").map(String::as_str),
         Some("\"Not !important Value\"")
      );
      let light2 = spec.resolve(ThemeMode::Light);
      assert_eq!(light2.tokens["background-brand"], Color::rgb(0, 0, 255));

      // Exponential 4-way fan-out chain of depth 10 must finish instantaneously.
      let mut fanout = String::from(":root {\n--v0: #112233;\n");
      for i in 1..=10 {
         let prev = i - 1;
         let _ = writeln!(
            fanout,
            "--v{i}: var(--v{prev}) var(--v{prev}) var(--v{prev}) var(--v{prev});"
         );
      }
      fanout.push_str("--bg-primary: var(--v0);\n}");
      let start = std::time::Instant::now();
      let resolved = ThemeSpec::parse_css(&fanout).resolve(ThemeMode::Dark);
      assert!(start.elapsed().as_millis() < 500);
      assert_eq!(
         resolved.tokens.get("background-base-low"),
         Some(&Color::rgb(0x11, 0x22, 0x33))
      );

      // Optional manifest name + case-insensitive theme filenames.
      let nameless = ThemeSpec::parse_json(r##"{"tokens":{"bg-primary":"#101010"}}"##)
         .expect("nameless json");
      assert_eq!(nameless.meta.name, "");
      assert!(valid_theme_file("UPPER.THEME.CSS"));
      assert!(valid_theme_file("UPPER.THEME.JSON"));

      // `@import` and `@charset` before and between blocks must not drop rules.
      let import_css = r#"
         @charset "utf-8";
         @import url("https://example.com/base.css");
         :root { --bg-primary: #112233; }
         @import url("https://example.com/extra.css");
         .theme-dark { --accent: #445566; }
      "#;
      let imported = ThemeSpec::parse_css(import_css).resolve(ThemeMode::Dark);
      assert_eq!(
         imported.tokens.get("background-base-low"),
         Some(&Color::rgb(0x11, 0x22, 0x33))
      );
      assert_eq!(
         imported.tokens.get("background-brand"),
         Some(&Color::rgb(0x44, 0x55, 0x66))
      );

      // Deeply nested valid `@media` blocks must terminate quickly (depth cap).
      let mut nested = String::new();
      for _ in 0..2000 {
         nested.push_str("@media (prefers-color-scheme: dark) { ");
      }
      nested.push_str(":root { --bg-primary: #123456; }");
      for _ in 0..2000 {
         nested.push_str(" }");
      }
      let t0 = std::time::Instant::now();
      let _ = ThemeSpec::parse_css(&nested);
      assert!(t0.elapsed().as_millis() < 200);
   }
}
