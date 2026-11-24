//! Color parsing and Discord token-to-ARGB remapping.

use std::{
   collections::{BTreeMap, HashMap},
   sync::OnceLock,
};

use serde::Deserialize;

use crate::themes::state::{ColorsSelection, ThemeBase};

const TOKENS_JSON: &str = include_str!("tokens.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
   Background,
   Text,
   Border,
   Generic,
}

impl Category {
   fn from_code(code: &str) -> Self {
      match code {
         "b" => Self::Background,
         "t" => Self::Text,
         "r" => Self::Border,
         _ => Self::Generic,
      }
   }
}

#[derive(Deserialize)]
struct RawTokens {
   raw: HashMap<String, u32>,
   semantic: HashMap<String, (String, u32, u32, u32, u32)>,
}

pub struct TokenTable {
   raw: HashMap<String, u32>,
   semantic: HashMap<String, (Category, [u32; 4])>,
}

impl TokenTable {
   pub fn global() -> &'static Self {
      static TABLE: OnceLock<TokenTable> = OnceLock::new();
      TABLE.get_or_init(|| {
         let parsed: RawTokens =
            serde_json::from_str(TOKENS_JSON).expect("embedded tokens.json is valid");
         let semantic = parsed
            .semantic
            .into_iter()
            .map(|(name, (code, dark, darker, light, midnight))| {
               (
                  name,
                  (Category::from_code(&code), [dark, darker, light, midnight]),
               )
            })
            .collect();
         Self {
            raw: parsed.raw,
            semantic,
         }
      })
   }

   /// Looks up a raw token's default ARGB (case-insensitive).
   #[must_use]
   pub fn raw_argb(&self, name: &str) -> Option<u32> {
      self.raw.get(&name.to_ascii_uppercase()).copied()
   }

   /// Looks up a semantic token's category and default ARGB for `base`.
   #[must_use]
   pub fn semantic_argb(&self, name: &str, base: ThemeBase) -> Option<(Category, u32)> {
      let (category, values) = self.semantic.get(&name.to_ascii_uppercase())?;
      Some((*category, values[base.token_index()]))
   }

   /// All raw tokens formatted as `#rrggbb` (or `#rrggbbaa` when translucent).
   #[must_use]
   pub fn raw_hex_map(&self) -> BTreeMap<String, String> {
      self
         .raw
         .iter()
         .map(|(name, &argb)| {
            let r = (argb >> 16) & 0xFF;
            let g = (argb >> 8) & 0xFF;
            let b = argb & 0xFF;
            let a = (argb >> 24) & 0xFF;
            let hex = if a == 0xFF {
               format!("#{r:02x}{g:02x}{b:02x}")
            } else {
               format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
            };
            (name.clone(), hex)
         })
         .collect()
   }
}

/// Parses `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, `rgb(...)`, or `rgba(...)`
/// into a 32-bit ARGB integer (`0xAARRGGBB`).
#[must_use]
pub fn parse_color(input: &str) -> Option<u32> {
   let trimmed = input.trim();
   if let Some(hex) = trimmed.strip_prefix('#') {
      return parse_hex(hex);
   }
   if let Some(inner) = trimmed
      .strip_prefix("rgba(")
      .or_else(|| trimmed.strip_prefix("RGBA("))
      .and_then(|rest| rest.strip_suffix(')'))
   {
      return parse_rgb_fn(inner, true);
   }
   if let Some(inner) = trimmed
      .strip_prefix("rgb(")
      .or_else(|| trimmed.strip_prefix("RGB("))
      .and_then(|rest| rest.strip_suffix(')'))
   {
      return parse_rgb_fn(inner, false);
   }
   None
}

fn parse_hex(hex: &str) -> Option<u32> {
   match hex.len() {
      3 => {
         let r = u32::from_str_radix(&hex[0..1], 16).ok()? * 0x11;
         let g = u32::from_str_radix(&hex[1..2], 16).ok()? * 0x11;
         let b = u32::from_str_radix(&hex[2..3], 16).ok()? * 0x11;
         Some(0xFF00_0000 | (r << 16) | (g << 8) | b)
      },
      4 => {
         let r = u32::from_str_radix(&hex[0..1], 16).ok()? * 0x11;
         let g = u32::from_str_radix(&hex[1..2], 16).ok()? * 0x11;
         let b = u32::from_str_radix(&hex[2..3], 16).ok()? * 0x11;
         let a = u32::from_str_radix(&hex[3..4], 16).ok()? * 0x11;
         Some((a << 24) | (r << 16) | (g << 8) | b)
      },
      6 => {
         let rgb = u32::from_str_radix(hex, 16).ok()?;
         Some(0xFF00_0000 | rgb)
      },
      8 => {
         let rgba = u32::from_str_radix(hex, 16).ok()?;
         let rgb = rgba >> 8;
         let a = rgba & 0xFF;
         Some((a << 24) | rgb)
      },
      _ => None,
   }
}

fn parse_rgb_fn(inner: &str, with_alpha: bool) -> Option<u32> {
   let parts = inner.split(',').map(str::trim).collect::<Vec<_>>();
   let expected = if with_alpha { 4 } else { 3 };
   if parts.len() != expected {
      return None;
   }
   let r = parse_channel(parts[0])?;
   let g = parse_channel(parts[1])?;
   let b = parse_channel(parts[2])?;
   let a = if with_alpha {
      let alpha = parts[3].parse::<f64>().ok()?;
      if !alpha.is_finite() {
         return None;
      }
      #[expect(
         clippy::cast_possible_truncation,
         clippy::cast_sign_loss,
         reason = "clamped to [0.0, 255.0] before rounding"
      )]
      let scaled = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
      scaled
   } else {
      0xFF
   };
   Some((a << 24) | (r << 16) | (g << 8) | b)
}

