//! Canonical sRGB/HSLA color types, CSS color parsing, and WCAG contrast helpers.

/// 8-bit sRGB color with alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
   pub r: u8,
   pub g: u8,
   pub b: u8,
   pub a: u8,
}

/// Hue, saturation, lightness, alpha, each normalized to `0.0..=1.0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsla {
   pub h: f32,
   pub s: f32,
   pub l: f32,
   pub a: f32,
}

/// Clamps to `0.0..=1.0`, treating `NaN` as `0.0`.
#[must_use]
pub const fn clamp01(v: f32) -> f32 {
   if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

fn channel_to_u8(v: f32) -> u8 {
   let scaled = (clamp01(v) * 255.0).round();
   // `scaled` is guaranteed finite in `0.0..=255.0`.
   #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
   {
      scaled as u8
   }
}

impl Color {
   pub const WHITE: Self = Self::rgb(255, 255, 255);
   pub const DARK_INK: Self = Self::rgb(17, 18, 20);
   pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

   #[must_use]
   pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
      Self { r, g, b, a: 255 }
   }

   #[must_use]
   pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
      Self { r, g, b, a }
   }

   /// Packs channels as `0xRRGGBBAA`.
   #[must_use]
   pub const fn to_rgba_u32(self) -> u32 {
      u32::from_be_bytes([self.r, self.g, self.b, self.a])
   }

   /// Unpacks `0xRRGGBBAA`.
   #[must_use]
   pub const fn from_rgba_u32(packed: u32) -> Self {
      let [r, g, b, a] = packed.to_be_bytes();
      Self { r, g, b, a }
   }

   /// Packs channels as `0xAARRGGBB`.
   #[must_use]
   pub const fn to_argb_u32(self) -> u32 {
      u32::from_be_bytes([self.a, self.r, self.g, self.b])
   }

   /// Unpacks `0xAARRGGBB`.
   #[must_use]
   pub const fn from_argb_u32(packed: u32) -> Self {
      let [a, r, g, b] = packed.to_be_bytes();
      Self { r, g, b, a }
   }

   #[must_use]
   pub const fn with_alpha(self, a: u8) -> Self {
      Self { a, ..self }
   }

   #[must_use]
   pub fn with_alpha_f32(self, a: f32) -> Self {
      self.with_alpha(channel_to_u8(a))
   }

   #[must_use]
   pub fn to_hex(self) -> String {
      if self.a == 255 {
         format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
      } else {
         format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
      }
   }

   #[must_use]
   pub fn to_hsla(self) -> Hsla {
      rgb_to_hsla(
         f32::from(self.r) / 255.0,
         f32::from(self.g) / 255.0,
         f32::from(self.b) / 255.0,
         f32::from(self.a) / 255.0,
      )
   }

   #[must_use]
   pub fn from_hsla(hsla: Hsla) -> Self {
      let hue = if hsla.h.is_finite() {
         hsla.h.rem_euclid(1.0)
      } else {
         0.0
      };
      let sat = clamp01(hsla.s);
      let light = clamp01(hsla.l);
      let alpha = clamp01(hsla.a);
      if sat <= 1.0e-6 {
         let gray = channel_to_u8(light);
         return Self::rgba(gray, gray, gray, channel_to_u8(alpha));
      }
      let hi = if light < 0.5 {
         light * (1.0 + sat)
      } else {
         light.mul_add(-sat, light + sat)
      };
      let lo = 2.0f32.mul_add(light, -hi);
      let hue_to_rgb = |mut t: f32| -> f32 {
         if t < 0.0 {
            t += 1.0;
         }
         if t > 1.0 {
            t -= 1.0;
         }
         if t < 1.0 / 6.0 {
            return (hi - lo).mul_add(6.0 * t, lo);
         }
         if t < 0.5 {
            return hi;
         }
         if t < 2.0 / 3.0 {
            return (hi - lo).mul_add((2.0 / 3.0 - t) * 6.0, lo);
         }
         lo
      };
      Self::rgba(
         channel_to_u8(hue_to_rgb(hue + 1.0 / 3.0)),
         channel_to_u8(hue_to_rgb(hue)),
         channel_to_u8(hue_to_rgb(hue - 1.0 / 3.0)),
         channel_to_u8(alpha),
      )
   }

   /// Shifts HSL lightness by `delta` (positive brightens, negative darkens).
   #[must_use]
   pub fn shift_lightness(self, delta: f32) -> Self {
      let mut hsla = self.to_hsla();
      hsla.l = clamp01(hsla.l + delta);
      Self::from_hsla(hsla)
   }

   /// Linear sRGB blend from `self` toward `other` by `t` in `0.0..=1.0`.
   #[must_use]
   pub fn mix(self, other: Self, t: f32) -> Self {
      let t = clamp01(t);
      let blend = |a: u8, b: u8| -> u8 {
         let af = f32::from(a);
         let bf = f32::from(b);
         channel_to_u8((bf - af).mul_add(t, af) / 255.0)
      };
      Self::rgba(
         blend(self.r, other.r),
         blend(self.g, other.g),
         blend(self.b, other.b),
         blend(self.a, other.a),
      )
   }

   /// WCAG 2.1 relative luminance in `0.0..=1.0`.
   #[must_use]
   pub fn relative_luminance(self) -> f32 {
      let linear = |channel: u8| -> f32 {
         let s = f32::from(channel) / 255.0;
         if s <= 0.03928 {
            s / 12.92
         } else {
            ((s + 0.055) / 1.055).powf(2.4)
         }
      };
      0.2126f32.mul_add(
         linear(self.r),
         0.7152f32.mul_add(linear(self.g), 0.0722 * linear(self.b)),
      )
   }

   /// WCAG 2.1 contrast ratio in `1.0..=21.0`.
   #[must_use]
   pub fn contrast_ratio(self, other: Self) -> f32 {
      let l1 = self.relative_luminance();
      let l2 = other.relative_luminance();
      let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
      (hi + 0.05) / (lo + 0.05)
   }

   /// Picks white or dark ink, whichever gives higher WCAG contrast on `self`.
   #[must_use]
   pub fn readable_foreground(self) -> Self {
      if self.contrast_ratio(Self::WHITE) >= self.contrast_ratio(Self::DARK_INK) {
         Self::WHITE
      } else {
         Self::DARK_INK
      }
   }
}

