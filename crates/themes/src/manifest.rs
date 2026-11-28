//! Optional JSON theme manifest format convertible to [`ThemeSpec`].

use std::collections::BTreeMap;

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   css::{
      ThemeMeta,
      ThemeMode,
   },
   spec::ThemeSpec,
};

/// JSON representation of a Sunbird theme (`.theme.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeManifest {
   #[serde(default)]
   pub name:         String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub description:  Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub author:       Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub version:      Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub source:       Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub mode:         Option<ThemeMode>,
   /// Base variables applied in both dark and light modes (with or without
   /// leading `--`).
   #[serde(default)]
   pub tokens:       BTreeMap<String, String>,
   #[serde(default)]
   pub dark_tokens:  BTreeMap<String, String>,
   #[serde(default)]
   pub light_tokens: BTreeMap<String, String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub radius_scale: Option<f32>,
}

fn normalize_map(input: BTreeMap<String, String>) -> BTreeMap<String, String> {
   input
      .into_iter()
      .map(|(k, v)| {
         let lower = k.trim().to_ascii_lowercase();
         let key = if lower.starts_with("--") {
            lower
         } else {
            format!("--{lower}")
         };
         (key, v.trim().to_owned())
      })
      .collect()
}

impl From<ThemeManifest> for ThemeSpec {
   fn from(manifest: ThemeManifest) -> Self {
      Self {
         meta:         ThemeMeta {
            name:        manifest.name,
            description: manifest.description,
            author:      manifest.author,
            version:     manifest.version,
            source:      manifest.source,
            mode:        manifest.mode,
         },
         root:         normalize_map(manifest.tokens),
         dark:         normalize_map(manifest.dark_tokens),
         light:        normalize_map(manifest.light_tokens),
         radius_scale: manifest.radius_scale.filter(|v| (0.0..=2.5).contains(v)),
      }
   }
}
