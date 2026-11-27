//! Light and dark colors for the settings pages, in Discord's own palette once
//! it sends one.

use std::sync::{
   Mutex,
   PoisonError,
   atomic::{
      AtomicU8,
      Ordering,
   },
};

use phoenix_ui_mobile::{
   Op,
   Prop,
   Props,
   ViewKind,
};
use phoenix_wire::{
   HostCall,
   Value,
};

use crate::host::{
   self,
   Field,
   Method,
};

const DARK: u8 = 0;
const LIGHT: u8 = 1;
static THEME: AtomicU8 = AtomicU8::new(DARK);

/// Discord's colors, sent in `AppState.setTheme` at start and on every change.
/// A Nitro theme sends its base theme's.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Palette {
   dark:         bool,
   surface:      Option<u32>,
   text:         Option<u32>,
   text_default: Option<u32>,
   text_muted:   Option<u32>,
   ripple:       Option<u32>,
   danger:       Option<u32>,
}

static PALETTE: Mutex<Option<Palette>> = Mutex::new(None);
static DRAWN: Mutex<Option<Palette>> = Mutex::new(None);

fn palette() -> Option<Palette> {
   *PALETTE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a guest call starting with `head` is `AppState.setTheme`.
pub fn wants(head: &[u8]) -> bool {
   host::table()
      .and_then(|table| table.call(Method::AppStateSetTheme))
      .is_some_and(|target| HostCall::target(head) == Some(target))
}

/// Keeps the palette an `AppState.setTheme` call carries.
pub fn set_theme(frame: &[u8]) {
   let Ok(call) = HostCall::decode(frame) else {
      return;
   };
   let Some(&Value::Bool(dark)) = host::field(&call.args, Field::Dark) else {
      return;
   };
   let color = |field| {
      host::field(&call.args, field)
         .and_then(color_value)
         .map(argb_bits)
   };
   let palette = Palette {
      dark,
      surface: color(Field::Surface),
      text: color(Field::Text),
      text_default: color(Field::TextDefault),
      text_muted: color(Field::TextMuted),
      ripple: color(Field::Ripple),
      danger: color(Field::Danger),
   };
   let previous = PALETTE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .replace(palette);
   if previous != Some(palette) {
      let surface = palette.surface.unwrap_or_default();
      log::info!(
         "Discord's theme is {} on {surface:08X}",
         if dark { "dark" } else { "light" }
      );
   }
}

/// Whether the settings pages need drawing again: the palette changed, or,
/// without one, Phoenix's settings text flipped between light and dark.
pub fn observe(ops: &[Op]) -> bool {
   if let Some(palette) = palette() {
      THEME.store(if palette.dark { DARK } else { LIGHT }, Ordering::Relaxed);
      let drawn = DRAWN
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .replace(palette);
      return drawn != Some(palette);
   }
   let mut light_text = 0;
   let mut dark_text = 0;
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
         light_text += 1;
      } else if luminance > 0.65 {
         dark_text += 1;
      }
   }
   let current = THEME.load(Ordering::Relaxed);
   let next = if light_text >= 3 && light_text >= dark_text + 2 {
      LIGHT
   } else if dark_text >= 3 && dark_text >= light_text + 2 {
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

fn light() -> bool {
   THEME.load(Ordering::Relaxed) == LIGHT
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
      let from_palette = palette().and_then(|palette| {
         match self {
            Self::Text => palette.text_default,
            Self::Muted => palette.text_muted,
            Self::Error => palette.danger,
            _ => None,
         }
      });
      if let Some(color) = from_palette {
         return i64::from(color);
      }
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

/// Maps a captured layout color, taken in the Dark theme, onto the palette,
/// or onto the light colors on a light screen without one.
pub fn template_color(color: i64) -> i64 {
   let role = palette().and_then(|palette| {
      match argb_bits(color) {
         0xFF24_2429 => palette.surface,
         0xFFEF_EFF1 => palette.text_default,
         0xFFFB_FBFB => palette.text,
         0xFF96_979E | 0xFFAB_ACB2 => palette.text_muted,
         0x12FF_FFFF => palette.ripple,
         _ => None,
      }
   });
   if let Some(color) = role {
      return i64::from(color);
   }
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
