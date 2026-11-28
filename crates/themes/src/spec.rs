//! Normalized [`ThemeSpec`] and mode-resolved [`ResolvedPalette`].

use std::collections::BTreeMap;

use crate::{
   aliases::build_token_map,
   color::{
      Color,
      parse_number,
   },
   css::{
      ThemeMeta,
      ThemeMode,
      parse_css,
      resolve_all_vars,
   },
   manifest::ThemeManifest,
};

/// Platform-agnostic theme specification before resolving against Discord's
/// current dark/light mode.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeSpec {
   pub meta:         ThemeMeta,
   pub root:         BTreeMap<String, String>,
   pub dark:         BTreeMap<String, String>,
   pub light:        BTreeMap<String, String>,
   pub radius_scale: Option<f32>,
}

/// Fully resolved token table and controls for a concrete [`ThemeMode`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPalette {
   pub mode:         ThemeMode,
   pub tokens:       BTreeMap<String, Color>,
   pub radius_scale: Option<f32>,
}

impl ResolvedPalette {
   #[must_use]
   #[allow(clippy::missing_const_for_fn)] // BTreeMap::is_empty is not const on stable
   pub fn is_active(&self) -> bool {
      !self.tokens.is_empty() || self.radius_scale.is_some()
   }
}

fn parse_float(raw: Option<&str>, range: std::ops::RangeInclusive<f32>) -> Option<f32> {
   let (v, unit) = parse_number(raw?.trim())?;
   let v = if unit.starts_with('%') { v / 100.0 } else { v };
   range.contains(&v).then_some(v)
}

fn extract_radius_scale(vars: &BTreeMap<String, String>) -> Option<f32> {
   parse_float(vars.get("--radius-scale").map(String::as_str), 0.0..=2.5)
}

impl ThemeSpec {
   /// Parses a `.theme.css` stylesheet.
   #[must_use]
   pub fn parse_css(css: &str) -> Self {
      let parsed = parse_css(css);
      let radius_scale = extract_radius_scale(&parsed.root);
      Self {
         meta: parsed.meta,
         root: parsed.root,
         dark: parsed.dark,
         light: parsed.light,
         radius_scale,
      }
   }

   /// Parses a `.theme.json` manifest.
   pub fn parse_json(json: &str) -> Result<Self, serde_json::Error> {
      let manifest: ThemeManifest = serde_json::from_str(json)?;
      Ok(Self::from(manifest))
   }

   /// Parses either `.theme.json` or `.theme.css` based on file extension.
   pub fn parse_file(file_name: &str, content: &str) -> Result<Self, String> {
      if file_name.to_ascii_lowercase().ends_with(".theme.json") {
         Self::parse_json(content).map_err(|err| format!("invalid theme json: {err}"))
      } else {
         Ok(Self::parse_css(content))
      }
   }

   /// Layers `overlay` (such as user quick-css) on top of `self`.
   #[must_use]
   pub fn merge(&self, overlay: &Self) -> Self {
      let mut out = self.clone();
      if !overlay.meta.name.is_empty() && out.meta.name.is_empty() {
         out.meta.name.clone_from(&overlay.meta.name);
      }
      if overlay.meta.mode.is_some() {
         out.meta.mode = overlay.meta.mode;
      }
      out.root.extend(
         overlay
            .root
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
      );
      out.dark.extend(
         overlay
            .dark
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
      );
      out.light.extend(
         overlay
            .light
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
      );
      if overlay.radius_scale.is_some() {
         out.radius_scale = overlay.radius_scale;
      }
      out
   }

   /// Resolves variables, aliases, and control flags for Discord's current
   /// `base_mode` (overridden by `@mode` when explicitly declared).
   #[must_use]
   pub fn resolve(&self, base_mode: ThemeMode) -> ResolvedPalette {
      let mode = self.meta.mode.unwrap_or(base_mode);
      let mut raw = self.root.clone();
      let scoped = match mode {
         ThemeMode::Dark => &self.dark,
         ThemeMode::Light => &self.light,
      };
      raw.extend(scoped.iter().map(|(k, v)| (k.clone(), v.clone())));

      let resolved_vars = resolve_all_vars(&raw);
      let radius_scale = extract_radius_scale(&resolved_vars).or(self.radius_scale);
      let tokens = build_token_map(&resolved_vars, mode);
      ResolvedPalette {
         mode,
         tokens,
         radius_scale,
      }
   }

   /// Whether this theme produces any active override in dark or light mode.
   #[must_use]
   pub fn is_active(&self) -> bool {
      self.resolve(ThemeMode::Dark).is_active() || self.resolve(ThemeMode::Light).is_active()
   }
}
