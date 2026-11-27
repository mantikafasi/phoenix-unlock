//! Sections, rows and text fields built from Phoenix's captured layouts.

use phoenix_ui_mobile::{
   Prop,
   Props,
   motion::Worklet,
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::settings::{
   Field,
   ICON_CHECK,
   ICON_FILTER,
   ICON_PLUGIN,
   Press,
   Slot,
   theme::template_props,
   tree::{
      Animate,
      Tree,
      text,
   },
};

const RADII: [Prop; 5] = [
   Prop::RADIUS,
   Prop::RADIUS_TOP_LEFT,
   Prop::RADIUS_TOP_RIGHT,
   Prop::RADIUS_BOTTOM_LEFT,
   Prop::RADIUS_BOTTOM_RIGHT,
];

pub fn is_radius(key: Prop) -> bool {
   RADII.contains(&key)
}

pub fn section(title: &str, rows: Vec<Tree>) -> Tree {
   let layouts = templates();
   let mut section = Tree::from_template(&layouts.section);
   let mut group = Tree::from_template(&layouts.group);
   let hidden = |row: &Tree| {
      row.props
         .iter()
         .any(|(key, value)| *key == prop!("hidden") && *value == Value::Bool(true))
   };
   let mut kids = Vec::new();
   let mut previous_hidden = true;
   for (index, row) in rows.into_iter().enumerate() {
      let row_hidden = hidden(&row);
      if index > 0 {
         // Filtered rows collapse in place; their adjoining separators must
         // too.
         let mut divider = Tree::from_template(&layouts.divider);
         let hidden = row_hidden || previous_hidden;
         divider.set(prop!("hidden"), Some(Value::Bool(hidden)));
         divider.set(prop!("accessibilityHidden"), Some(Value::Bool(hidden)));
         kids.push(divider);
      }
      kids.push(row);
      previous_hidden = row_hidden;
   }
   group.kids = kids;
   if !title.is_empty() {
      let mut header = Tree::from_template(&layouts.header);
      header.set(Prop::TEXT, Some(text(title)));
      section.kids.push(header);
   }
   section.kids.push(group);
   section
}

pub fn grid_section(title: &str, cells: Vec<Tree>) -> Tree {
   let layouts = templates();
   let mut section = Tree::from_template(&layouts.section);
   let mut group = Tree::from_template(&layouts.group);
   group.set(Prop::GAP, Some(Value::F64(8.0)));
   let mut cells = cells.into_iter();
   while let Some(first) = cells.next() {
      let mut row = Tree::from_template(&layouts.group);
      row.set(prop!("direction"), Some(text("row")));
      row.set(Prop::RADIUS, None);
      row.set(prop!("clip"), None);
      row.set(Prop::GAP, Some(Value::F64(8.0)));
      row.kids = vec![first];
      if let Some(second) = cells.next() {
         row.kids.push(second);
      } else {
         let mut spacer = Tree::from_template(&layouts.group);
         spacer.set(prop!("width"), None);
         spacer.set(prop!("flex"), Some(Value::F64(1.0)));
         spacer.set(Prop::RADIUS, None);
         spacer.set(prop!("clip"), None);
         row.kids.push(spacer);
      }
      group.kids.push(row);
   }
   if !title.is_empty() {
      let mut header = Tree::from_template(&layouts.header);
      header.set(Prop::TEXT, Some(text(title)));
      section.kids.push(header);
   }
   section.kids.push(group);
   section
}

pub fn grid_cell(mut row: Tree) -> Tree {
   row.set(prop!("width"), None);
   row.set(prop!("flex"), Some(Value::F64(1.0)));
   row.set(Prop::RADIUS, Some(Value::F64(12.0)));
   if let Some(inner) = row.kids.first_mut() {
      inner.kids.remove(0);
      inner.set(Prop::PADDING, Some(Value::Rect([8.0, 12.0, 8.0, 12.0])));
      inner.set(prop!("minHeight"), Some(Value::F64(64.0)));
      if let Some(title) = inner
         .kids
         .first_mut()
         .and_then(|column| column.kids.first_mut())
      {
         title.set(prop!("maxLines"), Some(Value::I32(2)));
         title.set(Prop::FONT_SIZE, Some(Value::F64(14.0)));
         // Let the host wrap plugin slugs at hyphens instead of mid-word.
         if let Some((_, Value::Str(name))) =
            title.props.iter_mut().find(|(key, _)| *key == Prop::TEXT)
         {
            *name = name.replace('-', "-\u{200b}");
         }
      }
      if let Some(toggle) = inner.kids.get_mut(1) {
         toggle.set(Prop::MARGIN, Some(Value::Rect([8.0, 0.0, 0.0, 0.0])));
      }
   }
   row
}

/// `root > row > [icon, text, chevron]`
pub fn button_row(title: &str, icon: &str, press: Option<Press>, chevron: bool) -> Tree {
   let mut root = Tree::from_template(&templates().button_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(title)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(icon_node) = row.kids.get_mut(0) {
         icon_node.set(Prop::URI, Some(text(icon)));
      }
      if let Some(label) = row.kids.get_mut(1) {
         label.set(Prop::TEXT, Some(text(title)));
      }
      if !chevron {
         row.kids.truncate(2);
      }
   }
   if press.is_none() {
      root.set(Prop::ON, None);
      root.set(Prop::ROLE, None);
   }
   root.press = press;
   root
}

/// A button row without an icon, with a checkmark in the chevron's place when
/// `selected`.
pub fn select_row(label: &str, selected: bool, press: Press) -> Tree {
   let mut root = button_row(label, ICON_PLUGIN, Some(press), selected);
   if let Some(row) = root.kids.first_mut() {
      row.kids.remove(0);
      if let Some(check) = row.kids.get_mut(1).and_then(|wrap| wrap.kids.first_mut()) {
         check.set(Prop::URI, Some(text(ICON_CHECK)));
      }
   }
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(selected)));
   root
}

