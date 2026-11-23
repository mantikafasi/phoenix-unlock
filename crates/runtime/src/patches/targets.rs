use phoenix_ui::{
   Node,
   Prop,
   Props,
   ViewKind,
};
use phoenix_wire::Value;

use crate::patches::{
   Contribution,
   Draft,
   Handler,
   Outcome,
   Place,
   Rendered,
   badges,
   mirror::{
      self,
      Created,
      Mirror,
   },
   tabs,
};

const DANGER: i64 = 4_294_475_386;
const MUTED: i64 = 4_288_059_294;
const DEFAULT_ICON: &str = "res://ic_mana_puzzle_piece_plus";
const ROW_TEMPLATES: [&str; 6] = [
   "message.actions.reply",
   "message.actions.copy_text",
   "message.actions.copy_id",
   "message.actions.mark_unread",
   "message.actions.copy_link",
   "message.actions.mention",
];

/// Where a contribution goes, next to one of Phoenix's own nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
   /// The chat header, left of search.
   Header,
   /// The message box, left of the emoji button.
   Composer,
   /// Under a message's content.
   Content,
   /// After a message's timestamp.
   Timestamp,
   /// The message long-press menu, above Delete.
   Action,
   /// The end of a profile's main tab.
   Profile,
   /// A tab after a profile's own tabs. Plugins add these; the page that goes
   /// with each is drawn at [`Target::TabPage`].
   Tab,
   /// The pages of profile tabs, after the profile's own pages.
   TabPage,
   /// The badge row on a profile.
   ProfileBadges,
   /// The badge grid in the badge directory.
   BadgeDirectory,
}

impl Target {
   pub const ALL: [Self; 10] = [
      Self::Header,
      Self::Composer,
      Self::Content,
      Self::Timestamp,
      Self::Action,
      Self::Profile,
      Self::Tab,
      Self::TabPage,
      Self::ProfileBadges,
      Self::BadgeDirectory,
   ];

   /// The target whose contributions this one draws.
   pub const fn drawn_by(self) -> Self {
      match self {
         Self::TabPage => Self::Tab,
         Self::BadgeDirectory => Self::ProfileBadges,
         other => other,
      }
   }

   pub fn matches(self, aid: &str) -> bool {
      match self {
         Self::Header => aid == "search.entry",
         Self::Composer => aid == "composer.expressions",
         Self::Content => aid.starts_with("message.content:"),
         Self::Timestamp => aid.starts_with("message.timestamp:"),
         Self::Action => aid == "message.actions.root",
         Self::Profile | Self::Tab | Self::TabPage => aid == "profile-tab-pages",
         Self::ProfileBadges => false,
         Self::BadgeDirectory => aid == "badge-directory-title",
      }
   }