fn parse_channel(part: &str) -> Option<u32> {
   let value = part.parse::<f64>().ok()?;
   if !value.is_finite() {
      return None;
   }
   #[expect(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      reason = "clamped to [0.0, 255.0] before rounding"
   )]
   let clamped = value.clamp(0.0, 255.0).round() as u32;
   Some(clamped)
}

/// Precomputed ARGB remapping table for one [`ColorsSelection`] and
/// [`ThemeBase`].
#[derive(Clone, Debug, Default)]
pub struct ColorPalette {
   /// Category-scoped semantic overrides (`(category, default_argb) -> new_argb`).
   semantic_by_category: HashMap<(Category, u32), u32>,
   /// Semantic overrides from the `generic` category or fallback lookup.
   semantic_any: HashMap<u32, u32>,
   /// Raw color overrides keyed by default 24-bit RGB (`0x00RRGGBB -> new_argb`).
   raw_by_rgb: HashMap<u32, u32>,
}

impl ColorPalette {
   #[must_use]
   pub fn build(selection: &ColorsSelection, base: ThemeBase) -> Self {
      let table = TokenTable::global();
      let mut palette = Self::default();

      for (name, hex) in &selection.values.raw {
         let Some(replacement) = parse_color(hex) else {
            continue;
         };
         let Some(default_argb) = table.raw_argb(name) else {
            continue;
         };
         palette
            .raw_by_rgb
            .insert(default_argb & 0x00FF_FFFF, replacement);
      }

      // Discord's dark-family palettes (ash/dark, onyx/midnight) extend darker,
      // and many third-party themes only populate "dark". Apply fallback bases
      // first so the exact base wins on conflicts.
      let base_chain: &[ThemeBase] = match base {
         ThemeBase::Dark => &[ThemeBase::Darker, ThemeBase::Dark],
         ThemeBase::Darker => &[ThemeBase::Dark, ThemeBase::Darker],
         ThemeBase::Midnight => &[ThemeBase::Dark, ThemeBase::Darker, ThemeBase::Midnight],
         ThemeBase::Light => &[ThemeBase::Light],
      };

      for candidate in base_chain {
         let Some(entries) = selection
            .values
            .semantic
            .iter()
            .find(|(key, _)| ThemeBase::parse(key) == Some(*candidate))
            .map(|(_, entries)| entries)
         else {
            continue;
         };
         for (name, hex) in entries {
            let Some(replacement) = parse_color(hex) else {
               continue;
            };
            let Some((category, default_argb)) = table.semantic_argb(name, base) else {
               continue;
            };
            palette
               .semantic_by_category
               .insert((category, default_argb), replacement);
            if category == Category::Generic {
               palette.semantic_any.insert(default_argb, replacement);
            } else {
               palette
                  .semantic_any
                  .entry(default_argb)
                  .or_insert(replacement);
            }
         }
      }

      palette
   }

   #[must_use]
   pub fn is_empty(&self) -> bool {
      self.semantic_by_category.is_empty()
         && self.semantic_any.is_empty()
         && self.raw_by_rgb.is_empty()
   }

   /// Remaps a Phoenix 32-bit ARGB color in `category`, returning `None` when
   /// the active theme does not override it.
   #[must_use]
   pub fn remap(&self, argb: u32, category: Category) -> Option<u32> {
      if let Some(&color) = self.semantic_by_category.get(&(category, argb)) {
         return Some(color);
      }
      if let Some(&color) = self.semantic_any.get(&argb) {
         return Some(color);
      }
      let &raw = self.raw_by_rgb.get(&(argb & 0x00FF_FFFF))?;
      let orig_a = (argb >> 24) & 0xFF;
      let raw_a = (raw >> 24) & 0xFF;
      let combined_a = (orig_a * raw_a + 127) / 255;
      Some((combined_a << 24) | (raw & 0x00FF_FFFF))
   }
}

/// Infers Phoenix's active [`ThemeBase`] from a root/surface background ARGB
/// when `AppState.setTheme` has not yet been observed.
#[must_use]
pub fn infer_base_from_background(argb: u32) -> Option<ThemeBase> {
   let table = TokenTable::global();
   for token in [
      "BACKGROUND_BASE_LOW",
      "BACKGROUND_BASE_LOWER",
      "BACKGROUND_BASE_LOWEST",
   ] {
      for base in [
         ThemeBase::Dark,
         ThemeBase::Darker,
         ThemeBase::Light,
         ThemeBase::Midnight,
      ] {
         if let Some((_, expected)) = table.semantic_argb(token, base)
            && expected == argb
         {
            // Light and Onyx share some values across tokens, so only treat
            // unambiguous matches as authoritative.
            let unique = [
               ThemeBase::Dark,
               ThemeBase::Darker,
               ThemeBase::Light,
               ThemeBase::Midnight,
            ]
            .into_iter()
            .filter(|other| {
               table
                  .semantic_argb(token, *other)
                  .is_some_and(|(_, value)| value == argb)
            })
            .count()
               == 1;
            if unique {
               return Some(base);
            }
         }
      }
   }
   None
}