#[must_use]
#[expect(clippy::many_single_char_names, reason = "standard color channels")]
pub fn rgb_to_hsla(r: f32, g: f32, b: f32, a: f32) -> Hsla {
   let (r, g, b, a) = (clamp01(r), clamp01(g), clamp01(b), clamp01(a));
   let (max, min) = (r.max(g).max(b), r.min(g).min(b));
   let delta = max - min;
   let l = f32::midpoint(max, min);
   if delta <= 1.0e-6 {
      return Hsla {
         h: 0.0,
         s: 0.0,
         l,
         a,
      };
   }
   let s = if l > 0.5 {
      delta / (2.0 - max - min)
   } else {
      delta / (max + min)
   };
   let h = if (max - r).abs() <= f32::EPSILON {
      (g - b) / delta + if g < b { 6.0 } else { 0.0 }
   } else if (max - g).abs() <= f32::EPSILON {
      (b - r) / delta + 2.0
   } else {
      (r - g) / delta + 4.0
   };
   Hsla {
      h: (h / 6.0).rem_euclid(1.0),
      s,
      l,
      a,
   }
}

/// Parses a leading finite number and its trailing unit slice.
pub(crate) fn parse_number(s: &str) -> Option<(f32, &str)> {
   let end = s
      .char_indices()
      .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && matches!(c, '+' | '-'))))
      .map_or(s.len(), |(i, _)| i);
   let v: f32 = s[..end].parse().ok()?;
   v.is_finite().then_some((v, &s[end..]))
}

fn color_args(text: &str) -> Option<Vec<(f32, &str)>> {
   let open = text.find('(')?;
   let close = text.rfind(')')?;
   if close <= open {
      return None;
   }
   let inner = &text[open + 1..close];
   let args = inner
      .split([' ', ',', '/', '\t'])
      .filter(|t| !t.is_empty())
      .map(parse_number)
      .collect::<Option<Vec<_>>>()?;
   (3..=4).contains(&args.len()).then_some(args)
}

fn alpha_arg(args: &[(f32, &str)]) -> f32 {
   args
      .get(3)
      .map_or(1.0, |&(v, unit)| if unit == "%" { v / 100.0 } else { v })
}