   /// Where this target's nodes go for the anchor `id`.
   pub fn place(self, mirror: &Mirror, id: i32, aid: &str, seen: &Seen) -> Option<Placement> {
      match self {
         Self::Header | Self::Composer => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: Some(id),
               place:  Place::Screen,
            })
         },
         Self::Content => {
            Some(Placement {
               parent: id,
               before: None,
               place:  Place::Message(aid.strip_prefix("message.content:")?.to_owned()),
            })
         },
         Self::Timestamp => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: None,
               place:  Place::Message(aid.strip_prefix("message.timestamp:")?.to_owned()),
            })
         },
         Self::Action => {
            let groups = *mirror.kids(id).last()?;
            let delete = mirror.kids(groups).iter().copied().find(|group| {
               mirror
                  .find_down(*group, &|aid| aid == "message.actions.delete", 2)
                  .is_some()
            });
            Some(Placement {
               parent: groups,
               before: delete,
               place:  seen
                  .long_pressed
                  .clone()
                  .map_or(Place::Screen, Place::Message),
            })
         },
         Self::Profile => {
            // Phoenix nests the sections six levels under the pages row.
            let mut node = id;
            for _ in 0..6 {
               node = *mirror.kids(node).first()?;
            }
            Some(Placement {
               parent: node,
               before: None,
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::Tab => {
            let (row, indicator) = tab_row(mirror, id)?;
            Some(Placement {
               parent: row,
               before: Some(indicator),
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::TabPage => {
            Some(Placement {
               parent: *mirror.kids(id).first()?,
               before: None,
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::ProfileBadges => badges::place_row(mirror, id, seen.badge_user.clone()),
         Self::BadgeDirectory => badges::place_directory(mirror, id, seen.badge_user.clone()),
      }
   }

   /// Whether drawing this target needs a plugin's `render` or `visible`.
   pub const fn evaluates(self) -> bool {
      matches!(
         self,
         Self::Content
            | Self::Timestamp
            | Self::Profile
            | Self::TabPage
            | Self::ProfileBadges
            | Self::BadgeDirectory
      )
   }
}

/// The row holding the tabs of the profile whose pages are `pager`, and its
/// last child, the sliding indicator.
///
/// `column > [tabs > stack > column > [strip > row > [tab…, indicator], divider],
/// frame > pager]`
pub fn tab_row(mirror: &Mirror, pager: i32) -> Option<(i32, i32)> {
   let frame = mirror.parent(pager)?;
   let siblings = mirror.kids(mirror.parent(frame)?);
   let at = siblings.iter().position(|sibling| *sibling == frame)?;
   let mut node = *siblings.get(at.checked_sub(1)?)?;
   for _ in 0..4 {
      node = *mirror.kids(node).first()?;
   }
   let kids = mirror.kids(node);
   if kids.len() < 2 {
      return None;
   }
   Some((node, *kids.last()?))
}

/// What the dispatches and batches showed about the screen.
#[derive(Default)]
pub struct Seen {
   pub long_pressed: Option<String>,
   pub profile_user: Option<String>,
   pub badge_user:   Option<String>,
}

pub struct Placement {
   pub parent: i32,
   /// The sibling the nodes go before, or the end.
   pub before: Option<i32>,
   pub place:  Place,
}

/// Phoenix's own nodes, snapshotted when they appear, so plugin nodes look
/// like their neighbours.
#[derive(Default)]
pub struct Templates {
   header:    Option<Node>,
   composer:  Option<Node>,
   timestamp: Option<Node>,
   row:       Option<Node>,
   group:     Option<(ViewKind, Props)>,
   divider:   Option<Node>,
   profile:   Option<Node>,
   /// A profile tab that is not selected, and the page width.
   tab:       Option<Node>,
   /// A tile of the badge directory's grid.
   badge_tile: Option<Node>,
   page_width: Option<Value>,
   /// The first text color on Phoenix's first page.
   page_text: Option<i64>,
   /// The selected tab's text color.
   tab_color: Option<i64>,
   /// The message long-press sheet from its outer view down to the node
   /// holding its groups, without animation or ids, for plugin menus.
   sheet:     Option<Vec<(ViewKind, Props)>>,
   /// The icons of the message menu's rows, by their accessibility id.
   icons:     std::collections::HashMap<String, String>,
}

/// A row of a menu sheet.
pub(super) struct MenuRow {
   pub(super) label:  String,
   pub(super) icon:   Option<String>,
   pub(super) danger: bool,
   pub(super) press:  Handler,
   pub(super) id:     Option<String>,
}

/// Props that tie a node to Phoenix's own animations or to its identity.
const BOUND: [&str; 5] = ["viewMotion", "worklet", "portalSlot", "accessibilityId", "accessibilityLabel"];

impl Templates {
   /// Learns the directory tile when its grid arrives after the title.
   pub(super) fn learn_badge_tile(&mut self, mirror: &Mirror, created: &Created<'_>) {
      badges::learn_tile(&mut self.badge_tile, mirror, None, created);
   }

   pub fn learn(&mut self, mirror: &Mirror, id: i32, aid: &str, created: &Created<'_>) {
      match aid {
         "search.entry" => self.header = mirror.snapshot(id, created),
         "composer.expressions" => self.composer = mirror.snapshot(id, created),
         "badge-directory-title" => {
            badges::learn_tile(&mut self.badge_tile, mirror, Some(id), created);
         },
         "profile-tab-pages" => {
            self.learn_profile(mirror, id, created);
            self.learn_tabs(mirror, id, created);
         },
         _ if aid.starts_with("message.timestamp:") && self.timestamp.is_none() => {
            self.timestamp = mirror.snapshot(id, created);
         },
         "message.actions.root" => self.learn_sheet(mirror, id, created),
         _ if aid.starts_with("message.actions.") => {
            if ROW_TEMPLATES.contains(&aid) {
               self.learn_row(mirror, id, created);
            }
            if let Some(icon) = mirror.snapshot(id, created).as_ref().and_then(first_image) {
               self.icons.insert(aid.to_owned(), icon);
            }
         },
         _ => {},
      }
   }

   fn learn_row(&mut self, mirror: &Mirror, id: i32, created: &Created<'_>) {
      let Some(row) = mirror.snapshot(id, created) else {
         return;
      };
      if row.kids.first().is_none_or(|inner| inner.kids.len() < 2) {
         return;
      }
      let group = mirror.parent(id);
      let siblings = group.map_or(&[][..], |group| mirror.kids(group));
      let next = siblings
         .iter()
         .position(|sibling| *sibling == id)
         .and_then(|at| siblings.get(at + 1))
         .copied();
      if let Some(divider) = next
         .filter(|next| mirror.accessibility_id(*next).is_none())
         .and_then(|next| mirror.snapshot(next, created))
      {
         self.divider = Some(divider);
      }
      if let Some((kind, props)) = group.and_then(|group| created.get(&group)) {
         self.group = Some(((*kind).clone(), (*props).clone()));
      }
      self.row = Some(row);
   }

   fn learn_sheet(&mut self, mirror: &Mirror, root: i32, created: &Created<'_>) {
      let Some(&groups) = mirror.kids(root).last() else {
         return;
      };
      let Some(sheet) = mirror.sheet_of(root) else {
         return;
      };
      let top = mirror.parent(sheet).unwrap_or(sheet);
      let mut path = vec![groups];
      let mut node = groups;
      while node != top && path.len() < 24 {
         let Some(parent) = mirror.parent(node) else {
            return;
         };
         path.push(parent);
         node = parent;
      }
      path.reverse();
      let chain = path
         .iter()
         .map(|node| {
            let (kind, props) = created.get(node)?;
            let props = props
               .iter()
               .filter(|(key, _)| !key.name().is_some_and(|name| BOUND.contains(&name)))
               .cloned()
               .collect();
            Some(((*kind).clone(), props))
         })
         .collect::<Option<Vec<_>>>();
      if chain.is_some() {
         self.sheet = chain;
      }
   }

   /// A menu sheet like the message long-press one: `title` over `rows`.
   /// `None` until that sheet was seen.
   pub(super) fn menu(&self, title: Option<&str>, rows: Vec<MenuRow>, dismiss: Handler) -> Option<Draft> {
      let chain = self.sheet.as_ref()?;
      let mut kids = Vec::new();
      if let Some(title) = title
         && let Some(row) = &self.row
         && let Some(sample) = row.kids.first().and_then(|inner| inner.kids.get(1))
      {
         let mut head = text(&sample.props, &Rendered {
            text:  title.to_owned(),
            color: None,
            tint:  None,
         });
         head.set(Prop::MARGIN, Some(Value::Rect([4.0, 0.0, 4.0, 8.0])));
         head.set(Prop::named("fontWeight"), Some(Value::I32(700)));
         kids.push(head);
      }
      kids.extend(self.group(rows));
      let mut node: Option<Draft> = None;
      for (kind, props) in chain.iter().rev() {
         let mut draft = Draft {
            kind:  kind.clone(),
            props: props.clone(),
            kids:  node.take().map_or_else(|| std::mem::take(&mut kids), |inner| vec![inner]),
            press: None,
         };
         if kind.as_str() == "sheet" {
            draft.press = Some(dismiss.clone());
         }
         node = Some(draft);
      }
      node
   }

   fn learn_tabs(&mut self, mirror: &Mirror, pager: i32, created: &Created<'_>) {
      let page = mirror.kids(pager).first().and_then(|row| mirror.kids(*row).first());
      if let Some(width) = page
         .and_then(|page| created.get(page))
         .and_then(|(_, props)| mirror::prop(props, Prop::named("width")))
      {
         self.page_width = Some(width.clone());
      }
      if let Some(page) = page.and_then(|page| mirror.snapshot(*page, created)) {
         self.page_text = first_text_color(&page).or(self.page_text);
      }
      let Some((row, _)) = tab_row(mirror, pager) else {
         return;
      };
      let tabs = mirror.kids(row);
      let color = |tab: &Node| {
         match tab.kids.first()?.kids.first().and_then(|text| mirror::prop(&text.props, Prop::COLOR)) {
            Some(Value::I64(color)) => Some(*color),
            Some(Value::I32(color)) => Some(i64::from(*color)),
            _ => None,
         }
      };
      for tab in &tabs[..tabs.len() - 1] {
         let Some(node) = mirror.snapshot(*tab, created) else {
            continue;
         };
         let selected = mirror::prop(&node.props, Prop::named("accessibilitySelected"));
         if matches!(selected, Some(Value::Bool(true))) {
            self.tab_color = color(&node).or(self.tab_color);
         } else if node.kids.first().is_some_and(|inner| !inner.kids.is_empty()) {
            self.tab = Some(node);
         }
      }
   }

   fn learn_profile(&mut self, mirror: &Mirror, id: i32, created: &Created<'_>) {
      let seen = Seen::default();
      let Some(placement) = Target::Profile.place(mirror, id, "", &seen) else {
         return;
      };
      let first = mirror
         .kids(placement.parent)
         .first()
         .and_then(|section| mirror.kids(*section).first());
      let Some(section) = first.and_then(|first| mirror.snapshot(*first, created)) else {
         return;
      };
      let shaped = section
         .kids
         .first()
         .is_some_and(|head| head.kind == ViewKind::Text)
         && section
            .kids
            .get(1)
            .and_then(|body| body.kids.first())
            .is_some_and(|text| text.kind == ViewKind::Text);
      if shaped {
         self.profile = Some(section);
      }
   }

   /// The nodes `contributions` draw at `place`. `outcome` answers what
   /// plugins already evaluated there, `None` while it is pending.
   pub fn draw(
      &self,
      target: Target,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      match target {
         Target::Header => Self::icon_buttons(self.header.as_ref(), contributions, place),
         Target::Composer => Self::icon_buttons(self.composer.as_ref(), contributions, place),
         Target::Content | Target::Timestamp => {
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  // A tint alone draws no line.
                  if rendered.text.is_empty() {
                     return None;
                  }
                  let mut node = text(&self.accessory_props(), &rendered);
                  if target == Target::Content {
                     node.set(Prop::MARGIN, Some(Value::Rect([0.0, 2.0, 0.0, 0.0])));
                  }
                  Some(node)
               })
               .collect()
         },
         Target::Action => self.action_group(contributions, place, outcome),
         Target::Tab => {
            let Some(tab) = &self.tab else {
               return Vec::new();
            };
            contributions
               .iter()
               .map(|contribution| tabs::tab(tab, contribution))
               .collect()
         },
         Target::TabPage => {
            contributions
               .iter()
               .map(|contribution| {
                  tabs::page(self.page_width.clone(), self.palette(), contribution, outcome(contribution))
               })
               .collect()
         },
         Target::Profile => {
            let Some(section) = &self.profile else {
               return Vec::new();
            };
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  let mut node = Draft::from_node(section);
                  let mut head = text(&section.kids[0].props, &Rendered {
                     text:  contribution.label.clone(),
                     color: None,
                     tint:  None,
                  });
                  head.set(
                     Prop::ACCESSIBILITY_LABEL,
                     Some(Value::Str(contribution.label.clone())),
                  );
                  let body = text(&section.kids[1].kids[0].props, &rendered);
                  node.kids.truncate(2);
                  node.kids[0] = head;
                  node.kids[1].kids = vec![body];
                  Some(node)
               })
               .collect()
         },
         Target::ProfileBadges => badges::draw_row(contributions, outcome),
         Target::BadgeDirectory => match &self.badge_tile {
            Some(tile) => badges::draw_directory(tile, contributions, outcome),
            None => Vec::new(),
         },
      }
   }

   fn icon_buttons(
      template: Option<&Node>,
      contributions: &[&Contribution],
      place: &Place,
   ) -> Vec<Draft> {
      let Some(template) = template else {
         return Vec::new();
      };
      contributions
         .iter()
         .map(|contribution| {
            let mut button = Draft::from_node(template);
            button.set(
               Prop::ACCESSIBILITY_ID,
               Some(Value::Str(format!(
                  "phoenix.{}.{}",
                  contribution.plugin, contribution.id.0
               ))),
            );
            button.set(
               Prop::ACCESSIBILITY_LABEL,
               Some(Value::Str(contribution.label.clone())),
            );
            if let (Some(icon), Some(image)) = (&contribution.icon, button.kids.first_mut()) {
               image.set(Prop::URI, Some(Value::Str(icon.clone())));
            }
            button.press = Some(Handler::Press {
               contribution: contribution.id,
               place:        place.clone(),
               dismiss:      false,
            });
            button
         })
         .collect()
   }

   fn action_group(
      &self,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      let rows = contributions
         .iter()
         .filter(|contribution| {
            !contribution.conditional || matches!(outcome(contribution), Some(Outcome::Shown(true)))
         })
         .map(|contribution| {
            MenuRow {
               label:  contribution.label.clone(),
               icon:   contribution.icon.clone(),
               danger: contribution.danger,
               press:  Handler::Press {
                  contribution: contribution.id,
                  place:        place.clone(),
                  dismiss:      true,
               },
               id:     Some(format!("phoenix.{}.{}", contribution.plugin, contribution.id.0)),
            }
         })
         .collect();
      self.group(rows)
   }

   /// `rows` in one rounded group, styled like the message menu's rows.
   fn group(&self, rows: Vec<MenuRow>) -> Vec<Draft> {
      let (Some(row), Some((group_kind, group_props))) = (&self.row, &self.group) else {
         return Vec::new();
      };
      let count = rows.len();
      let mut kids = Vec::new();
      for (index, entry) in rows.into_iter().enumerate() {
         let mut draft = Draft::from_node(row);
         for corner in [
            Prop::RADIUS,
            Prop::RADIUS_TOP_LEFT,
            Prop::RADIUS_TOP_RIGHT,
            Prop::RADIUS_BOTTOM_LEFT,
            Prop::RADIUS_BOTTOM_RIGHT,
         ] {
            draft.set(corner, None);
         }
         let round = Some(Value::F64(12.0));
         if count == 1 {
            draft.set(Prop::RADIUS, round);
         } else if index == 0 {
            draft.set(Prop::RADIUS_TOP_LEFT, round.clone());
            draft.set(Prop::RADIUS_TOP_RIGHT, round);
         } else if index == count - 1 {
            draft.set(Prop::RADIUS_BOTTOM_LEFT, round.clone());
            draft.set(Prop::RADIUS_BOTTOM_RIGHT, round);
         }
         draft.set(Prop::ACCESSIBILITY_ID, entry.id.map(Value::Str));
         draft.set(Prop::ACCESSIBILITY_LABEL, Some(Value::Str(entry.label.clone())));
         let inner = &mut draft.kids[0];
         // An icon without a scheme names a message menu row to borrow it from.
         let icon = match entry.icon.as_deref() {
            Some(icon) if !icon.contains("://") => self.icons.get(icon).map_or(DEFAULT_ICON, String::as_str),
            Some(icon) => icon,
            None => DEFAULT_ICON,
         };
         inner.kids[0].set(Prop::URI, Some(Value::Str(icon.to_owned())));
         inner.kids[1].set(Prop::TEXT, Some(Value::Str(entry.label.clone())));
         if entry.danger {
            inner.kids[1].set(Prop::COLOR, Some(Value::I64(DANGER)));
            inner.kids[0].set(Prop::TINT, Some(Value::I64(DANGER)));
         }
         draft.press = Some(entry.press);
         if index > 0
            && let Some(divider) = &self.divider
         {
            kids.push(Draft::from_node(divider));
         }
         kids.push(draft);
      }
      if kids.is_empty() {
         return Vec::new();
      }
      vec![Draft {
         kind: group_kind.clone(),
         props: group_props.clone(),
         kids,
         press: None,
      }]
   }

   /// The colors of Phoenix's first profile page.
   pub fn palette(&self) -> tabs::Palette {
      tabs::Palette {
         text:  self.page_text.unwrap_or(tabs::TEXT),
         muted: self.tab_colors().1,
      }
   }

   /// A plugin tab's text color when selected, and when not.
   pub fn tab_colors(&self) -> (i64, i64) {
      let unselected = self
         .tab
         .as_ref()
         .and_then(|tab| tab.kids.first()?.kids.first())
         .and_then(|text| {
            match mirror::prop(&text.props, Prop::COLOR) {
               Some(Value::I64(color)) => Some(*color),
               Some(Value::I32(color)) => Some(i64::from(*color)),
               _ => None,
            }
         })
         .unwrap_or(tabs::MUTED);
      (self.tab_color.unwrap_or(tabs::ACCENT), unselected)
   }

   /// The timestamp's text style without its placement, or a fallback.
   fn accessory_props(&self) -> Props {
      const DROPPED: [Prop; 7] = [
         Prop::ACCESSIBILITY_ID,
         Prop::ACCESSIBILITY_LABEL,
         Prop::named("accessibilityHidden"),
         Prop::named("maxLines"),
         Prop::MARGIN,
         Prop::named("gravity"),
         Prop::ON,
      ];
      self.timestamp.as_ref().map_or_else(
         || {
            vec![
               (Prop::COLOR, Value::I64(MUTED)),
               (Prop::FONT_SIZE, Value::F64(12.0)),
               (Prop::named("fontWeight"), Value::I32(500)),
            ]
         },
         |timestamp| {
            timestamp
               .props
               .iter()
               .filter(|(key, _)| !DROPPED.contains(key))
               .cloned()
               .collect()
         },
      )
   }
}

fn first_image(node: &Node) -> Option<String> {
   if node.kind == ViewKind::Image
      && let Some(Value::Str(uri)) = mirror::prop(&node.props, Prop::URI)
   {
      return Some(uri.clone());
   }
   node.kids.iter().find_map(first_image)
}

fn first_text_color(node: &Node) -> Option<i64> {
   if node.kind == ViewKind::Text {
      match mirror::prop(&node.props, Prop::COLOR) {
         Some(Value::I64(color)) => return Some(*color),
         Some(Value::I32(color)) => return Some(i64::from(*color)),
         _ => {},
      }
   }
   node.kids.iter().find_map(first_text_color)
}

pub fn text(props: &Props, rendered: &Rendered) -> Draft {
   let mut node = Draft {
      kind:  ViewKind::Text,
      props: props.clone(),
      kids:  Vec::new(),
      press: None,
   };
   node.set(Prop::named("spans"), None);
   node.set(Prop::TEXT, Some(Value::Str(rendered.text.clone())));
   if let Some(color) = rendered.color {
      node.set(Prop::COLOR, Some(Value::I64(color)));
   }
   node
}
