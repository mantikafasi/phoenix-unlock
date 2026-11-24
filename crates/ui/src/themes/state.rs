//! Theme selection state, matching Valenced's `state.json` schema (`format: 1`),
//! plus a parser for standalone `.json` theme files.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const STATE_FORMAT: i32 = 1;
pub const SYSTEM_EMOJI_FONT: &str = "system";

/// Discord's four base themes. Phoenix and Discord's token tables name Ash
/// `"dark"`, Dark `"darker"`, Light `"light"`, and Onyx `"midnight"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeBase {
   Dark,
   #[default]
   Darker,
   Light,
   Midnight,
}

impl ThemeBase {
   #[must_use]
   pub const fn as_str(self) -> &'static str {
      match self {
         Self::Dark => "dark",
         Self::Darker => "darker",
         Self::Light => "light",
         Self::Midnight => "midnight",
      }
   }

   /// Index into the four-element theme arrays in `tokens.json` (`dark`,
   /// `darker`, `light`, `midnight`).
   #[must_use]
   pub const fn token_index(self) -> usize {
      match self {
         Self::Dark => 0,
         Self::Darker => 1,
         Self::Light => 2,
         Self::Midnight => 3,
      }
   }

   /// Accepts both Discord's internal names (`dark`/`darker`/`light`/`midnight`)
   /// and the user-facing palette names (`ash`/`onyx`).
   #[must_use]
   pub fn parse(name: &str) -> Option<Self> {
      match name.trim().to_ascii_lowercase().as_str() {
         "dark" | "ash" => Some(Self::Dark),
         "darker" => Some(Self::Darker),
         "light" => Some(Self::Light),
         "midnight" | "onyx" | "amoled" => Some(Self::Midnight),
         _ => None,
      }
   }

   /// Maps Discord's numeric theme index (`0` = dark/ash, `1` = light,
   /// `2` = midnight/onyx, `3` = darker).
   #[must_use]
   pub const fn from_index(index: i32) -> Self {
      match index {
         1 => Self::Light,
         2 => Self::Midnight,
         3 => Self::Darker,
         _ => Self::Dark,
      }
   }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorValues {
   #[serde(default)]
   pub semantic: BTreeMap<String, BTreeMap<String, String>>,
   #[serde(default)]
   pub raw: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorsSelection {
   #[serde(default)]
   pub plugin: String,
   #[serde(default)]
   pub group: String,
   #[serde(default)]
   pub theme: String,
   #[serde(default)]
   pub values: ColorValues,
}

const fn default_font_weight() -> i32 {
   400
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontVariant {
   #[serde(default)]
   pub path: String,
   #[serde(default = "default_font_weight")]
   pub weight: i32,
   #[serde(default)]
   pub italic: bool,
   #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
   pub axes: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FontSource {
   #[serde(default)]
   pub plugin: String,
   #[serde(default)]
   pub group: String,
   #[serde(default)]
   pub pack: String,
   #[serde(default)]
   pub variants: Vec<FontVariant>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FontsSelection {
   #[serde(default)]
   pub families: BTreeMap<String, FontSource>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconsSelection {
   #[serde(default)]
   pub plugin: String,
   #[serde(default)]
   pub group: String,
   #[serde(default)]
   pub pack: String,
   #[serde(default)]
   pub icons: BTreeMap<String, String>,
   #[serde(default)]
   pub drawables: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiSelection {
   #[serde(default)]
   pub plugin: String,
   #[serde(default)]
   pub group: String,
   #[serde(default)]
   pub pack: String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub images: Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub font: Option<String>,
}

const fn default_format() -> i32 {
   STATE_FORMAT
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemesState {
   #[serde(default = "default_format")]
   pub format: i32,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub colors: Option<ColorsSelection>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub fonts: Option<FontsSelection>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub icons: Option<IconsSelection>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub emoji: Option<EmojiSelection>,
}

impl Default for ThemesState {
   fn default() -> Self {
      Self {
         format: STATE_FORMAT,
         colors: None,
         fonts: None,
         icons: None,
         emoji: None,
      }
   }
}

/// Metadata and color overrides parsed from a standalone `.json` theme file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTheme {
   pub name: String,
   pub description: Option<String>,
   pub authors: Vec<String>,
   pub colors: ColorsSelection,
}

#[derive(Deserialize)]
struct ThemeAuthor {
   name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SemanticEntry {
   /// Vendetta/Bunny/Revenge tuple: `[dark, light]` or
   /// `[dark, light, darker, midnight]`.
   Tuple(Vec<Option<String>>),
   /// Valenced/object form: `{ dark, darker, light, midnight }`.
   ByBase(BTreeMap<String, Option<String>>),
   /// Single color applied to every base theme.
   Single(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StandaloneThemeJson {
   #[serde(default)]
   name: Option<String>,
   #[serde(default)]
   description: Option<String>,
   #[serde(default)]
   authors: Vec<ThemeAuthor>,
   #[serde(default)]
   semantic_colors: BTreeMap<String, SemanticEntry>,
   #[serde(default)]
   raw_colors: BTreeMap<String, String>,
   /// Valenced `ColorsSelection` fields, accepted directly when present.
   #[serde(default)]
   theme: Option<String>,
   #[serde(default)]
   values: Option<ColorValues>,
}

/// Parses either a Valenced `ColorsSelection` JSON or a Vendetta/Bunny/Revenge
/// theme JSON into a [`ParsedTheme`].
pub fn parse_theme_json(fallback_name: &str, json: &str) -> Result<ParsedTheme, serde_json::Error> {
   let raw: StandaloneThemeJson = serde_json::from_str(json)?;
   let name = raw
      .name
      .or(raw.theme)
      .filter(|value| !value.trim().is_empty())
      .unwrap_or_else(|| fallback_name.to_owned());
   let description = raw.description.filter(|value| !value.trim().is_empty());
   let authors = raw
      .authors
      .into_iter()
      .map(|author| author.name)
      .filter(|author| !author.trim().is_empty())
      .collect();

   let values = if let Some(values) = raw.values {
      values
   } else {
      let mut semantic: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
      for (token, entry) in raw.semantic_colors {
         match entry {
            SemanticEntry::Tuple(items) => {
               // Spec 2/3 orders tuple slots as [dark, light, darker, midnight].
               let slots = [
                  (0, ThemeBase::Dark),
                  (1, ThemeBase::Light),
                  (2, ThemeBase::Darker),
                  (3, ThemeBase::Midnight),
               ];
               for (index, base) in slots {
                  if let Some(Some(color)) = items.get(index)
                     && !color.is_empty()
                  {
                     semantic
                        .entry(base.as_str().to_owned())
                        .or_default()
                        .insert(token.clone(), color.clone());
                  }
               }
            },
            SemanticEntry::ByBase(map) => {
               for (base_key, color) in map {
                  let Some(color) = color.filter(|value| !value.is_empty()) else {
                     continue;
                  };
                  let base = ThemeBase::parse(&base_key)
                     .map_or(base_key, |parsed| parsed.as_str().to_owned());
                  semantic
                     .entry(base)
                     .or_default()
                     .insert(token.clone(), color);
               }
            },
            SemanticEntry::Single(color) if !color.is_empty() => {
               for base in [
                  ThemeBase::Dark,
                  ThemeBase::Darker,
                  ThemeBase::Light,
                  ThemeBase::Midnight,
               ] {
                  semantic
                     .entry(base.as_str().to_owned())
                     .or_default()
                     .insert(token.clone(), color.clone());
               }
            },
            SemanticEntry::Single(_) => {},
         }
      }
      ColorValues {
         semantic,
         raw: raw.raw_colors,
      }
   };

   Ok(ParsedTheme {
      description,
      authors,
      colors: ColorsSelection {
         plugin: String::new(),
         group: String::new(),
         theme: name.clone(),
         values,
      },
      name,
   })
}
