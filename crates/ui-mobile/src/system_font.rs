//! Apply the platform sans-serif to text views and styled inline runs.
//! Keep their last incoming values so a live toggle can restore the same runs.
use std::collections::{
   HashMap,
   HashSet,
};

use phoenix_wire::Value;

use crate::{
   Op,
   Prop,
   Props,
   ViewKind,
   prop,
};

fn plain_family(v: &Value) -> bool {
   let name = v.as_str().unwrap_or_default().to_ascii_lowercase();
   !(name.contains("mono")
      || name.contains("emoji")
      || name.contains("icon")
      || name.contains("glyph"))
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

fn text_run(span: &Value) -> bool {
   matches!(span, Value::Obj(_) | Value::Map(_))
      && !matches!(
         record_field(span, crate::prop!("code")),
         Some(Value::Bool(true) | Value::I32(1))
      )
      && !matches!(
         record_field(span, crate::prop!("image")),
         Some(Value::Obj(_) | Value::Map(_))
      )
      && record_field(span, crate::prop!("font")).is_none_or(plain_family)
}

fn system_family() -> Value {
   Value::Str("sans-serif".into())
}

fn substitute(key: Prop, value: &Value) -> Option<Value> {
   if key == Prop::FONT_FAMILY {
      return (plain_family(value) && value.as_str() != Some("sans-serif")).then(system_family);
   }
   if key != prop!("spans") && key != prop!("valueSpans") {
      return None;
   }
   let Value::Arr(runs) = value else { return None };
   let mut changed = None;
   for (index, run) in runs.iter().enumerate() {
      // Unstyled runs inherit the TextView's face and weight. Assigning an
      // explicit family to those runs can erase a bold notification title.
      if text_run(run)
         && [prop!("font"), prop!("weight"), prop!("bold")]
            .iter()
            .any(|k| record_field(run, *k).is_some())
         && record_field(run, prop!("font")).and_then(Value::as_str) != Some("sans-serif")
      {
         let out = changed.get_or_insert_with(|| runs.clone());
         set_record_field(&mut out[index], prop!("font"), system_family());
      }
   }
   changed.map(Value::Arr)
}

#[derive(Default)]
pub struct SystemFont {
   owners: HashSet<String>,
   labels: HashMap<i32, Props>,
}

impl SystemFont {
   /// Apply after other view patches so changing a face preserves the latest
   /// colors, links, weights and input content supplied by the host or plugins.
   pub fn observe(&mut self, ops: &mut [Op]) -> bool {
      let enabled = !self.owners.is_empty();
      let mut changed = false;
      for op in ops {
         let (id, props, inherited) = match op {
            Op::Create { id, kind, props } => {
               self.labels.remove(id);
               if !matches!(kind, ViewKind::Text | ViewKind::Input | ViewKind::Markup) {
                  continue;
               }
               let saved = self.labels.entry(*id).or_default();
               let inherited = !props.iter().any(|(k, _)| *k == Prop::FONT_FAMILY);
               if inherited {
                  saved.push((Prop::FONT_FAMILY, Value::Null));
               }
               (*id, props, inherited)
            },
            Op::Update { id, props } => (*id, props, false),
            Op::Delete { ids } => {
               for id in ids {
                  self.labels.remove(id);
               }
               continue;
            },
            _ => continue,
         };
         let Some(saved) = self.labels.get_mut(&id) else {
            continue;
         };
         for (key, value) in &mut *props {
            if *key != Prop::FONT_FAMILY && *key != prop!("spans") && *key != prop!("valueSpans") {
               continue;
            }
            saved.retain(|(k, _)| k != key);
            if let Some(mapped) = substitute(*key, value) {
               saved.push((*key, value.clone()));
               if enabled {
                  *value = mapped;
                  changed = true;
               }
            }
         }
         if enabled && inherited {
            props.push((Prop::FONT_FAMILY, system_family()));
            changed = true;
         }
      }
      changed
   }

   pub fn set_owner(&mut self, plugin: &str, enable: bool) -> Vec<Op> {
      let was_empty = self.owners.is_empty();
      if enable {
         self.owners.insert(plugin.into());
      } else {
         self.owners.remove(plugin);
      }
      if was_empty == self.owners.is_empty() {
         return Vec::new();
      }
      self
         .labels
         .iter()
         .filter_map(|(&id, saved)| {
            let props: Props = if enable {
               saved
                  .iter()
                  .filter_map(|(k, v)| substitute(*k, v).map(|mapped| (*k, mapped)))
                  .collect()
            } else {
               saved.clone()
            };
            (!props.is_empty()).then_some(Op::Update { id, props })
         })
         .collect()
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::get_prop;

   fn text(s: &str) -> Value {
      Value::Str(s.into())
   }

   #[test]
   fn refresh_restores_the_latest_styled_runs_and_drops_recycled_labels() {
      let mut font = SystemFont::default();
      let spans = prop!("spans");
      let original = Value::Map(vec![
         ("text".into(), text("example")),
         ("font".into(), text("gg sans")),
         ("url".into(), text("https://example.org")),
         ("weight".into(), Value::I32(700)),
      ]);
      let special = [
         Value::Obj(vec![(prop!("font").key(), text("monospace"))]),
         Value::Map(vec![("font".into(), text("aiglyphs"))]),
         Value::Map(vec![("code".into(), Value::Bool(true))]),
         Value::Map(vec![("image".into(), Value::Map(Vec::new()))]),
         Value::Obj(vec![(prop!("text").key(), text("inherits title weight"))]),
      ];
      let mut items = vec![original.clone()];
      items.extend_from_slice(&special);
      let mut create = [Op::Create {
         id:    7,
         kind:  ViewKind::Text,
         props: vec![(spans, Value::Arr(items))],
      }];
      assert!(!font.observe(&mut create));
      let first = font.set_owner("reader", true);
      let [Op::Update { id: 7, props }] = first.as_slice() else {
         panic!("missing update");
      };
      assert_eq!(get_prop(props, Prop::FONT_FAMILY), Some(&system_family()));
      let Some(Value::Arr(runs)) = get_prop(props, spans) else {
         panic!("missing runs");
      };
      assert_eq!(
         record_field(&runs[0], prop!("weight")),
         Some(&Value::I32(700))
      );
      assert_eq!(
         record_field(&runs[0], prop!("url")),
         record_field(&original, prop!("url"))
      );
      assert_eq!(
         record_field(&runs[0], prop!("font")),
         Some(&system_family())
      );
      assert_eq!(runs[1..], special);
      assert_eq!(font.set_owner("second", true), [] as [Op; 0]);
      let mut latest = original;
      set_record_field(&mut latest, Prop::COLOR, Value::I32(42));
      let replacement = Value::Arr(vec![latest]);
      let mut update = [Op::Update {
         id:    7,
         props: vec![(spans, replacement.clone())],
      }];
      assert!(font.observe(&mut update));
      assert_eq!(font.set_owner("reader", false), [] as [Op; 0]);
      let restore = font.set_owner("second", false);
      let [Op::Update { id: 7, props }] = restore.as_slice() else {
         panic!("missing restore");
      };
      assert_eq!(get_prop(props, spans), Some(&replacement));
      assert_eq!(get_prop(props, Prop::FONT_FAMILY), Some(&Value::Null));
      font.observe(&mut [Op::Create {
         id:    7,
         kind:  ViewKind::Image,
         props: vec![],
      }]);
      assert_eq!(font.set_owner("reader", true), [] as [Op; 0]);
   }

   #[test]
   fn new_inputs_inherit_the_system_face_and_keep_their_text() {
      let mut font = SystemFont::default();
      font.set_owner("reader", true);
      let run = Value::Obj(vec![
         (prop!("weight").key(), Value::I32(700)),
         (prop!("text").key(), text("hello")),
      ]);
      let mut ops = [Op::Create {
         id:    3,
         kind:  ViewKind::Input,
         props: vec![
            (prop!("valueSpans"), Value::Arr(vec![run])),
            (Prop::VALUE, text("hello")),
         ],
      }];
      assert!(font.observe(&mut ops));
      let Op::Create { props, .. } = &ops[0] else {
         unreachable!()
      };
      assert_eq!(get_prop(props, Prop::VALUE), Some(&text("hello")));
      let Some(Value::Arr(runs)) = get_prop(props, prop!("valueSpans")) else {
         panic!("missing runs");
      };
      assert_eq!(
         record_field(&runs[0], prop!("font")),
         Some(&system_family())
      );
      assert_eq!(get_prop(props, Prop::FONT_FAMILY), Some(&system_family()));
      font.observe(&mut [Op::Delete { ids: vec![3] }]);
      assert_eq!(font.set_owner("reader", false), [] as [Op; 0]);
   }
}
