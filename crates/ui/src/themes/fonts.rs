//! Font variant selection, ported from Valenced's `ThemesFontsHook.mm`.

use crate::themes::state::{FontVariant, FontsSelection};

pub const DEFAULT_FONT_WEIGHT: i32 = 400;

/// Picks the variant whose italic flag and weight best match `(weight, italic)`.
/// Matches Valenced's `VTHClosestVariantIndex` scoring.
#[must_use]
pub fn closest_variant(
   variants: &[FontVariant],
   weight: i32,
   italic: bool,
) -> Option<&FontVariant> {
   variants.iter().min_by_key(|variant| {
      let italic_penalty = if variant.italic == italic {
         0_i64
      } else {
         1_000_000
      };
      let weight_diff = (i64::from(variant.weight) - i64::from(weight)).abs() * 10;
      italic_penalty + weight_diff - i64::from(variant.weight)
   })
}

/// Resolves a Phoenix `fontFamily` name (or `"systemFont"` when unset) to a
/// custom font variant path.
#[must_use]
pub fn resolve_font_path<'a>(
   selection: &'a FontsSelection,
   family: Option<&str>,
   weight: i32,
   italic: bool,
) -> Option<&'a str> {
   let source = family
      .and_then(|name| selection.families.get(name))
      .or_else(|| selection.families.get("systemFont"))?;
   closest_variant(&source.variants, weight, italic)
      .map(|variant| variant.path.as_str())
      .filter(|path| !path.is_empty())
}
