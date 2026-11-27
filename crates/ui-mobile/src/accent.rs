//! Remap resolved Discord UI colors while retaining host values for live theme
//! changes.
mod animated;
mod canvas;

use std::collections::{
   HashMap,
   HashSet,
};

use phoenix_wire::Value;

use crate::{
   Op,
   Prop,
   mirror::Mirror,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Palette {
   pub primary:    u32,
   pub on_primary: u32,
   pub container:  u32,
}

struct Paint {
   key:     Prop,
   source:  Value,
   applied: bool,
}

#[derive(Default)]
pub struct Accents {
   animated:  animated::Animated,
   owners:    Vec<(String, Palette)>,
   originals: HashMap<i32, Vec<Paint>>,
}

pub(crate) fn argb(v: &Value) -> Option<u32> {
   match v {
      Value::I32(n) => Some(n.cast_unsigned()),
      Value::I64(n) => {
         u32::try_from(*n)
            .ok()
            .or_else(|| i32::try_from(*n).ok().map(i32::cast_unsigned))
      },
      Value::F64(number)
         if number.fract() == 0.0
            && (f64::from(i32::MIN)..=f64::from(u32::MAX)).contains(number) =>
      {
         #[expect(
            clippy::cast_possible_truncation,
            reason = "validated exact 32-bit integer color"
         )]
         let integer = *number as i64;
         u32::try_from(integer)
            .ok()
            .or_else(|| i32::try_from(integer).ok().map(i32::cast_unsigned))
      },
      _ => None,
   }
}

enum Tone {
   Brand,
   Pale,
   Link,
}

// Named blurple, brand and text-link stops in the mobile resource palette.
#[expect(
   clippy::unreadable_literal,
   reason = "match named RGB hex swatches as written in the palette"
)]
const fn tone(rgb: u32) -> Option<Tone> {
   match rgb & 0x00FF_FFFF {
      0xDFE4FF | 0xE7E9FD | 0xDEE0FC | 0xC9CDFB | 0xBCCAFF | 0xB9C8FF => Some(Tone::Pale),
      0x5865F2 | 0x9DB0FF | 0x9AADFF | 0x94A8FF | 0x8195FB | 0x798DF9 | 0x6374F4 | 0x525FE0
      | 0x404EB1 | 0x3A48A3 | 0x273478 | 0x7984F5 | 0x707BF4 | 0x6571F3 | 0x505CDC | 0x4752C4
      | 0x3C45A5 | 0x404EED | 0x7289DA => Some(Tone::Brand),
      0x8EBEFA | 0x7AB2F7 | 0x76AFF6 | 0x66A5F3 | 0x4D96EE | 0x4591EC | 0x2781E7 | 0x006DD4
      | 0x0361BC | 0x003064 | 0x00A8FC | 0x00AFF4 | 0x006CE7 | 0x0068E0 => Some(Tone::Link),
      _ => None,
   }
}

/// Whether a surface uses an original Discord brand fill (including translucent
/// fills).
#[must_use]
pub const fn brand_fill(color: u32) -> bool {
   color >> 24 != 0 && matches!(tone(color), Some(Tone::Brand | Tone::Pale))
}

fn scalar(prop: Prop) -> bool {
   matches!(
      prop.name(),
      Some(
         "color"
            | "tint"
            | "fill"
            | "background"
            | "bg"
            | "activeBackground"
            | "borderColor"
            | "bgBorder"
            | "activeBorderColor"
            | "focusBorderColor"
            | "selectionColor"
            | "minTrackTint"
            | "maxTrackTint"
            | "thumbTint"
            | "handleColor"
            | "ripple"
            | "placeholderColor"
      )
   )
}

fn protected(mirror: &Mirror, id: i32) -> bool {
   let mut node = Some(id);
   for _ in 0..8 {
      let Some(id) = node else { break };
      if mirror.accessibility_id(id).is_some_and(|a| {
         a.starts_with("message.author:")
            || a.starts_with("message.avatar:")
            || a.starts_with("embed:author")
      }) {
         return true;
      }
      node = mirror.parent(id);
   }
   false
}

