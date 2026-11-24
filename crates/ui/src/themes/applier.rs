//! Rewrites Phoenix UI-op batches to apply custom colors, fonts, icons and
//! emoji, and tracks original props so live views update immediately when the
//! theme changes or is cleared.

use std::collections::HashMap;

use phoenix_wire::{Key, Value};

use crate::{
   Op, Prop, Props, ViewKind, get_prop, prop, set_prop,
   themes::{
      color::{Category, ColorPalette, infer_base_from_background},
      emoji,
      fonts::{DEFAULT_FONT_WEIGHT, resolve_font_path},
      state::{
         ColorsSelection, EmojiSelection, FontsSelection, IconsSelection, ThemeBase, ThemesState,
      },
   },
};

const ACTIVE_BACKGROUND: Prop = prop!("activeBackground");
const LOADING_BACKGROUND: Prop = prop!("loadingBackground");
const SCRIM_COLOR: Prop = prop!("scrimColor");
const NAV_SCRIM_COLOR: Prop = prop!("navScrimColor");
const BG: Prop = prop!("bg");
const PLACEHOLDER_COLOR: Prop = prop!("placeholderColor");
const SELECTION_COLOR: Prop = prop!("selectionColor");
const BORDER_COLOR: Prop = prop!("borderColor");
const ACTIVE_BORDER_COLOR: Prop = prop!("activeBorderColor");
const FOCUS_BORDER_COLOR: Prop = prop!("focusBorderColor");
const BG_BORDER: Prop = prop!("bgBorder");
const BORDER_LEFT: Prop = prop!("borderLeft");
const ACTIVE_BORDER_LEFT: Prop = prop!("activeBorderLeft");
const TINT: Prop = prop!("tint");
const HANDLE_COLOR: Prop = prop!("handleColor");
const MIN_TRACK_TINT: Prop = prop!("minTrackTint");
const MAX_TRACK_TINT: Prop = prop!("maxTrackTint");
const THUMB_TINT: Prop = prop!("thumbTint");
const ELEVATION_COLOR: Prop = prop!("elevationColor");
const SPANS: Prop = prop!("spans");
const VALUE_SPANS: Prop = prop!("valueSpans");
const BACKGROUND_GRADIENT: Prop = prop!("backgroundGradient");
const ACTIVE_BACKGROUND_GRADIENT: Prop = prop!("activeBackgroundGradient");
const TEXT_GRADIENT: Prop = prop!("textGradient");
const MASK_GRADIENT: Prop = prop!("maskGradient");
const COLORS: Prop = prop!("colors");
const SHADOW: Prop = prop!("shadow");
const ACTIVE_SHADOW: Prop = prop!("activeShadow");
const TEXT_SHADOW: Prop = prop!("textShadow");
const BG_SHADOW: Prop = prop!("bgShadow");
const SWIPE_SHADOW: Prop = prop!("swipeShadow");
const FONT_WEIGHT: Prop = prop!("fontWeight");
const ITALIC: Prop = prop!("italic");
const IMAGE: Prop = prop!("image");
const ICON: Prop = prop!("icon");
const LEADING_ICON: Prop = prop!("leadingIcon");

/// Renders `(codepoints, font)` into a local PNG path for font-based emoji
/// packs.
pub type EmojiRenderer<'a> = &'a dyn Fn(&str, &str) -> Option<String>;

/// Live theme engine that rewrites UI-op batches and tracks original props for
/// instant re-theming.
pub struct ThemeApplier {
   base: ThemeBase,
   base_explicit: bool,
   state: ThemesState,
   palette: ColorPalette,
   /// Original un-themed themeable props per live node id.
   originals: HashMap<i32, NodeOriginals>,
}

struct NodeOriginals {
   kind: ViewKind,
   props: Props,
}

impl Default for ThemeApplier {
   fn default() -> Self {
      Self::new()
   }
}

impl ThemeApplier {
   #[must_use]
   pub fn new() -> Self {
      Self {
         base: ThemeBase::default(),
         base_explicit: false,
         state: ThemesState::default(),
         palette: ColorPalette::default(),
         originals: HashMap::new(),
      }
   }