/// `root > row > [label, value wrap > value, chevron]`
pub fn value_row(label: &str, value: &str) -> Tree {
   let mut root = Tree::from_template(&templates().value_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ON, None);
   root.set(Prop::ROLE, None);
   root.set(
      Prop::ACCESSIBILITY_LABEL,
      Some(text(&format!("{label}, {value}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(label_node) = row.kids.get_mut(0) {
         label_node.set(Prop::TEXT, Some(text(label)));
      }
      if let Some(value_node) = row
         .kids
         .get_mut(1)
         .and_then(|wrap| wrap.kids.get_mut(0))
         .and_then(|inner| inner.kids.get_mut(0))
      {
         value_node.set(Prop::TEXT, Some(text(value)));
      }
      row.kids.truncate(2);
   }
   root
}

/// `root > row > [icon, column > [title, subtitle], wrap > track > knob]`.
/// Pressing the row runs `press`, pressing the switch runs `toggle`.
pub fn switch_row(
   icon: Option<&str>,
   title: &str,
   subtitle: Option<&str>,
   on: bool,
   press: Press,
   toggle: Press,
) -> Tree {
   let layouts = templates();
   let mut root = Tree::from_template(&layouts.switch_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   let label = subtitle.map_or_else(
      || title.to_owned(),
      |subtitle| format!("{title}, {subtitle}"),
   );
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(&label)));
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(on)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   root.press = Some(press);
   let Some(row) = root.kids.first_mut() else {
      return root;
   };
   if let Some(icon_node) = row.kids.get_mut(0) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(column) = row.kids.get_mut(1) {
      if let Some(title_node) = column.kids.get_mut(0) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      match subtitle {
         Some(subtitle) => {
            if let Some(subtitle_node) = column.kids.get_mut(1) {
               subtitle_node.set(Prop::TEXT, Some(text(subtitle)));
            }
         },
         None => column.kids.truncate(1),
      }
   }
   if let Some(track) = row.kids.get_mut(2).and_then(|wrap| wrap.kids.get_mut(0)) {
      track.press = Some(toggle);
      animate(track, &layouts.track_worklet, &layouts.track_off, on, false);
      if let Some(knob) = track.kids.get_mut(0) {
         animate(knob, &layouts.knob_worklet, &layouts.knob_off, on, true);
      }
   }
   if icon.is_none() {
      row.kids.remove(0);
   }
   root
}

/// Gives a switch part built from the "on" template the props for `on` and a
/// worklet to spring between both states.
fn animate(node: &mut Tree, worklet: &'static Worklet, off: &Props, on: bool, knob: bool) {
   let mut off = off.clone();
   template_props(&mut off);
   if knob && crate::settings::theme::light() {
      for (key, value) in &mut off {
         if *key == prop!("background") && *value == Value::I64(4294967295) {
            *value = Value::I64(0xFF62_636D);
         }
      }
   }
   let other = if on {
      off
   } else {
      std::mem::replace(&mut node.props, off)
   };
   node.animate = Some(Animate {
      worklet,
      other,
      flipped: false,
   });
}

/// What a text field takes, which picks its keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
   Text,
   Number,
   Secret,
}