fn on_brand(mirror: &Mirror, id: i32) -> bool {
   let mut node = Some(id);
   for _ in 0..6 {
      let Some(id) = node else { break };
      if let Some(fill) = mirror.surface(id).and_then(crate::mirror::Surface::fill) {
         return matches!(tone(fill), Some(Tone::Brand));
      }
      node = mirror.parent(id);
   }
   false
}

fn colored(source: u32, target: u32) -> Value {
   Value::I64(i64::from((source & 0xFF00_0000) | (target & 0x00FF_FFFF)))
}

// Phoenix uses numeric keys for compact records and strings for map records.
fn record_field(record: &Value, key: Prop) -> Option<&Value> {
   if let Value::Map(entries) = record {
      let name = key.name()?;
      return entries.iter().find(|(k, _)| k == name).map(|(_, v)| v);
   }
   record.field(key.key())
}

fn set_record_field(record: &mut Value, key: Prop, value: Value) {
   match record {
      Value::Obj(entries) => {
         if let Some((_, v)) = entries.iter_mut().find(|(k, _)| *k == key.key()) {
            *v = value;
         } else {
            entries.push((key.key(), value));
         }
      },
      Value::Map(entries) => {
         let Some(name) = key.name() else { return };
         if let Some((_, v)) = entries.iter_mut().find(|(k, _)| k == name) {
            *v = value;
         } else {
            entries.push((name.to_owned(), value));
         }
      },
      _ => {},
   }
}

fn link_color(span: &Value) -> Option<u32> {
   let url = crate::prop!("url");
   let linked = [url, crate::prop!("link")]
      .into_iter()
      .filter_map(|k| record_field(span, k))
      .any(|v| {
         v.as_str()
            .or_else(|| record_field(v, url).and_then(Value::as_str))
            .is_some_and(|s| s.starts_with("https://") || s.starts_with("http://"))
      });
   let color = record_field(span, Prop::COLOR).and_then(argb);
   (linked || color.is_some_and(|c| matches!(tone(c), Some(Tone::Link))))
      .then_some(color.unwrap_or(0xFF00_0000))
}

fn blur(prop: Prop) -> bool {
   prop == crate::prop!("blur") || prop == crate::prop!("activeBlur")
}

pub(crate) fn blur_fallback(value: &Value) -> Option<u32> {
   record_field(value, crate::prop!("fallback")).and_then(argb)
}

fn gradient(prop: Prop) -> bool {
   matches!(
      prop.name(),
      Some("backgroundGradient" | "activeBackgroundGradient" | "gradient")
   )
}

fn stop_colors(value: &Value) -> Option<&Vec<Value>> {
   match value {
      Value::Arr(colors) => Some(colors),
      Value::Obj(_) => {
         match value.field(crate::prop!("colors").key())? {
            Value::Arr(colors) => Some(colors),
            _ => None,
         }
      },
      _ => None,
   }
}

fn text_runs(prop: Prop) -> bool {
   prop == crate::prop!("spans")
}

fn candidate(prop: Prop, value: &Value) -> bool {
   if blur(prop) {
      return blur_fallback(value).is_some_and(brand_fill);
   }
   if prop == canvas::DRAW {
      return matches!(value, Value::Arr(_) | Value::Words(_));
   }
   if text_runs(prop) {
      return matches!(value, Value::Arr(spans) if spans.iter().any(|s| link_color(s).is_some()));
   }
   if gradient(prop) {
      return stop_colors(value).is_some_and(|v| v.iter().any(|s| argb(s).is_some_and(brand_fill)));
   }
   scalar(prop)
      && argb(value).is_some_and(|c| {
         c >> 24 != 0
            && (tone(c).is_some()
               || ((prop == Prop::COLOR || prop == Prop::TINT) && c & 0x00FF_FFFF == 0x00FF_FFFF))
      })
}

