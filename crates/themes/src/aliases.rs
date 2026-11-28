//! High-level `.theme.css` palette aliases expanded into Discord Mana semantic
//! token families, aware of dark vs light lightness direction and WCAG contrast.

use std::collections::BTreeMap;

use crate::{
   color::{
      Color,
      parse_css_color,
   },
   css::ThemeMode,
};

/// Non-color control variables that must never be passed as Mana color tokens.
pub(crate) fn is_control_var(name: &str) -> bool {
   name == "--radius-scale"
}

/// High-level shorthand aliases that expand to semantic families rather than
/// naming a Mana token directly.
fn is_alias_only(name: &str) -> bool {
   matches!(
      name,
      "--bg-1"
         | "--bg-2"
         | "--bg-3"
         | "--bg-4"
         | "--bg-tertiary"
         | "--bg-secondary"
         | "--bg-primary"
         | "--bg-elevated"
         | "--accent-color"
         | "--accent"
         | "--accent-1"
         | "--accent-2"
         | "--ui-border-color"
         | "--text-1"
         | "--text-2"
         | "--text-3"
         | "--text-4"
   )
}

fn pick(vars: &BTreeMap<String, String>, keys: &[&str]) -> Option<Color> {
   keys.iter().find_map(|key| parse_css_color(vars.get(*key)?))
}

fn expand_surfaces(
   vars: &BTreeMap<String, String>,
   mode: ThemeMode,
   tokens: &mut BTreeMap<String, Color>,
) {
   let tertiary = pick(vars, &[
      "--bg-tertiary",
      "--background-tertiary",
      "--bg-base-tertiary",
      "--bg-1",
   ]);
   let secondary = pick(vars, &[
      "--bg-secondary",
      "--background-secondary",
      "--bg-base-secondary",
      "--bg-2",
   ]);
   let primary = pick(vars, &[
      "--bg-primary",
      "--background-primary",
      "--bg-base-primary",
      "--bg-3",
   ]);
   let elevated = pick(vars, &[
      "--bg-elevated",
      "--background-floating",
      "--background-modifier-hover",
      "--bg-4",
   ]);

   if let Some(base) = primary.or(secondary).or(tertiary).or(elevated) {
      let step = match mode {
         ThemeMode::Dark => 1.0_f32,
         ThemeMode::Light => -1.0_f32,
      };
      let primary = primary.unwrap_or(base);
      let secondary = secondary.unwrap_or_else(|| primary.shift_lightness(-0.03));
      let tertiary = tertiary.unwrap_or_else(|| secondary.shift_lightness(-0.03));
      let elevated = elevated.unwrap_or_else(|| primary.shift_lightness(0.05 * step));
      let highest = elevated.shift_lightness(0.04 * step);
      let hover = primary.shift_lightness(0.04 * step);
      let active = primary.shift_lightness(0.07 * step);
      let subtle = primary.mix(elevated, 0.55);

      for (name, color) in [
         ("background-base-lowest", tertiary),
         ("bg-base-tertiary", tertiary),
         ("background-tertiary", tertiary),
         ("background-base-lower", secondary),
         ("bg-base-secondary", secondary),
         ("background-secondary", secondary),
         ("background-secondary-alt", tertiary.mix(secondary, 0.5)),
         ("background-base-low", primary),
         ("bg-base-primary", primary),
         ("background-primary", primary),
         ("chat-background-default", primary),
         ("home-background", primary),
         ("background-surface-high", elevated),
         ("background-surface-higher", highest),
         (
            "background-surface-highest",
            highest.shift_lightness(0.03 * step),
         ),
         ("background-floating", elevated),
         ("bg-surface-overlay", elevated),
         ("bg-surface-raised", elevated),
         ("card-background-default", secondary),
         ("card-primary-bg", secondary),
         ("card-secondary-bg", tertiary),
         ("modal-background", primary),
         ("modal-footer-background", secondary),
         ("input-background-default", tertiary.mix(secondary, 0.5)),
         ("background-mod-subtle", subtle.with_alpha(128)),
         ("background-mod-normal", hover.with_alpha(170)),
         ("background-mod-strong", active.with_alpha(200)),
         ("background-modifier-hover", hover.with_alpha(128)),
         ("background-modifier-active", active.with_alpha(165)),
         ("background-modifier-selected", active.with_alpha(180)),
      ] {
         tokens.insert(name.to_owned(), color);
      }

      let chat_input =
         pick(vars, &["--channeltextarea-background"]).unwrap_or_else(|| primary.mix(elevated, 0.65));
      tokens.insert("chat-input-container-background".to_owned(), chat_input);
      tokens.insert("channeltextarea-background".to_owned(), chat_input);
   } else if let Some(chat_input) = pick(vars, &["--channeltextarea-background"]) {
      tokens.insert("chat-input-container-background".to_owned(), chat_input);
      tokens.insert("channeltextarea-background".to_owned(), chat_input);
   }
}