/// `root > frame > row > [icon wrap > icon, input]`, Phoenix's search box.
pub fn text_field(
   field: Field,
   icon: Option<&str>,
   placeholder: &str,
   value: &str,
   entry: InputKind,
) -> Tree {
   let mut root = Tree::from_template(&templates().input);
   let Some(row) = root
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
   else {
      return root;
   };
   if let Some(icon_node) = row.kids.first_mut().and_then(|wrap| wrap.kids.first_mut()) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(input) = row.kids.get_mut(1) {
      input.set(Prop::PLACEHOLDER, Some(text(placeholder)));
      input.set(Prop::VALUE, Some(text(value)));
      input.set(
         Prop::KEYBOARD,
         (entry == InputKind::Number).then(|| text("number")),
      );
      input.set(
         Prop::SECURE,
         (entry == InputKind::Secret).then_some(Value::Bool(true)),
      );
      input.set(Prop::RETURN_KEY, Some(text("go")));
      input.set(Prop::ROLE, None);
      input.set(
         Prop::ACCESSIBILITY_ID,
         Some(text(&format!("phoenix-{placeholder}"))),
      );
      input.slot = Some(Slot::Input(field));
   }
   if icon.is_none() {
      row.kids.remove(0);
      if let Some(input) = row.kids.first_mut() {
         input.set(Prop::MARGIN, None);
      }
   }
   root
}

/// Phoenix's search field beside a compact filter button, both inside a plain
/// horizontal container so the surrounding card does not get a nested card.
pub fn search_filter_row(filter: &str, press: Press) -> Tree {
   let layouts = templates();
   let mut row = Tree::from_template(&layouts.group);
   row.props.clear();
   row.set(prop!("direction"), Some(text("row")));
   row.set(prop!("width"), Some(text("fill")));
   row.set(prop!("align"), Some(text("center")));
   row.set(Prop::GAP, Some(Value::F64(8.0)));

   let mut search = text_field(
      Field::Search,
      Some("res://ic_mana_magnifying_glass"),
      "Search plugins",
      "",
      InputKind::Text,
   );
   search.set(prop!("width"), None);
   search.set(prop!("flex"), Some(Value::F64(1.0)));

   let mut button = button_row(filter, ICON_FILTER, Some(press), false);
   button.set(prop!("width"), None);
   button.set(Prop::RADIUS, Some(Value::F64(8.0)));
   if let Some(inner) = button.kids.first_mut() {
      inner.set(prop!("width"), None);
      inner.set(Prop::PADDING, Some(Value::Rect([8.0, 10.0, 8.0, 10.0])));
      inner.set(prop!("minHeight"), Some(Value::F64(40.0)));
      if let Some(icon) = inner.kids.get_mut(0) {
         icon.set(prop!("width"), Some(Value::F64(18.0)));
         icon.set(Prop::HEIGHT, Some(Value::F64(18.0)));
         icon.set(Prop::MARGIN, Some(Value::Rect([2.0, 2.0, 8.0, 2.0])));
      }
      if let Some(label) = inner.kids.get_mut(1) {
         label.set(prop!("flex"), None);
         label.set(Prop::FONT_SIZE, Some(Value::F64(14.0)));
         label.set(prop!("maxLines"), Some(Value::I32(1)));
      }
   }
   row.kids = vec![search, button];
   row
}