fn themed_blur(mirror: &Mirror, id: i32, v: &Value, palette: Palette) -> Option<Value> {
   let color = blur_fallback(v)?;
   if !brand_fill(color) || protected(mirror, id) {
      return None;
   }
   // Android uses the blur fallback in preference to the ordinary fill.
   // Rewrite it in the guest's batch, including pressed-state updates.
   let target = if matches!(tone(color), Some(Tone::Pale)) {
      palette.container
   } else {
      palette.primary
   };
   let mut out = v.clone();
   set_record_field(&mut out, crate::prop!("fallback"), colored(color, target));
   Some(out)
}

fn themed_gradient(mirror: &Mirror, id: i32, v: &Value, palette: Palette) -> Option<Value> {
   let stops = stop_colors(v)?;
   if protected(mirror, id) || !stops.iter().any(|s| argb(s).is_some_and(brand_fill)) {
      return None;
   }
   let mut out = v.clone();
   let colors = match &mut out {
      Value::Arr(colors) => colors,
      Value::Obj(fields) => {
         let (_, Value::Arr(colors)) = fields
            .iter_mut()
            .find(|(k, _)| *k == crate::prop!("colors").key())?
         else {
            return None;
         };
         colors
      },
      _ => return None,
   };
   for stop in colors {
      if let Some(c) = argb(stop).filter(|c| brand_fill(*c)) {
         *stop = colored(
            c,
            if matches!(tone(c), Some(Tone::Pale)) {
               palette.container
            } else {
               palette.primary
            },
         );
      }
   }
   Some(out)
}

fn replacement(
   mirror: &Mirror,
   id: i32,
   prop: Prop,
   v: &Value,
   palette: Option<Palette>,
) -> Option<Value> {
   if blur(prop) {
      return themed_blur(mirror, id, v, palette?);
   }
   if prop == canvas::DRAW {
      return canvas::repaint(v, mirror, id, palette?);
   }
   if gradient(prop) {
      return themed_gradient(mirror, id, v, palette?);
   }
   if text_runs(prop) {
      let Value::Arr(spans) = v else { return None };
      let mut result = None;
      let pal = palette.filter(|_| prop == crate::prop!("spans"));
      for (i, span) in spans.iter().enumerate() {
         if let Some((color, palette)) = link_color(span).zip(pal)
            && !protected(mirror, id)
         {
            let out = result.get_or_insert_with(|| spans.clone());
            set_record_field(&mut out[i], Prop::COLOR, colored(color, palette.primary));
         }
      }
      return result.map(Value::Arr);
   }
   if !scalar(prop) {
      return None;
   }
   let palette = palette?;
   let color = argb(v)?;
   if color >> 24 == 0 {
      return None;
   }
   let foreground = prop == Prop::COLOR || prop == Prop::TINT;
   let rgb = color & 0x00FF_FFFF;
   let target = match tone(rgb) {
      Some(Tone::Pale) if !foreground => palette.container,
      Some(Tone::Brand | Tone::Pale) => palette.primary,
      Some(Tone::Link) if foreground => palette.primary,
      None if foreground && rgb == 0x00FF_FFFF && on_brand(mirror, id) => palette.on_primary,
      _ => return None,
   };
   if protected(mirror, id) {
      return None;
   }
   Some(colored(color, target))
}

impl Accents {
   pub fn set(&mut self, owner: &str, palette: Option<Palette>) -> bool {
      let before = self.owners.last().map(|o| o.1);
      self.owners.retain(|o| o.0 != owner);
      if let Some(p) = palette {
         self.owners.push((owner.to_owned(), p));
      }
      before != self.owners.last().map(|o| o.1)
   }

