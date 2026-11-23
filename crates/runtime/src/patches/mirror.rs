use std::collections::{
   HashMap,
   HashSet,
};

use phoenix_ui::{
   ChildEdit,
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
};
use phoenix_wire::Value;

const FIRST: Prop = Prop::named("first");

/// Rust's view tree as its batches describe it: who holds whom, and the
/// accessibility id each node carries.
#[derive(Default)]
pub struct Mirror {
   root:         Option<i32>,
   kids:         HashMap<i32, Vec<i32>>,
   parent:       HashMap<i32, i32>,
   ids:          HashMap<i32, String>,
   kinds:        HashMap<i32, String>,
   uris:         HashMap<i32, String>,
   sheets:       HashSet<i32>,
   navigators:   Vec<i32>,
   stack_header: Option<Props>,
   /// The list index of each virtualized list's first mounted row.
   firsts:       HashMap<i32, i32>,
   /// Original `on` event list of each virtualized/row-gesture list.
   list_on:      HashMap<i32, Vec<Value>>,
   /// The message box inputs and their text, newest last.
   composer:     Vec<(i32, String)>,
}

/// What one batch created, by node.
pub type Created<'ops> = HashMap<i32, (&'ops ViewKind, &'ops Props)>;

impl Mirror {
   /// Applies `ops` and returns the nodes they created, plus the nodes they
   /// deleted.
   pub fn apply<'ops>(&mut self, ops: &'ops [Op]) -> (Created<'ops>, Vec<i32>) {
      let mut created = HashMap::new();
      let mut deleted = Vec::new();
      for op in ops {
         match op {
            Op::Create { id, kind, props } => {
               created.insert(*id, (kind, props));
               let is_list = prop(props, FIRST).is_some();
               if let Some(Value::I32(first)) = prop(props, FIRST) {
                  self.firsts.insert(*id, *first);
               }
               if let Some(Value::Str(uri)) = prop(props, Prop::URI) {
                  self.uris.insert(*id, uri.clone());
               }
               if let Some(Value::Arr(on)) = prop(props, Prop::ON)
                  && (is_list
                     || on
                        .iter()
                        .any(|value| matches!(value, Value::Str(name) if name == "rowLongPress")))
               {
                  self.list_on.insert(*id, on.clone());
               }
               if let Some(Value::Str(aid)) = prop(props, Prop::ACCESSIBILITY_ID) {
                  if aid == "composer" && !self.composer.iter().any(|(input, _)| input == id) {
                     self.composer.push((*id, String::new()));
                  }
                  self.ids.insert(*id, aid.clone());
               }
               self.kinds.insert(*id, kind.as_str().to_owned());
               if kind.as_str() == "sheet" {
                  self.sheets.insert(*id);
               }
               if *kind == ViewKind::Navigator && !self.navigators.contains(id) {
                  self.navigators.push(*id);
               }
               if prop(props, Prop::STACK_HEADER).is_some() {
                  self.stack_header = Some(props.clone());
               }
            },
            Op::Children { id, ids } => {
               for kid in ids {
                  self.parent.insert(*kid, *id);
               }
               self.kids.insert(*id, ids.clone());
            },
            Op::ChildEdits { id, edits } => {
               let old = self.kids.remove(id).unwrap_or_default();
               let new = apply_edits(&old, edits);
               for kid in edits.iter().flat_map(|edit| &edit.ins) {
                  self.parent.insert(*kid, *id);
               }
               self.kids.insert(*id, new);
            },
            Op::Delete { ids } => {
               for id in ids {
                  if self.root == Some(*id) {
                     self.root = None;
                  }
                  self.kids.remove(id);
                  self.parent.remove(id);
                  self.ids.remove(id);
                  self.kinds.remove(id);
                  self.uris.remove(id);
                  self.sheets.remove(id);
                   self.navigators.retain(|nav| nav != id);
                   self.firsts.remove(id);
                   self.list_on.remove(id);
                   self.composer.retain(|(input, _)| input != id);
                }
                deleted.extend_from_slice(ids);
             },
             Op::Root { id } => {
                self.root = Some(*id);
             },
             Op::Update { id, props } => {
                if let Some(Value::I32(first)) = prop(props, FIRST) {
                   self.firsts.insert(*id, *first);
                }
                if let Some(Value::Str(uri)) = prop(props, Prop::URI) {
                   self.uris.insert(*id, uri.clone());
                }
                match prop(props, Prop::ACCESSIBILITY_ID) {
                   Some(Value::Str(aid)) => {
                      if aid == "composer" && !self.composer.iter().any(|(input, _)| input == id) {
                         self.composer.push((*id, String::new()));
                      }
                      self.ids.insert(*id, aid.clone());
                   },
                   Some(Value::Null) => {
                      self.ids.remove(id);
                   },
                   _ => {},
                }
                if (self.firsts.contains_key(id) || self.list_on.contains_key(id))
                   && let Some(Value::Arr(on)) = prop(props, Prop::ON)
                {
                   self.list_on.insert(*id, on.clone());
                }
                if let Some(Value::Str(text)) = prop(props, Prop::VALUE) {
                   self.set_composer_text(*id, text.clone());
                }
             },
            _ => {},
         }
      }
      (created, deleted)
   }

   /// The row at list index `index` of the virtualized list `id`.
   pub fn row(&self, id: i32, index: i32) -> Option<i32> {
      let offset = index - self.firsts.get(&id).copied().unwrap_or(0);
      self.kids(id).get(usize::try_from(offset).ok()?).copied()
   }

   pub fn is_row_list(&self, id: i32) -> bool {
      self.list_on.contains_key(&id) || self.firsts.contains_key(&id)
   }

   pub fn list_on(&self, id: i32) -> &[Value] {
      self.list_on.get(&id).map_or(&[], Vec::as_slice)
   }

   pub fn kids(&self, id: i32) -> &[i32] {
      self.kids.get(&id).map_or(&[], Vec::as_slice)
   }

   /// The root node set by `Op::Root`, or the top ancestor of a live navigator.
   pub fn root(&self) -> Option<i32> {
      if let Some(root) = self.root
         && self.has_kids(root)
      {
         return Some(root);
      }
      let mut cur = *self.navigators.first()?;
      for _ in 0..64 {
         let Some(parent) = self.parent(cur) else {
            break;
         };
         cur = parent;
      }
      self.has_kids(cur).then_some(cur)
   }

   pub fn has_kids(&self, id: i32) -> bool {
      self.kids.contains_key(&id)
   }

   pub fn parent(&self, id: i32) -> Option<i32> {
      self.parent.get(&id).copied()
   }

   pub fn accessibility_id(&self, id: i32) -> Option<&str> {
      self.ids.get(&id).map(String::as_str)
   }

   /// The kind of view `id` is, if it is still mounted.
   pub fn kind(&self, id: i32) -> Option<&str> {
      self.kinds.get(&id).map(String::as_str)
   }

   /// The node with accessibility id `aid`.
   pub fn find_aid(&self, aid: &str) -> Option<i32> {
      self
         .ids
         .iter()
         .find_map(|(id, name)| (name == aid).then_some(*id))
   }

   /// Whether `id` is a sheet.
   pub fn is_sheet(&self, id: i32) -> bool {
      self.sheets.contains(&id)
   }

   /// The image URI of `id`, if it has one.
   pub fn uri(&self, id: i32) -> Option<&str> {
      self.uris.get(&id).map(String::as_str)
   }

   /// Every live node whose accessibility id equals `aid`.
   pub fn nodes_with_id(&self, aid: &str) -> Vec<i32> {
      self
         .ids
         .iter()
         .filter_map(|(id, existing)| (existing == aid).then_some(*id))
         .collect()
   }

   /// Every live node whose accessibility id passes `test`.
   pub fn nodes_where(&self, test: &impl Fn(&str) -> bool) -> Vec<i32> {
      self
         .ids
         .iter()
         .filter_map(|(id, existing)| test(existing).then_some(*id))
         .collect()
   }

   /// Walks up from `id` to its enclosing virtualized/row-gesture list, if any.
   pub fn enclosing_list(&self, mut id: i32) -> Option<i32> {
      for _ in 0..16 {
         let parent = self.parent(id)?;
         if self.is_row_list(parent) {
            return Some(parent);
         }
         id = parent;
      }
      None
   }

   /// Whether `id` is currently tracked in the mirror.
   pub fn exists(&self, id: i32) -> bool {
      self.parent.contains_key(&id)
         || self.kids.contains_key(&id)
         || self.ids.contains_key(&id)
         || self.sheets.contains(&id)
         || self.firsts.contains_key(&id)
   }

   /// The first node at or under `id`, depth first, whose accessibility id
   /// passes `test`.
   pub fn find_down(&self, id: i32, test: &impl Fn(&str) -> bool, depth: usize) -> Option<i32> {
      if self.accessibility_id(id).is_some_and(test) {
         return Some(id);
      }
      if depth == 0 {
         return None;
      }
      self
         .kids(id)
         .iter()
         .find_map(|kid| self.find_down(*kid, test, depth - 1))
   }

   /// The sheet created last, which is on top.
   pub fn newest_sheet(&self) -> Option<i32> {
      self.sheets.iter().max().copied()
   }

   /// The outermost live `Navigator` (not inside a sheet) that currently holds
   /// pages — Phoenix's root navigator above `main-tabs` and chat.
   pub fn active_navigator(&self) -> Option<i32> {
      self
         .navigators
         .iter()
         .copied()
         .filter(|id| !self.kids(*id).is_empty() && self.sheet_of(*id).is_none())
         .min_by_key(|id| {
            let mut depth = 0usize;
            let mut cur = self.parent(*id);
            while let Some(parent) = cur {
               depth += 1;
               cur = self.parent(parent);
               if depth > 64 {
                  break;
               }
            }
            (depth, *id)
         })
   }

   /// Header padding/height from Phoenix's own stack header, if seen.
   pub fn stack_header(&self) -> Option<&Props> {
      self.stack_header.as_ref()
   }

   /// The sheet holding `id`, if any.
   pub fn sheet_of(&self, id: i32) -> Option<i32> {
      let mut node = self.parent(id);
      for _ in 0..40 {
         let current = node?;
         if self.sheets.contains(&current) {
            return Some(current);
         }
         node = self.parent(current);
      }
      None
   }

   pub fn composer(&self) -> Option<(i32, &str)> {
      self
         .composer
         .last()
         .map(|(input, text)| (*input, text.as_str()))
   }

   pub fn is_composer(&self, id: i32) -> bool {
      self.composer.iter().any(|(input, _)| *input == id)
   }

   pub fn set_composer_text(&mut self, id: i32, text: String) {
      if let Some((_, current)) = self.composer.iter_mut().find(|(input, _)| *input == id) {
         *current = text;
      }
   }

   /// `id` and what this batch created under it, as a template.
   pub fn snapshot(&self, id: i32, created: &Created<'_>) -> Option<Node> {
      let (kind, props) = created.get(&id)?;
      Some(Node {
         kind:  (*kind).clone(),
         props: (*props).clone(),
         kids:  self
            .kids(id)
            .iter()
            .filter_map(|kid| self.snapshot(*kid, created))
            .collect(),
         src:   None,
      })
   }
}

pub fn prop(props: &Props, key: Prop) -> Option<&Value> {
   props
      .iter()
      .find(|(existing, _)| *existing == key)
      .map(|(_, value)| value)
}

fn apply_edits(old: &[i32], edits: &[ChildEdit]) -> Vec<i32> {
   let mut out = Vec::with_capacity(old.len());
   let mut at = 0;
   for edit in edits {
      let keep = usize::try_from(edit.keep).unwrap_or_default();
      let del = usize::try_from(edit.del).unwrap_or_default();
      out.extend(old.iter().skip(at).take(keep));
      at = (at + keep + del).min(old.len());
      out.extend_from_slice(&edit.ins);
   }
   out.extend(old.iter().skip(at));
   out
}