fn expand_accent(vars: &BTreeMap<String, String>, tokens: &mut BTreeMap<String, Color>) {
   let Some(accent) = pick(vars, &[
      "--accent-color",
      "--accent",
      "--brand-experiment",
      "--brand-500",
      "--accent-1",
      "--accent-2",
   ]) else {
      return;
   };
   let dir = if accent.to_hsla().l > 0.55 { -1.0 } else { 1.0 };
   let hover = accent.shift_lightness(0.06 * dir);
   let active = accent.shift_lightness(0.11 * dir);
   let on_accent = accent.readable_foreground();
   let mention_bg = accent.with_alpha(52);

   for (name, color) in [
      ("background-brand", accent),
      ("brand-500", accent),
      ("brand-560", hover),
      ("brand-600", active),
      ("button-filled-brand-background", accent),
      ("button-filled-brand-background-hover", hover),
      ("button-filled-brand-background-active", active),
      ("button-filled-brand-text", on_accent),
      ("control-primary-background-default", accent),
      ("control-primary-background-hover", hover),
      ("control-primary-background-active", active),
      ("control-primary-text-default", on_accent),
      ("text-brand", accent),
      ("icon-brand", accent),
      ("text-link", accent),
      ("mention-foreground", accent),
      ("mention-background", mention_bg),
      ("focus-primary", accent),
   ] {
      tokens.insert(name.to_owned(), color);
   }
}

fn expand_text_and_borders(
   vars: &BTreeMap<String, String>,
   mode: ThemeMode,
   tokens: &mut BTreeMap<String, Color>,
) {
   if let Some(normal) = pick(vars, &["--text-normal", "--text-primary", "--text-1", "--text-2"]) {
      let strong = match mode {
         ThemeMode::Dark => normal.shift_lightness(0.06),
         ThemeMode::Light => normal.shift_lightness(-0.06),
      };
      for (name, color) in [
         ("text-default", normal),
         ("text-normal", normal),
         ("text-strong", strong),
         ("header-primary", strong),
         ("interactive-text-default", normal),
         ("interactive-text-active", strong),
         ("icon-default", normal),
         ("icon-strong", strong),
      ] {
         tokens.insert(name.to_owned(), color);
      }

      // Discord's dark table keeps these white over surfaces themes recolor.
      if mode == ThemeMode::Light || normal.to_hsla().l < 0.5 {
         for (name, color) in [
            ("control-secondary-text-default", normal),
            ("control-secondary-text-hover", strong),
            ("control-secondary-text-active", strong),
            ("control-secondary-icon-default", normal),
            ("control-secondary-icon-hover", strong),
            ("control-secondary-icon-active", strong),
            ("button-outline-primary-text", normal),
            ("control-icon-only-icon-hover", strong),
            ("control-icon-only-icon-active", strong),
            ("interactive-text-hover", strong),
            ("interactive-icon-hover", strong),
            ("interactive-icon-active", strong),
            ("tab-label-text-focused", strong),
            ("redesign-channel-name-text", strong),
            ("badge-secondary-text", normal),
            ("badge-common-text", normal),
         ] {
            tokens.insert(name.to_owned(), color);
         }
      }
   }

   if let Some(muted) = pick(vars, &[
      "--text-muted",
      "--text-secondary",
      "--text-3",
      "--text-4",
   ]) {
      let subtle = match mode {
         ThemeMode::Dark => muted.shift_lightness(-0.05),
         ThemeMode::Light => muted.shift_lightness(0.05),
      };
      for (name, color) in [
         ("text-muted", muted),
         ("text-subtle", subtle),
         ("header-secondary", muted),
         ("channels-default", muted),
         ("interactive-text-muted", muted),
         ("icon-muted", muted),
         ("icon-subtle", subtle),
         ("input-placeholder-text-default", muted),
      ] {
         tokens.insert(name.to_owned(), color);
      }
   }

   if let Some(border) = pick(vars, &[
      "--ui-border-color",
      "--background-modifier-accent",
      "--border-subtle",
   ]) {
      let strong = match mode {
         ThemeMode::Dark => border.shift_lightness(0.08),
         ThemeMode::Light => border.shift_lightness(-0.08),
      };
      let faint = border.with_alpha(border.a / 2);
      for (name, color) in [
         ("border-subtle", border),
         ("border-normal", border),
         ("border-faint", faint),
         ("border-strong", strong),
         ("app-border-frame", border),
         ("background-modifier-accent", border),
         ("input-border-default", border),
      ] {
         tokens.insert(name.to_owned(), color);
      }
   }
}

/// Expands high-level aliases first, then applies every explicit `--<token>`
/// color override on top so direct Mana tokens always take precedence.
#[must_use]
pub fn build_token_map(
   resolved_vars: &BTreeMap<String, String>,
   mode: ThemeMode,
) -> BTreeMap<String, Color> {
   let mut tokens = BTreeMap::new();
   expand_surfaces(resolved_vars, mode, &mut tokens);
   expand_accent(resolved_vars, &mut tokens);
   expand_text_and_borders(resolved_vars, mode, &mut tokens);

   for (raw_key, raw_val) in resolved_vars {
      if is_control_var(raw_key) || is_alias_only(raw_key) {
         continue;
      }
      let Some(token_name) = raw_key.strip_prefix("--") else {
         continue;
      };
      if let Some(color) = parse_css_color(raw_val) {
         tokens.insert(token_name.to_owned(), color);
      }
   }

   tokens
}