   /// Keep only potentially themed properties so partial updates, row reuse and
   /// plugin unload all have the original value available.
   pub fn rewrite(&mut self, ops: &mut Vec<Op>, mirror: &Mirror) -> bool {
      let palette = self.owners.last().map(|o| o.1);
      let mut changed = false;
      let mut roots = Vec::new();
      for op in &mut *ops {
         match op {
            Op::Motion { words } => {
               changed |= self.animated.rewrite(words, mirror, palette);
               continue;
            },
            Op::Children { ids, .. } => roots.extend_from_slice(ids),
            Op::ChildEdits { id, .. } => roots.push(*id),
            Op::Delete { ids } => {
               for id in ids {
                  self.originals.remove(id);
                  self.animated.forget(*id);
               }
            },
            Op::Create { id, .. } => {
               self.animated.forget(*id);
               self.originals.remove(id);
            },
            _ => {},
         }
         let (Op::Create { id, props, .. } | Op::Update { id, props }) = op else {
            continue;
         };
         for (prop, value) in &mut *props {
            if *prop == canvas::EDITS {
               changed |= self.expand_draw_edits(mirror, palette, *id, prop, value);
            }
            if *prop == crate::prop!("worklet") {
               self.animated.bind(*id, value);
               continue;
            }
            if matches!(*prop, Prop::ACCESSIBILITY_ID | Prop::ROLE | Prop::COLOR)
               || matches!(prop.name(), Some("background" | "bg" | "blur"))
            {
               roots.push(*id);
            }
            let candidate = candidate(*prop, value);
            if !candidate {
               self.remove(*id, *prop);
               continue;
            }
            let mapped = replacement(mirror, *id, *prop, value, palette);
            let item = Paint {
               key:     *prop,
               source:  value.clone(),
               applied: mapped.is_some(),
            };
            let stored = self.originals.entry(*id).or_default();
            if let Some(old) = stored.iter_mut().find(|old| old.key == *prop) {
               *old = item;
            } else {
               stored.push(item);
            }
            if let Some(mapped) = mapped {
               *value = mapped;
               changed = true;
            }
         }
      }
      if palette.is_some() {
         let mut seen = HashSet::new();
         while let Some(id) = roots.pop() {
            if !seen.insert(id) {
               continue;
            }
            roots.extend_from_slice(mirror.kids(id));
            if let Some(paints) = self.originals.get_mut(&id)
               && let Some(op) = repaint(paints, mirror, id, palette, false)
            {
               ops.push(op);
               changed = true;
            }
         }
      }
      changed
   }

   fn expand_draw_edits(
      &mut self,
      mirror: &Mirror,
      palette: Option<Palette>,
      id: i32,
      prop: &mut Prop,
      value: &mut Value,
   ) -> bool {
      let Some(paint) = self
         .originals
         .get_mut(&id)
         .and_then(|v| v.iter_mut().find(|p| p.key == canvas::DRAW))
      else {
         return false;
      };
      if canvas::merge(&mut paint.source, value).is_none() {
         self.remove(id, canvas::DRAW);
         return false;
      }
      if !paint.applied
         && palette
            .and_then(|p| canvas::repaint(&paint.source, mirror, id, p))
            .is_none()
      {
         return false;
      }
      // Replay the current full program when a partial edit changes a themed
      // plate.
      *prop = canvas::DRAW;
      *value = paint.source.clone();
      paint.applied
   }

   fn remove(&mut self, id: i32, key: Prop) {
      if let Some(props) = self.originals.get_mut(&id) {
         props.retain(|p| p.key != key);
         if props.is_empty() {
            self.originals.remove(&id);
         }
      }
   }

   pub fn refresh(&mut self, mirror: &Mirror) -> Vec<Op> {
      let palette = self.owners.last().map(|o| o.1);
      let mut ops: Vec<_> = self
         .originals
         .iter_mut()
         .filter_map(|(&id, props)| repaint(props, mirror, id, palette, true))
         .collect();
      ops.extend(self.animated.refresh(mirror, palette));
      ops
   }
}

