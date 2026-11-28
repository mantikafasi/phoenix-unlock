//! Light and dark colors for the settings pages.

use std::sync::atomic::{
   AtomicU8,
   Ordering,
};

use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   ViewKind,
};
use phoenix_wire::Value;

const DARK: u8 = 0;
const LIGHT: u8 = 1;
static THEME: AtomicU8 = AtomicU8::new(DARK);

/// Whether Phoenix's own settings text is light on dark or dark on light.
pub fn observe(ops: &[Op]) -> bool {
   let mut light_theme = 0;
   let mut dark_theme = 0;
   for op in ops {
      let props = match op {
         Op::Create { kind, props, .. } if *kind == ViewKind::Text => props,
         Op::Update { props, .. } => props,
         _ => continue,
      };
      let Some((_, value)) = props.iter().find(|(key, _)| *key == Prop::COLOR) else {
         continue;
      };
      let Some(color) = color_value(value) else {
         continue;
      };
      let luminance = luminance(color);
      if luminance < 0.35 {
         light_theme += 1;
      } else if luminance > 0.65 {
         dark_theme += 1;
      }
   }
   let current = THEME.load(Ordering::Relaxed);
   let next = if light_theme >= 3 && light_theme >= dark_theme + 2 {
      LIGHT
   } else if dark_theme >= 3 && dark_theme >= light_theme + 2 {
      DARK
   } else {
      current
   };
   let changed = next != current;
   THEME.store(next, Ordering::Relaxed);
   changed
}

fn color_value(value: &Value) -> Option<i64> {
   match value {
      Value::I32(color) => Some(i64::from(*color)),
      Value::I64(color) => Some(*color),
      _ => None,
   }
}

/// ARGB colors are 32-bit bit patterns, even when the wire carries them as
/// signed integers.
#[expect(
   clippy::cast_possible_truncation,
   clippy::cast_sign_loss,
   reason = "ARGB colors are 32-bit bit patterns"
)]
const fn argb_bits(color: i64) -> u32 {
   color as u32
}

fn luminance(color: i64) -> f64 {
   let color = argb_bits(color);
   let red = f64::from((color >> 16) & 0xFF);
   let green = f64::from((color >> 8) & 0xFF);
   let blue = f64::from(color & 0xFF);
   0.0722f64.mul_add(blue, 0.7152f64.mul_add(green, 0.2126 * red)) / 255.0
}

pub fn light() -> bool {
   THEME.load(Ordering::Relaxed) == LIGHT
}

/// Opaque surface for code and inset content.
pub fn surface() -> i64 {
   if light() { 0xFFF2_F3F5 } else { 0xFF14_1518 }
}

/// Raised card surface.
pub fn raised() -> i64 {
   if light() { 0xFFFF_FFFF } else { 0xFF23_2428 }
}

/// Pressed/hover surface on raised content.
pub fn active() -> i64 {
   if light() { 0xFFE3_E5E8 } else { 0xFF2B_2D31 }
}

/// Hairline around raised content.
pub fn border() -> i64 {
   if light() { 0x1F00_0000 } else { 0x26FF_FFFF }
}

/// Translucent press wash with the theme's contrast color at `alpha`.
pub fn overlay(alpha: i64) -> i64 {
   let rgb = if light() { 0x0000_0000 } else { 0x00FF_FFFF };
   (alpha << 24) | rgb
}

/// The text colors Sunbird draws its pages with.
#[derive(Clone, Copy)]
pub enum Color {
   Text,
   Muted,
   Warn,
   Error,
   Accent,
   Tag,
   String,
   Keyword,
   Number,
   Comment,
}

impl Color {
   /// ARGB for the theme Phoenix shows now.
   pub fn argb(self) -> i64 {
      let (on_dark, on_light) = match self {
         Self::Text => (0xFFF6_F6F7, 0xFF1E_1F22),
         Self::Muted => (0xFF94_9BA4, 0xFF5E_6168),
         Self::Warn => (0xFFF0_B232, 0xFF9A_6800),
         Self::Error => (0xFFF2_3F42, 0xFFD0_2438),
         Self::Accent | Self::Number => (0xFF79_C0FF, 0xFF00_66D9),
         Self::Tag => (0xFFD2_A8FF, 0xFF8A_3FFC),
         Self::String => (0xFF7E_E787, 0xFF1F_8F4D),
         Self::Keyword => (0xFFFF_7B72, 0xFFD0_2438),
         Self::Comment => (0xFF8B_949E, 0xFF6D_6F78),
      };
      if light() { on_light } else { on_dark }
   }
}

/// The captured layouts are dark-theme screenshots. On a light settings
/// screen, swap their surface and text colors for the matching light roles.
pub fn template_color(color: i64) -> i64 {
   if !light() {
      return color;
   }
   let mapped: u32 = match argb_bits(color) {
      0x1497_979F => 0x0F00_0000,
      0x12FF_FFFF => 0x1400_0000,
      0x3397_979F => 0x3300_0000,
      0x1F97_979F => 0x1F00_0000,
      0xFF24_2429 => 0xFFFF_FFFF,
      0xFF8F_9097 => 0xFF6D_6F78,
      0xFF96_979E | 0xFFAB_ACB2 => 0xFF5E_6168,
      0xFFEF_EFF1 | 0xFFFB_FBFB => 0xFF1E_1F22,
      _ => return color,
   };
   i64::from(mapped)
}

/// Applies [`template_color`] to every color-like prop in `props`.
pub fn template_props(props: &mut Props) {
   for (key, value) in props {
      let colorish = *key == Prop::COLOR
         || *key == Prop::TINT
         || *key == phoenix_ui_mobile::prop!("background")
         || *key == phoenix_ui_mobile::prop!("borderColor")
         || *key == phoenix_ui_mobile::prop!("placeholderColor");
      if !colorish {
         continue;
      }
      match value {
         Value::I32(color) => *color = argb_bits(template_color(i64::from(*color))).cast_signed(),
         Value::I64(color) => *color = template_color(*color),
         _ => {},
      }
   }
}