   #[must_use]
   pub const fn base(&self) -> ThemeBase {
      self.base
   }

   #[must_use]
   pub const fn state(&self) -> &ThemesState {
      &self.state
   }

   /// True when any color, font, icon, or emoji override is active.
   #[must_use]
   pub fn is_active(&self) -> bool {
      !self.palette.is_empty()
         || self
            .state
            .fonts
            .as_ref()
            .is_some_and(|fonts| !fonts.families.is_empty())
         || self
            .state
            .icons
            .as_ref()
            .is_some_and(|icons| !icons.icons.is_empty() || !icons.drawables.is_empty())
         || self
            .state
            .emoji
            .as_ref()
            .is_some_and(|emoji| emoji.images.is_some() || emoji.font.is_some())
   }

   /// Updates the active base theme (e.g. from `AppState.setTheme`), rebuilding
   /// the color palette if needed. Returns `true` if the base changed.
   pub fn set_base(&mut self, base: ThemeBase) -> bool {
      self.base_explicit = true;
      if self.base == base {
         return false;
      }
      self.base = base;
      self.rebuild_palette();
      true
   }

   pub fn set_state(&mut self, state: ThemesState) {
      self.state = state;
      self.rebuild_palette();
   }

   pub fn set_colors(&mut self, colors: Option<ColorsSelection>) {
      self.state.colors = colors;
      self.rebuild_palette();
   }

   pub fn set_fonts(&mut self, fonts: Option<FontsSelection>) {
      self.state.fonts = fonts;
   }

   pub fn set_icons(&mut self, icons: Option<IconsSelection>) {
      self.state.icons = icons;
   }

   pub fn set_emoji(&mut self, emoji: Option<EmojiSelection>) {
      self.state.emoji = emoji;
   }

   fn rebuild_palette(&mut self) {
      self.palette = self
         .state
         .colors
         .as_ref()
         .map_or_else(ColorPalette::default, |colors| {
            ColorPalette::build(colors, self.base)
         });
   }

   /// Records original themeable props from `ops` and rewrites `ops` in place
   /// when overrides are active. Returns `true` if any op was modified.
   pub fn rewrite_batch(&mut self, ops: &mut [Op]) -> bool {
      self.rewrite_batch_with_emoji(ops, None)
   }

   /// Like [`Self::rewrite_batch`], with an optional callback that renders
   /// `(codepoints, font)` into a local PNG path for font-based emoji packs.
   pub fn rewrite_batch_with_emoji(
      &mut self,
      ops: &mut [Op],
      draw_emoji: Option<EmojiRenderer<'_>>,
   ) -> bool {
      if !self.base_explicit
         && let Some(inferred) = detect_base(ops)
         && inferred != self.base
      {
         self.base = inferred;
         self.rebuild_palette();
      }

      self.track_originals(ops);

      if !self.is_active() {
         return false;
      }

      let mut changed = false;
      for op in ops {
         match op {
            Op::Create { kind, props, .. } => {
               changed |= self.apply_to_props(Some(kind), props, draw_emoji);
            },
            Op::Update { id, props } => {
               let kind = self.originals.get(id).map(|entry| entry.kind.clone());
               changed |= self.apply_to_props(kind.as_ref(), props, draw_emoji);
            },
            _ => {},
         }
      }
      changed
   }

   /// Builds `Op::Update` ops to apply the current theme (or restore original
   /// props when cleared) across all tracked live nodes.
   #[must_use]
   pub fn refresh_ops(&self) -> Vec<Op> {
      self.refresh_ops_with_emoji(None)
   }

   /// Like [`Self::refresh_ops`], with an optional font-based emoji renderer.
   #[must_use]
   pub fn refresh_ops_with_emoji(
      &self,
      draw_emoji: Option<EmojiRenderer<'_>>,
   ) -> Vec<Op> {
      let mut ops = Vec::with_capacity(self.originals.len());
      let active = self.is_active();
      for (&id, entry) in &self.originals {
         let is_text_like = matches!(entry.kind, ViewKind::Text | ViewKind::Input);
         if entry.props.is_empty() && !is_text_like {
            continue;
         }
         let mut props = entry.props.clone();
         if active {
            let _ = self.apply_to_props(Some(&entry.kind), &mut props, draw_emoji);
         }
         // Omitting `fontFamily` in `Op::Update` leaves a previously injected
         // custom font on the Android view; send `Null` to restore Phoenix's
         // default typeface when no override applies.
         if is_text_like && get_prop(&props, Prop::FONT_FAMILY).is_none() {
            props.push((Prop::FONT_FAMILY, Value::Null));
         }
         if !props.is_empty() {
            ops.push(Op::Update { id, props });
         }
      }
      ops
   }