fn repaint(
   paints: &mut [Paint],
   mirror: &Mirror,
   id: i32,
   palette: Option<Palette>,
   force: bool,
) -> Option<Op> {
   let mut props = Vec::new();
   for paint in paints {
      let mapped = replacement(mirror, id, paint.key, &paint.source, palette);
      if (force && mapped.is_some()) || paint.applied != mapped.is_some() {
         paint.applied = mapped.is_some();
         props.push((paint.key, mapped.unwrap_or_else(|| paint.source.clone())));
      }
   }
   (!props.is_empty()).then_some(Op::Update { id, props })
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::{
      Props,
      ViewKind,
   };

   const PINK: Palette = Palette {
      primary:    0xFF81_4D5F,
      on_primary: 0xFFFF_FFFF,
      container:  0xFFED_DDE1,
   };
   const DARK: Palette = Palette {
      primary:    0xFFEF_B3C5,
      on_primary: 0xFF4B_2733,
      container:  0xFF49_373E,
   };
   fn color(value: u32) -> Value {
      Value::I64(i64::from(value))
   }
   fn create(id: i32, kind: ViewKind, props: Props) -> Op {
      Op::Create { id, kind, props }
   }
   fn send(m: &mut Mirror, a: &mut Accents, mut ops: Vec<Op>) -> Vec<Op> {
      m.apply(&ops, false);
      a.rewrite(&mut ops, m);
      ops
   }
   fn get(ops: &[Op], id: i32, prop: Prop) -> Option<&Value> {
      ops.iter().rev().find_map(|op| {
         match op {
            Op::Create { id: n, props, .. } | Op::Update { id: n, props } if *n == id => {
               crate::get_prop(props, prop)
            },
            _ => None,
         }
      })
   }

   #[test]
   fn blur_fallback_is_themed_in_each_host_batch_before_a_button_is_tagged() {
      let (mut m, mut a) = (Mirror::default(), Accents::default());
      let blur = crate::prop!("blur");
      let active = crate::prop!("activeBlur");
      let fallback = crate::prop!("fallback");
      let radius = crate::prop!("radius");
      let brand = color(0xFF58_65F2);
      let regular = Value::Obj(vec![
         (fallback.key(), brand),
         (radius.key(), Value::I32(10)),
      ]);
      let pressed = Value::Map(vec![("fallback".into(), color(0x8058_65F2))]);
      a.set("theme", Some(DARK));
      let ops = send(&mut m, &mut a, vec![
         create(1, ViewKind::Stack, vec![
            (blur, regular.clone()),
            (active, pressed.clone()),
         ]),
         create(2, ViewKind::Text, vec![(Prop::COLOR, color(0xFFFF_FFFF))]),
         Op::Children {
            id:  1,
            ids: vec![2],
         },
      ]);
      assert_eq!(
         get(&ops, 1, blur).and_then(blur_fallback),
         Some(DARK.primary)
      );
      assert_eq!(
         get(&ops, 1, active).and_then(blur_fallback),
         Some(0x80EF_B3C5)
      );
      assert_eq!(get(&ops, 2, Prop::COLOR), Some(&color(DARK.on_primary)));
      assert_eq!(
         get(&ops, 1, blur).and_then(|v| record_field(v, radius)),
         Some(&Value::I32(10))
      );

      a.set("theme", Some(PINK));
      assert_eq!(
         get(&a.refresh(&m), 1, blur).and_then(blur_fallback),
         Some(PINK.primary)
      );
      let ops = send(&mut m, &mut a, vec![Op::Update {
         id:    1,
         props: vec![(active, pressed.clone()), (blur, regular.clone())],
      }]);
      assert_eq!(
         get(&ops, 1, active).and_then(blur_fallback),
         Some(0x8081_4D5F)
      );
      a.set("theme", None);
      let restored = a.refresh(&m);
      assert_eq!(get(&restored, 1, blur), Some(&regular));
      assert_eq!(get(&restored, 1, active), Some(&pressed));
      a.set("theme", Some(DARK));
      assert_eq!(
         get(&a.refresh(&m), 2, Prop::COLOR),
         Some(&color(DARK.on_primary))
      );
      let ops = send(&mut m, &mut a, vec![Op::Update {
         id:    1,
         props: vec![(blur, Value::Null), (active, Value::Null)],
      }]);
      assert_eq!(get(&ops, 1, blur), Some(&Value::Null));
      assert_eq!(get(&ops, 2, Prop::COLOR), Some(&color(0xFFFF_FFFF)));
      assert!(get(&a.refresh(&m), 1, blur).is_none());
   }

   #[test]
   fn reaction_reuse_and_theme_changes_keep_host_alpha_and_current_state() {
      let (mut m, mut a) = (Mirror::default(), Accents::default());
      let bg = crate::prop!("background");
      let stroke = crate::prop!("borderColor");
      let original = color(0x3058_65F2);
      let ops = send(&mut m, &mut a, vec![create(1, ViewKind::View, vec![
         (bg, original.clone()),
         (stroke, color(0xFF58_65F2)),
      ])]);
      assert_eq!(get(&ops, 1, bg), Some(&original));
      a.set("theme", Some(PINK));
      let ops = a.refresh(&m);
      assert_eq!(get(&ops, 1, bg), Some(&color(0x3081_4D5F)));
      assert_eq!(get(&ops, 1, stroke), Some(&color(PINK.primary)));
      a.set("theme", Some(DARK));
      assert_eq!(get(&a.refresh(&m), 1, bg), Some(&color(0x30EF_B3C5)));
      let neutral = color(0xFF34_3335);
      send(&mut m, &mut a, vec![Op::Update {
         id:    1,
         props: vec![(bg, neutral), (stroke, Value::Null)],
      }]);
      a.set("theme", None);
      assert_eq!(a.refresh(&m), [] as [Op; 0]);
      a.set("theme", Some(PINK));
      send(&mut m, &mut a, vec![Op::Update {
         id:    1,
         props: vec![(bg, original.clone())],
      }]);
      a.set("theme", None);
      assert_eq!(get(&a.refresh(&m), 1, bg), Some(&original));
      send(&mut m, &mut a, vec![
         Op::Delete { ids: vec![1] },
         create(1, ViewKind::Image, vec![(
            Prop::URI,
            Value::Str("icon".into()),
         )]),
      ]);
      a.set("theme", Some(DARK));
      assert_eq!(a.refresh(&m), [] as [Op; 0]);
   }

   #[test]
   fn canvas_button_remaps_fill_words_and_replays_edits_without_coloring_geometry() {
      let (mut m, mut a) = (Mirror::default(), Accents::default());
      a.set("theme", Some(DARK));
      let brand = 0xFF58_65F2_u32.cast_signed();
      let source = vec![258, brand, 1040, brand, 0, 0, 0];
      let ops = send(&mut m, &mut a, vec![
         create(12, ViewKind::View, vec![(
            Prop::ROLE,
            Value::Str("button".into()),
         )]),
         create(13, ViewKind::Canvas, vec![(
            canvas::DRAW,
            Value::Words(source.clone()),
         )]),
         Op::Children {
            id:  12,
            ids: vec![13],
         },
      ]);
      let Some(Value::Words(painted)) = get(&ops, 13, canvas::DRAW) else {
         panic!()
      };
      assert_eq!(painted[1], DARK.primary.cast_signed());
      assert_eq!(painted[3], brand);
      let edited = send(&mut m, &mut a, vec![Op::Update {
         id:    13,
         props: vec![(canvas::EDITS, Value::Words(vec![6, 42]))],
      }]);
      let Some(Value::Words(painted)) = get(&edited, 13, canvas::DRAW) else {
         panic!()
      };
      assert_eq!(painted[1], DARK.primary.cast_signed());
      assert_eq!(painted[6], 42);
      a.set("theme", None);
      let mut expected = source;
      expected[6] = 42;
      assert_eq!(
         get(&a.refresh(&m), 13, canvas::DRAW),
         Some(&Value::Words(expected))
      );
   }

   #[test]
   fn native_color_motion_uses_monet_targets_and_keeps_the_spring() {
      let (mut m, mut a) = (Mirror::default(), Accents::default());
      a.set("m3e", Some(PINK));
      let program = Value::Words(vec![-1_792_780_819, 1, 8, 1, 14, 15, 16, 17]);
      send(&mut m, &mut a, vec![create(1, ViewKind::View, vec![(
         crate::prop!("worklet"),
         program,
      )])]);
      let spring = |c: u32| {
         crate::motion::words(
            &[14, 15, 16, 17]
               .into_iter()
               .zip(c.to_be_bytes())
               .map(|(slot, b)| {
                  crate::motion::Command::Spring {
                     slot,
                     target: f32::from(b),
                     spring: crate::motion::Spring::SWITCH,
                  }
               })
               .collect::<Vec<_>>(),
         )
      };
      let ops = send(&mut m, &mut a, vec![Op::Motion {
         words: spring(0x9958_65F2),
      }]);
      assert!(matches!(&ops[0],Op::Motion{words} if *words==spring(0x9981_4d5f)));
      a.set("m3e", Some(DARK));
      assert!(
         a.refresh(&m)
            .iter()
            .any(|op| matches!(op, Op::Motion { .. }))
      );
      let neutral = spring(0xFF83_8383);
      let ops = send(&mut m, &mut a, vec![Op::Motion {
         words: neutral.clone(),
      }]);
      assert!(matches!(&ops[0],Op::Motion{words} if *words==neutral));
      send(&mut m, &mut a, vec![Op::Delete { ids: vec![1] }]);
      assert_eq!(a.refresh(&m), [] as [Op; 0]);
   }

   #[test]
   fn links_only_change_text_paint_and_brand_buttons_pair_their_foreground() {
      let (mut m, mut a) = (Mirror::default(), Accents::default());
      a.set("theme", Some(DARK));
      let text = (Prop::TEXT.key(), Value::Str("link".into()));
      let action = (
         crate::prop!("url").key(),
         Value::Str("https://example.org/".into()),
      );
      let link = Value::Obj(vec![text.clone(), action.clone()]);
      let normal = Value::Obj(vec![text.clone(), (Prop::COLOR.key(), color(0xFF80_2291))]);
      let spans = crate::prop!("spans");
      let bg = crate::prop!("background");
      let original = Value::Arr(vec![normal.clone(), link]);
      let ops = send(&mut m, &mut a, vec![
         create(1, ViewKind::View, vec![(bg, color(0xFF58_65F2))]),
         create(2, ViewKind::Text, vec![(Prop::COLOR, color(0xFFFF_FFFF))]),
         create(3, ViewKind::Text, vec![(spans, original.clone())]),
         create(4, ViewKind::Text, vec![
            (
               Prop::ACCESSIBILITY_ID,
               Value::Str("message.author:1".into()),
            ),
            (Prop::COLOR, color(0xFF58_65F2)),
         ]),
         Op::Children {
            id:  1,
            ids: vec![2, 3, 4],
         },
      ]);
      assert_eq!(get(&ops, 1, bg), Some(&color(DARK.primary)));
      assert_eq!(get(&ops, 2, Prop::COLOR), Some(&color(DARK.on_primary)));
      assert_eq!(get(&ops, 4, Prop::COLOR), Some(&color(0xFF58_65F2)));
      let themed = Value::Arr(vec![
         normal,
         Value::Obj(vec![text, action, (Prop::COLOR.key(), color(DARK.primary))]),
      ]);
      assert_eq!(get(&ops, 3, spans), Some(&themed));
      let ops = send(&mut m, &mut a, vec![Op::Update {
         id:    1,
         props: vec![(bg, color(0xFF44_4444))],
      }]);
      assert_eq!(get(&ops, 2, Prop::COLOR), Some(&color(0xFFFF_FFFF)));
      a.set("theme", None);
      assert_eq!(get(&a.refresh(&m), 3, spans), Some(&original));
   }
}