fn named_css_color(name: &str) -> Option<Color> {
   let rgb = match name {
      "black" => (0x00, 0x00, 0x00),
      "silver" => (0xc0, 0xc0, 0xc0),
      "gray" | "grey" => (0x80, 0x80, 0x80),
      "white" => (0xff, 0xff, 0xff),
      "maroon" => (0x80, 0x00, 0x00),
      "red" => (0xff, 0x00, 0x00),
      "purple" => (0x80, 0x00, 0x80),
      "fuchsia" | "magenta" => (0xff, 0x00, 0xff),
      "green" => (0x00, 0x80, 0x00),
      "lime" => (0x00, 0xff, 0x00),
      "olive" => (0x80, 0x80, 0x00),
      "yellow" => (0xff, 0xff, 0x00),
      "navy" => (0x00, 0x00, 0x80),
      "blue" => (0x00, 0x00, 0xff),
      "teal" => (0x00, 0x80, 0x80),
      "aqua" | "cyan" => (0x00, 0xff, 0xff),
      "orange" => (0xff, 0xa5, 0x00),
      "rebeccapurple" => (0x66, 0x33, 0x99),
      "pink" => (0xff, 0xc0, 0xcb),
      "gold" => (0xff, 0xd7, 0x00),
      "coral" => (0xff, 0x7f, 0x50),
      "crimson" => (0xdc, 0x14, 0x3c),
      "indigo" => (0x4b, 0x00, 0x82),
      "violet" => (0xee, 0x82, 0xee),
      "brown" => (0xa5, 0x2a, 0x2a),
      "darkgray" | "darkgrey" => (0xa9, 0xa9, 0xa9),
      "lightgray" | "lightgrey" => (0xd3, 0xd3, 0xd3),
      "dimgray" | "dimgrey" => (0x69, 0x69, 0x69),
      "slategray" | "slategrey" => (0x70, 0x80, 0x90),
      "cornflowerblue" => (0x64, 0x95, 0xed),
      "dodgerblue" => (0x1e, 0x90, 0xff),
      "deepskyblue" => (0x00, 0xbf, 0xff),
      "royalblue" => (0x41, 0x69, 0xe1),
      "tomato" => (0xff, 0x63, 0x47),
      "orchid" => (0xda, 0x70, 0xd6),
      "plum" => (0xdd, 0xa0, 0xdd),
      "salmon" => (0xfa, 0x80, 0x72),
      "turquoise" => (0x40, 0xe0, 0xd0),
      "lavender" => (0xe6, 0xe6, 0xfa),
      "ivory" => (0xff, 0xff, 0xf0),
      "beige" => (0xf5, 0xf5, 0xdc),
      "khaki" => (0xf0, 0xe6, 0x8c),
      "tan" => (0xd2, 0xb4, 0x8c),
      "wheat" => (0xf5, 0xde, 0xb3),
      _ => return None,
   };
   Some(Color::rgb(rgb.0, rgb.1, rgb.2))
}

/// Parses `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb[a]()`, `hsl[a]()`,
/// `transparent`, and common CSS named colors.
#[must_use]
pub fn parse_css_color(raw: &str) -> Option<Color> {
   let lower = raw.trim().to_ascii_lowercase();
   if lower == "transparent" {
      return Some(Color::TRANSPARENT);
   }
   if let Some(named) = named_css_color(&lower) {
      return Some(named);
   }
   if let Some(hex) = lower.strip_prefix('#') {
      let expanded: String = match hex.len() {
         3 => {
            hex.chars()
               .flat_map(|c| [c, c])
               .chain("ff".chars())
               .collect()
         },
         4 => hex.chars().flat_map(|c| [c, c]).collect(),
         6 => format!("{hex}ff"),
         8 => hex.to_owned(),
         _ => return None,
      };
      let [r, g, b, a] = u32::from_str_radix(&expanded, 16).ok()?.to_be_bytes();
      return Some(Color::rgba(r, g, b, a));
   }
   if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
      let args = color_args(&lower)?;
      let ch = |i: usize| -> u8 {
         let (v, unit) = args[i];
         if unit == "%" {
            channel_to_u8(v / 100.0)
         } else {
            channel_to_u8(v / 255.0)
         }
      };
      return Some(Color::rgba(
         ch(0),
         ch(1),
         ch(2),
         channel_to_u8(alpha_arg(&args)),
      ));
   }
   if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
      let args = color_args(&lower)?;
      let (hue, unit) = args[0];
      let turns = match unit {
         "turn" => hue,
         "grad" => hue / 400.0,
         "rad" => hue / std::f32::consts::TAU,
         "" | "deg" => hue / 360.0,
         _ => return None,
      };
      let hsla = Hsla {
         h: turns.rem_euclid(1.0),
         s: clamp01(args[1].0 / 100.0),
         l: clamp01(args[2].0 / 100.0),
         a: clamp01(alpha_arg(&args)),
      };
      return Some(Color::from_hsla(hsla));
   }
   None
}