   fn track_originals(&mut self, ops: &[Op]) {
      for op in ops {
         match op {
            Op::Create { id, kind, props } => {
               let kept = props
                  .iter()
                  .filter(|(prop, _)| is_themeable_prop(*prop))
                  .cloned()
                  .collect::<Props>();
               if !kept.is_empty() || matches!(kind, ViewKind::Text | ViewKind::Input) {
                  self.originals.insert(
                     *id,
                     NodeOriginals {
                        kind: kind.clone(),
                        props: kept,
                     },
                  );
               }
            },
            Op::Update { id, props } => {
               if let Some(entry) = self.originals.get_mut(id) {
                  for (prop, value) in props {
                     if is_themeable_prop(*prop) {
                        set_prop(&mut entry.props, *prop, Some(value.clone()));
                     }
                  }
               }
            },
            Op::Delete { ids } => {
               for id in ids {
                  self.originals.remove(id);
               }
            },
            _ => {},
         }
      }
   }

   fn apply_to_props(
      &self,
      kind: Option<&ViewKind>,
      props: &mut Props,
      draw_emoji: Option<EmojiRenderer<'_>>,
   ) -> bool {
      let mut changed = false;

      if !self.palette.is_empty() {
         for (prop, value) in props.iter_mut() {
            if let Some(category) = color_prop_category(*prop) {
               changed |= remap_color_value(&self.palette, value, category);
            } else if is_gradient_prop(*prop) {
               changed |= remap_gradient(&self.palette, value);
            } else if is_shadow_prop(*prop) {
               changed |= remap_shadow(&self.palette, value);
            }
         }
      }

      if !self.palette.is_empty() || self.state.icons.is_some() || self.state.emoji.is_some() {
         for (prop, value) in props.iter_mut() {
            if *prop == SPANS || *prop == VALUE_SPANS {
               changed |= self.remap_spans(value, draw_emoji);
            }
         }
      }

      if self.state.icons.is_some() || self.state.emoji.is_some() {
         for (prop, value) in props.iter_mut() {
            if !is_uri_prop(*prop) {
               continue;
            }
            let Value::Str(uri) = value else {
               continue;
            };
            if let Some(replacement) = self.resolve_uri(uri, draw_emoji)
               && replacement != *uri
            {
               *uri = replacement;
               changed = true;
            }
         }
      }

      if let Some(fonts) = &self.state.fonts
         && !fonts.families.is_empty()
      {
         let weight = get_prop(props, FONT_WEIGHT)
            .and_then(Value::as_i32)
            .unwrap_or(DEFAULT_FONT_WEIGHT);
         let italic =
            get_prop(props, ITALIC).is_some_and(|value| matches!(value, Value::Bool(true)));
         let current_family = get_prop(props, Prop::FONT_FAMILY).and_then(Value::as_str);
         let is_text_like = matches!(kind, Some(ViewKind::Text | ViewKind::Input));
         if (current_family.is_some() || is_text_like)
            && let Some(path) = resolve_font_path(fonts, current_family, weight, italic)
            && current_family != Some(path)
         {
            set_prop(props, Prop::FONT_FAMILY, Some(Value::Str(path.to_owned())));
            changed = true;
         }
      }

      changed
   }

