//! Emoji asset codepoint extraction and fallback file resolution, ported from
//! Valenced's `ThemesEmoji.mm`.

use std::path::{Path, PathBuf};

use crate::themes::state::EmojiSelection;

const EMOJI_PREFIX: &str = "emoji-";
const EMOJI_SUFFIX: &str = ".png";

/// Extracts lowercase hex codepoints from an asset name like
/// `emoji://1f600`, `emoji-1f600.png`, or `emoji-1f44d-1f3fd@2x.png`.
#[must_use]
pub fn codepoints_from_asset(asset_name: &str) -> Option<String> {
   let clean = asset_name.split(['?', '#']).next().unwrap_or(asset_name);
   let codepoints = if let Some(rest) = clean.strip_prefix("emoji://") {
      rest.trim_end_matches('/')
   } else {
      let file = Path::new(clean).file_name()?.to_str()?;
      let stem = file.strip_suffix(EMOJI_SUFFIX).unwrap_or(file);
      let normalized = stem.rsplit_once('@').map_or(stem, |(base, _)| base);
      normalized.strip_prefix(EMOJI_PREFIX)?
   };
   if codepoints.is_empty() {
      return None;
   }
   let valid = codepoints.split('-').all(|part| {
      !part.is_empty()
         && part
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
   });
   valid.then(|| codepoints.to_owned())
}

/// Converts a Unicode emoji sequence into dash-separated lowercase hex
/// codepoints.
#[must_use]
pub fn codepoints_from_text(text: &str) -> Option<String> {
   if text.is_empty() {
      return None;
   }
   let parts = text
      .chars()
      .map(|ch| format!("{:x}", u32::from(ch)))
      .collect::<Vec<_>>();
   Some(parts.join("-"))
}

/// Candidate filenames for `codepoints`, ordered from most specific to most
/// general: exact, without `fe0f` variation selectors, without Fitzpatrick skin
/// tone modifiers (`1f3fb`..=`1f3ff`), and without both.
#[must_use]
pub fn candidate_image_names(codepoints: &str) -> Vec<String> {
   let parts = codepoints.split('-').collect::<Vec<_>>();
   let mut variants = Vec::new();
   let mut push = |slice: Vec<&str>| {
      if slice.is_empty() {
         return;
      }
      let joined = slice.join("-");
      if !variants.contains(&joined) {
         variants.push(joined);
      }
   };

   push(parts.clone());
   push(
      parts
         .iter()
         .copied()
         .filter(|part| *part != "fe0f")
         .collect(),
   );
   let no_tone = parts
      .iter()
      .copied()
      .filter(|part| {
         u32::from_str_radix(part, 16).is_ok_and(|value| !(0x1F3FB..=0x1F3FF).contains(&value))
      })
      .collect::<Vec<_>>();
   push(no_tone.clone());
   push(no_tone.into_iter().filter(|part| *part != "fe0f").collect());

   variants
      .into_iter()
      .map(|variant| format!("{EMOJI_PREFIX}{variant}{EMOJI_SUFFIX}"))
      .collect()
}

/// Resolves an emoji asset name against `selection.images`, returning the first
/// existing file path in the fallback chain.
#[must_use]
pub fn resolve_asset(selection: &EmojiSelection, asset_name: &str) -> Option<PathBuf> {
   let images_dir = selection.images.as_deref()?;
   let codepoints = codepoints_from_asset(asset_name)?;
   for name in candidate_image_names(&codepoints) {
      let path = Path::new(images_dir).join(name);
      if path.is_file() {
         return Some(path);
      }
   }
   None
}