   fn remap_spans(
      &self,
      value: &mut Value,
      draw_emoji: Option<EmojiRenderer<'_>>,
   ) -> bool {
      let Value::Arr(spans) = value else {
         return false;
      };
      let color_key = Prop::COLOR.key();
      let bg_key = BG.key();
      let bg_border_key = BG_BORDER.key();
      let uri_key = Prop::URI.key();
      let image_key = IMAGE.key();
      let icon_key = ICON.key();
      let remap_colors = !self.palette.is_empty();
      let remap_uris = self.state.icons.is_some() || self.state.emoji.is_some();
      let mut changed = false;
      for span in spans {
         let Value::Obj(entries) = span else {
            continue;
         };
         for (key, field_val) in entries {
            if remap_colors {
               if *key == color_key {
                  changed |= remap_color_value(&self.palette, field_val, Category::Text);
                  continue;
               }
               if *key == bg_key {
                  changed |= remap_color_value(&self.palette, field_val, Category::Background);
                  continue;
               }
               if *key == bg_border_key {
                  changed |= remap_color_value(&self.palette, field_val, Category::Border);
                  continue;
               }
            }
            if remap_uris && (*key == uri_key || *key == image_key || *key == icon_key) {
               match field_val {
                  Value::Str(uri) => {
                     if let Some(replacement) = self.resolve_uri(uri, draw_emoji)
                        && replacement != *uri
                     {
                        *uri = replacement;
                        changed = true;
                     }
                  },
                  Value::Obj(nested) if *key == image_key => {
                     for (nested_key, nested_val) in nested {
                        if *nested_key == uri_key
                           && let Value::Str(uri) = nested_val
                           && let Some(replacement) = self.resolve_uri(uri, draw_emoji)
                           && replacement != *uri
                        {
                           *uri = replacement;
                           changed = true;
                        }
                     }
                  },
                  _ => {},
               }
            }
         }
      }
      changed
   }

   fn resolve_uri(
      &self,
      uri: &str,
      draw_emoji: Option<EmojiRenderer<'_>>,
   ) -> Option<String> {
      if let Some(icons) = &self.state.icons
         && let Some(res_name) = uri.strip_prefix("res://")
      {
         let short = res_name.strip_prefix("ic_mana_").unwrap_or(res_name);
         let pascal_icon = snake_to_pascal_icon(short);
         let replacement = icons
            .icons
            .get(res_name)
            .or_else(|| icons.icons.get(short))
            .or_else(|| icons.icons.get(&pascal_icon))
            .or_else(|| icons.drawables.get(res_name))
            .or_else(|| icons.drawables.get(short))
            .or_else(|| icons.drawables.get(&pascal_icon));
         if let Some(path) = replacement
            && !path.is_empty()
         {
            return Some(as_file_uri(path));
         }
      }

      if let Some(emoji_sel) = &self.state.emoji {
         if let Some(path) = emoji::resolve_asset(emoji_sel, uri) {
            return Some(as_file_uri(&path.to_string_lossy()));
         }
         if let (Some(font), Some(render), Some(codepoints)) = (
            emoji_sel.font.as_deref(),
            draw_emoji,
            emoji::codepoints_from_asset(uri),
         ) && let Some(path) = render(&codepoints, font)
         {
            return Some(as_file_uri(&path));
         }
      }

      None
   }
}

/// Converts a Phoenix `ic_mana_*` stem like `arrow_large_left` to Valenced's
/// React Native icon name `ArrowLargeLeftIcon`.
fn snake_to_pascal_icon(snake: &str) -> String {
   let mut out = String::with_capacity(snake.len() + 4);
   for part in snake.split('_').filter(|part| !part.is_empty()) {
      let mut chars = part.chars();
      if let Some(first) = chars.next() {
         for upper in first.to_uppercase() {
            out.push(upper);
         }
         out.push_str(chars.as_str());
      }
   }
   out.push_str("Icon");
   out
}

fn as_file_uri(path: &str) -> String {
   if path.starts_with("data:")
      || path.starts_with("file://")
      || path.starts_with("https://")
      || path.starts_with("http://")
      || path.starts_with("res://")
   {
      path.to_owned()
   } else {
      format!("file://{path}")
   }
}

fn detect_base(ops: &[Op]) -> Option<ThemeBase> {
   for op in ops {
      let Op::Create { props, .. } = op else {
         continue;
      };
      let Some(bg) = get_prop(props, Prop::BACKGROUND).and_then(value_as_argb) else {
         continue;
      };
      if let Some(base) = infer_base_from_background(bg) {
         return Some(base);
      }
   }
   None
}

fn is_themeable_prop(prop: Prop) -> bool {
   color_prop_category(prop).is_some()
      || prop == SPANS
      || prop == VALUE_SPANS
      || is_gradient_prop(prop)
      || is_shadow_prop(prop)
      || is_uri_prop(prop)
      || prop == Prop::FONT_FAMILY
      || prop == FONT_WEIGHT
      || prop == ITALIC
}

const fn is_uri_prop(prop: Prop) -> bool {
   matches!(prop, Prop::URI | IMAGE | ICON | LEADING_ICON)
}

const fn color_prop_category(prop: Prop) -> Option<Category> {
   match prop {
      Prop::BACKGROUND
      | ACTIVE_BACKGROUND
      | LOADING_BACKGROUND
      | SCRIM_COLOR
      | NAV_SCRIM_COLOR
      | BG => Some(Category::Background),
      Prop::COLOR | PLACEHOLDER_COLOR | SELECTION_COLOR => Some(Category::Text),
      BORDER_COLOR | ACTIVE_BORDER_COLOR | FOCUS_BORDER_COLOR | BG_BORDER | BORDER_LEFT
      | ACTIVE_BORDER_LEFT => Some(Category::Border),
      TINT | HANDLE_COLOR | MIN_TRACK_TINT | MAX_TRACK_TINT | THUMB_TINT | ELEVATION_COLOR => {
         Some(Category::Generic)
      },
      _ => None,
   }
}

const fn is_gradient_prop(prop: Prop) -> bool {
   matches!(
      prop,
      BACKGROUND_GRADIENT | ACTIVE_BACKGROUND_GRADIENT | TEXT_GRADIENT | MASK_GRADIENT | COLORS
   )
}

const fn is_shadow_prop(prop: Prop) -> bool {
   matches!(
      prop,
      SHADOW | ACTIVE_SHADOW | TEXT_SHADOW | BG_SHADOW | SWIPE_SHADOW
   )
}

fn value_as_argb(value: &Value) -> Option<u32> {
   match *value {
      Value::I64(n) => u32::try_from(n).ok().or_else(|| {
         // Signed 32-bit ARGB values sign-extended to i64 (e.g. -1 -> 0xFFFFFFFF).
         i32::try_from(n).ok().map(i32::cast_unsigned)
      }),
      Value::I32(n) => Some(n.cast_unsigned()),
      _ => None,
   }
}

fn write_argb(value: &mut Value, argb: u32) {
   match value {
      Value::I64(slot) => *slot = i64::from(argb),
      Value::I32(slot) => *slot = argb.cast_signed(),
      _ => {},
   }
}

fn remap_color_value(palette: &ColorPalette, value: &mut Value, category: Category) -> bool {
   let Some(argb) = value_as_argb(value) else {
      return false;
   };
   let Some(next) = palette.remap(argb, category) else {
      return false;
   };
   if next == argb {
      return false;
   }
   write_argb(value, next);
   true
}

fn remap_gradient(palette: &ColorPalette, value: &mut Value) -> bool {
   let Value::Arr(items) = value else {
      return false;
   };
   let color_key = Prop::COLOR.key();
   let mut changed = false;
   for item in items {
      match item {
         Value::I64(_) | Value::I32(_) => {
            changed |= remap_color_value(palette, item, Category::Background);
         },
         Value::Obj(entries) => {
            changed |= remap_obj_field(palette, entries, color_key, Category::Background);
         },
         _ => {},
      }
   }
   changed
}

fn remap_shadow(palette: &ColorPalette, value: &mut Value) -> bool {
   let Value::Obj(entries) = value else {
      return false;
   };
   remap_obj_field(palette, entries, Prop::COLOR.key(), Category::Generic)
}

fn remap_obj_field(
   palette: &ColorPalette,
   entries: &mut [(Key, Value)],
   target: Key,
   category: Category,
) -> bool {
   let mut changed = false;
   for (key, value) in entries {
      if *key == target {
         changed |= remap_color_value(palette, value, category);
      }
   }
   changed
}
