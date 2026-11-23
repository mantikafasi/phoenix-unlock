//! Plugin badges: the row on profiles, the grid in Phoenix's badge directory,
//! and the detail sheet.
//!
//! - The row has no accessibility id; its images are matched by their
//!   `/badge-icons/` URI.
//! - The directory's title can arrive a batch before its grid, so placement
//!   retries and the tile is learned from a later batch.
//! - The detail sheet follows Phoenix's shape. `mana` and `readerLock` are
//!   wire-only names, so they cannot come from the prop table.

use std::{
   collections::HashSet,
   sync::atomic::{
      AtomicU64,
      Ordering,
   },
   thread,
   time::Duration,
};

use phoenix_ui::{
   Node,
   Op,
   Prop,
   ViewKind,
};
use phoenix_wire::Value;

use super::{
   Before,
   Contribution,
   Draft,
   Handler,
   Job,
   Outcome,
   Patches,
   Place,
   Slot,
   SlotKind,
   avatar_user,
   mirror::{
      Created,
      Mirror,
      prop,
   },
   queue,
   targets::{
      Placement,
      Target,
   },
};

/// A badge a plugin draws on a profile and in the badge directory.
#[derive(Clone)]
pub struct Badge {
   pub uri:         String,
   pub label:       String,
   /// Shown below the badge's name in the badge directory.
   pub description: String,
   /// What gave the badge, like `Vencord` or `BadgeVault`. Shown above the
   /// name.
   pub source:      String,
}

fn view(props: Vec<(Prop, Value)>, kids: Vec<Draft>) -> Draft {
   Draft {
      kind: ViewKind::View,
      props,
      kids,
      press: None,
   }
}

fn fill() -> (Prop, Value) {
   (Prop::named("width"), Value::Str("fill".to_owned()))
}

fn direction(value: &str) -> (Prop, Value) {
   (Prop::named("direction"), Value::Str(value.to_owned()))
}

fn align(value: &str) -> (Prop, Value) {
   (Prop::named("align"), Value::Str(value.to_owned()))
}

impl Patches {
   /// Tracks the user the badge surfaces belong to, which is not always the
   /// one Phoenix's profile targets pick.
   pub(super) fn track_badge_user(&mut self, ops: &[Op], created: &Created<'_>) {
      // Old profile sheets stay mounted, so a batch can touch one sheet while
      // another still holds a profile.
      let touched_sheets = ops
         .iter()
         .flat_map(|op| {
            let id = match op {
               Op::Create { id, .. }
               | Op::Update { id, .. }
               | Op::Children { id, .. }
               | Op::ChildEdits { id, .. } => Some(*id),
               _ => None,
            };
            id.and_then(|id| {
               self.mirror
                  .is_sheet(id)
                  .then_some(id)
                  .or_else(|| self.mirror.sheet_of(id))
            })
         })
         .collect::<HashSet<_>>();
      let you_node = self.mirror.find_aid("you-avatar-preload");
      let on_you = you_node
         .and_then(|id| self.mirror.sheet_of(id))
         .is_some_and(|sheet| touched_sheets.contains(&sheet));

      let mut pages = None;
      let mut scroll = None;
      let mut user = None;
      let mut you = None;
      let mut you_screen = false;
      for id in created.keys().copied() {
         match self.mirror.accessibility_id(id).unwrap_or_default() {
            // Other avatar URLs on the screen belong to app widgets and
            // mutuals, so they cannot name the user.
            "Avatar" => user = avatar_at(&self.mirror, id).or(user),
            "profile-tab-pages" | "profile-user-tag" => pages = Some(id),
            "profile-scroll" => scroll = Some(id),
            "you-avatar-preload" => you = Some(id),
            // The "you" screen is a home tab, not a sheet. Opening it creates
            // its root and scroll view; the avatar preload above persists.
            "you-root" | "you-scroll" | "you-tab-pages" => you_screen = true,
            _ => {},
         }
      }
      // The "you" screen's avatar preload names the user. It wins over a chat
      // sheet still mounted in the same batch.
      let you_seen = you.is_some() || you_screen || on_you;
      let you_id = you.or_else(|| if you_seen { you_node } else { None });
      if let Some(id) = you_id {
         user = avatar_at(&self.mirror, id).or(user);
      }
      if user.is_none()
         && let Some(pages) = pages
      {
         // The scroll view holds this profile's own avatar. Walking up from
         // the pages would reach the app root and find the last profile's
         // avatar instead.
         user = scroll
            .and_then(|scroll| avatar_under(&self.mirror, scroll))
            .or_else(|| avatar_above(&self.mirror, pages));
      }
      if user.is_none() && pages.is_some() && !you_seen {
         // A default avatar carries no user id; the profile fetch just named
         // the user. Never on the "you" screen, where the last fetched
         // profile belongs to someone else.
         user = crate::discord::last_profile();
      }
      let moved = match &user {
         Some(user) if self.seen.badge_user.as_deref() != Some(user.as_str()) => {
            self.seen.badge_user = Some(user.clone());
            true
         },
         Some(_) => false,
         // A new screen with no known user must not keep the last one's
         // badges.
         None if pages.is_some() || you_seen => self.seen.badge_user.take().is_some(),
         None => false,
      };
      if moved {
         // Badges may have been filled before the user was known; point their
         // slots at the new user and draw them again.
         let user = Place::User(self.seen.badge_user.clone());
         for slot in &mut self.slots {
            if matches!(
               slot.kind,
               SlotKind::Target(Target::ProfileBadges | Target::BadgeDirectory)
            ) {
               slot.place = user.clone();
            }
         }
         queue(Job::Refresh(Target::ProfileBadges));
      }
   }

   /// Anchors the badge row on the native badge image in this batch, if any.
   pub(super) fn anchor_badges(
      &mut self,
      ops: &[Op],
      out: &mut Vec<Op>,
      touched: &mut HashSet<i32>,
   ) {
      let image = ops.iter().fold(None, |found, op| {
         let (Op::Create { id, props, .. } | Op::Update { id, props }) = op else {
            return found;
         };
         match prop(props, Prop::URI) {
            Some(Value::Str(uri)) if uri.contains("/badge-icons/") => Some(*id),
            _ => found,
         }
      });
      let Some(id) = image else {
         return;
      };
      self.anchors
         .entry(Target::ProfileBadges)
         .or_default()
         .insert(id);
      if let Some(parent) = self.place(Target::ProfileBadges, id, out) {
         touched.insert(parent);
      }
   }

   /// Shows `badge` in a sheet shaped like Phoenix's own badge detail, with
   /// the MYTHIC pill the badge directory uses.
   pub(super) fn open_badge_detail(&mut self, badge: &Badge) -> Vec<Op> {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      #[expect(clippy::cast_possible_truncation, reason = "details are few")]
      let menu = 0x8000_0000 | NEXT.fetch_add(1, Ordering::Relaxed) as u32;
      let Some(host) = self
         .mirror
         .find_aid("badge-directory-title")
         .and_then(|title| detail_host(&self.mirror, title))
      else {
         log::warn!("patches: nowhere to show a badge detail");
         return Vec::new();
      };
      let dismiss = Handler::Choice { menu, index: -1 };
      let draft = badge_detail(badge, dismiss);
      let mut out = Vec::new();
      let mut all = Vec::new();
      let root = self.emit(draft, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Menu(menu),
         anchor: host,
         parent: host,
         before: Before::End,
         place: Place::Screen,
         roots: vec![root],
         all,
      });
      out.push(Op::Children {
         id:  host,
         ids: self.merged(host),
      });
      out
   }

   /// Retries a directory placement when the title arrived before the grid.
   pub(super) fn retry_badge_placement(&mut self, id: i32, placed: bool) {
      if placed {
         self.badge_retries.remove(&id);
         return;
      }
      let tries = self.badge_retries.entry(id).or_insert(0);
      if *tries < 3 {
         *tries += 1;
         // Directory slots are drawn by ProfileBadges; a refresh named
         // BadgeDirectory would match nothing.
         thread::spawn(|| {
            thread::sleep(Duration::from_millis(400));
            queue(Job::Refresh(Target::ProfileBadges));
         });
      }
   }
}

/// Keeps a tile of the badge directory's grid, for `title` or the directory
/// mounted now.
pub(super) fn learn_tile(
   tile: &mut Option<Node>,
   mirror: &Mirror,
   title: Option<i32>,
   created: &Created<'_>,
) {
   if tile.is_some() {
      return;
   }
   let Some(title) = title.or_else(|| mirror.find_aid("badge-directory-title")) else {
      return;
   };
   let Some(grid) = badge_grid(mirror, title) else {
      return;
   };
   let Some(found) = mirror.kids(grid).iter().find(|kid| has_image(mirror, **kid)) else {
      return;
   };
   *tile = mirror.snapshot(*found, created);
}

/// Where the badge row goes for the native badge image `id`.
pub(super) fn place_row(
   mirror: &Mirror,
   id: i32,
   user: Option<String>,
) -> Option<Placement> {
   // Two wrappers above the image is the row; the chip is fixed-size.
   let chip = mirror.parent(id)?;
   let row = mirror.parent(chip)?;
   let before = first_badge(mirror, row).or_else(|| mirror.kids(row).first().copied());
   Some(Placement {
      parent: row,
      before,
      place: Place::User(user),
   })
}

/// Where the directory grid goes for its title `id`.
pub(super) fn place_directory(
   mirror: &Mirror,
   id: i32,
   user: Option<String>,
) -> Option<Placement> {
   let grid = badge_grid(mirror, id)?;
   Some(Placement {
      parent: grid,
      before: mirror.kids(grid).first().copied(),
      place: Place::User(user),
   })
}

/// The badge images for a profile row.
pub(super) fn draw_row(
   contributions: &[&Contribution],
   outcome: &impl Fn(&Contribution) -> Option<Outcome>,
) -> Vec<Draft> {
   let mut nodes = Vec::new();
   for contribution in contributions {
      let Some(Outcome::Badges(badges)) = outcome(contribution) else {
         continue;
      };
      nodes.extend(badges.iter().map(badge_node));
   }
   nodes
}

/// The badge tiles for the directory, cloned from Phoenix's own tile.
pub(super) fn draw_directory(
   tile: &Node,
   contributions: &[&Contribution],
   outcome: &impl Fn(&Contribution) -> Option<Outcome>,
) -> Vec<Draft> {
   let mut nodes = Vec::new();
   for contribution in contributions {
      let Some(Outcome::Badges(badges)) = outcome(contribution) else {
         continue;
      };
      for (index, badge) in badges.iter().enumerate() {
         let mut node = Draft::from_node(tile);
         strip_tile(&mut node);
         node.set(
            Prop::ACCESSIBILITY_ID,
            Some(Value::Str(format!(
               "phoenix.{}.{}.{index}",
               contribution.plugin, contribution.id.0
            ))),
         );
         node.set(
            Prop::ACCESSIBILITY_LABEL,
            Some(Value::Str(badge.label.clone())),
         );
         if let Some(image) = first_image_mut(&mut node) {
            image.set(Prop::URI, Some(Value::Str(badge.uri.clone())));
         }
         node.press = Some(Handler::Detail {
            badge: badge.clone(),
         });
         nodes.push(node);
      }
   }
   nodes
}

/// Whether `id` or anything under it draws an image.
fn has_image(mirror: &Mirror, id: i32) -> bool {
   mirror.uri(id).is_some() || mirror.kids(id).iter().any(|kid| has_image(mirror, *kid))
}

/// The scroll view of the badge directory whose title is `title`.
fn badge_scroll(mirror: &Mirror, title: i32) -> Option<i32> {
   let mut node = title;
   for _ in 0..6 {
      node = mirror.parent(node)?;
      if let Some(scroll) = mirror
         .kids(node)
         .iter()
         .copied()
         .find(|kid| mirror.kind(*kid) == Some("scroll"))
      {
         return Some(scroll);
      }
   }
   None
}

/// The container native badge details mount in, found from the directory
/// itself so a cold detail still presents natively.
fn detail_host(mirror: &Mirror, title: i32) -> Option<i32> {
   let mut node = title;
   loop {
      let parent = mirror.parent(node)?;
      if mirror.kind(parent) == Some("navigator") {
         return mirror.kids(node).first().copied().or(Some(node));
      }
      node = parent;
   }
}

/// The first flow of badge tiles under the directory window whose title is
/// `title`.
fn badge_grid(mirror: &Mirror, title: i32) -> Option<i32> {
   let mut stack = vec![badge_scroll(mirror, title)?];
   while let Some(node) = stack.pop() {
      if mirror.kind(node) == Some("flow")
         && mirror.kids(node).iter().any(|kid| has_image(mirror, *kid))
      {
         return Some(node);
      }
      stack.extend(mirror.kids(node).iter().rev().copied());
   }
   None
}

/// The first row child holding one of Phoenix's badge images.
fn first_badge(mirror: &Mirror, row: i32) -> Option<i32> {
   mirror.kids(row).iter().copied().find(|kid| {
      let mut stack = vec![*kid];
      while let Some(node) = stack.pop() {
         if mirror
            .uri(node)
            .is_some_and(|uri| uri.contains("/badge-icons/"))
         {
            return true;
         }
         stack.extend(mirror.kids(node).iter().copied());
      }
      false
   })
}

/// The badge sheet: Phoenix's shape, with the badge, its name, its source, and
/// the MYTHIC pill.
pub(super) fn badge_detail(badge: &Badge, dismiss: Handler) -> Draft {
   const SHEET_BG: i64 = 0xFF24_2429;
   const SCRIM: i64 = 0x800F_1014;
   Draft {
      kind:  ViewKind::Other("sheet".to_owned()),
      props: vec![
         (Prop::named("background"), Value::I64(SHEET_BG)),
         (Prop::RADIUS_TOP_LEFT, Value::F64(24.0)),
         (Prop::RADIUS_TOP_RIGHT, Value::F64(24.0)),
         (Prop::named("mana"), Value::Bool(true)),
         (Prop::named("readerLock"), Value::Str("stack".to_owned())),
         (Prop::named("navScrimColor"), Value::I64(SCRIM)),
         (Prop::named("handleColor"), Value::I32(865_572_767)),
         (Prop::named("scrollable"), Value::Bool(true)),
         (Prop::named("dismissLabel"), Value::Str(badge.label.clone())),
         (
            Prop::ON,
            Value::Arr(vec![
               Value::Str("dismiss".to_owned()),
               Value::Str("opened".to_owned()),
            ]),
         ),
      ],
      kids:  vec![view(
         vec![
            direction("column"),
            fill(),
            (Prop::PADDING, Value::Rect([0.0, 16.0, 0.0, 24.0])),
         ],
         vec![badge_body(badge)],
      )],
      press: Some(dismiss),
   }
}

/// The badge sheet's content: art, source, name, the MYTHIC pill, and the
/// description.
fn badge_body(badge: &Badge) -> Draft {
   const MUTED: i64 = 0xFFAB_ACB2;
   const WHITE: i64 = 0xFFFB_FBFB;
   const DESC: i64 = 0xFFEF_EFE1;
   const PILL_BG: i64 = 0x3D58_65F2;
   const PILL_LINE: i64 = 0xFF58_65F2;
   const PILL_TEXT: i64 = 0xFFA0_A9FA;

   let source = if badge.source.is_empty() {
      "Client Mod"
   } else {
      badge.source.as_str()
   };
   let credit = Draft {
      kind:  ViewKind::Text,
      props: vec![
         (Prop::TEXT, Value::Str(source.to_owned())),
         (Prop::COLOR, Value::I64(MUTED)),
         (Prop::FONT_SIZE, Value::F64(16.0)),
         (Prop::named("fontWeight"), Value::I32(500)),
         (Prop::LINE_HEIGHT, Value::F64(20.0)),
         (Prop::named("lineHeightExact"), Value::Bool(true)),
         (Prop::named("textAlign"), Value::Str("center".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
   };
   let title = Draft {
      kind:  ViewKind::Text,
      props: vec![
         (Prop::TEXT, Value::Str(badge.label.clone())),
         (Prop::COLOR, Value::I64(WHITE)),
         (Prop::FONT_SIZE, Value::F64(24.0)),
         (Prop::named("fontWeight"), Value::I32(700)),
         (Prop::FONT_FAMILY, Value::Str("ginto".to_owned())),
         (Prop::LINE_HEIGHT, Value::F64(26.0)),
         (Prop::named("textTransform"), Value::Str("uppercase".to_owned())),
         (Prop::named("lineHeightExact"), Value::Bool(true)),
         (Prop::named("textAlign"), Value::Str("center".to_owned())),
         (Prop::ROLE, Value::Str("header".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
   };
   let pill = view(
      vec![
         direction("row"),
         (Prop::PADDING, Value::Rect([7.0, 1.0, 7.0, 1.0])),
         (Prop::named("minHeight"), Value::F64(20.0)),
         (Prop::GAP, Value::F64(4.0)),
         align("center"),
         (Prop::named("background"), Value::I64(PILL_BG)),
         (Prop::RADIUS, Value::F64(9999.0)),
         (Prop::named("borderColor"), Value::I64(PILL_LINE)),
         (Prop::named("borderWidth"), Value::F64(1.0)),
      ],
      vec![
         Draft {
            kind:  ViewKind::Image,
            props: vec![
               (Prop::URI, Value::Str("res://ic_mana_experimental_rare".to_owned())),
               (Prop::named("width"), Value::F64(12.0)),
               (Prop::HEIGHT, Value::F64(12.0)),
               (Prop::TINT, Value::I64(PILL_TEXT)),
               (Prop::named("scale"), Value::Str("fit".to_owned())),
            ],
            kids:  Vec::new(),
            press: None,
         },
         Draft {
            kind:  ViewKind::Text,
            props: vec![
               (Prop::TEXT, Value::Str("MYTHIC".to_owned())),
               (Prop::COLOR, Value::I64(PILL_TEXT)),
               (Prop::FONT_SIZE, Value::F64(12.0)),
               (Prop::named("fontWeight"), Value::I32(600)),
               (Prop::LINE_HEIGHT, Value::F64(16.0)),
               (Prop::named("lineHeightExact"), Value::Bool(true)),
               (Prop::named("maxLines"), Value::I32(1)),
            ],
            kids:  Vec::new(),
            press: None,
         },
      ],
   );
   let meta = Draft {
      kind:  ViewKind::Other("flow".to_owned()),
      props: vec![
         direction("row"),
         (Prop::GAP, Value::F64(6.0)),
         align("center"),
         (Prop::named("justify"), Value::Str("center".to_owned())),
      ],
      kids:  vec![pill],
      press: None,
   };
   let credit_wrap = view(
      vec![
         (Prop::MARGIN, Value::Rect([0.0, 0.0, 0.0, 4.0])),
         align("stretch"),
      ],
      vec![credit],
   );
   let title_wrap = view(vec![fill(), align("stretch")], vec![title]);
   let text_block = view(
      vec![direction("column"), fill(), align("center")],
      vec![credit_wrap, title_wrap, meta],
   );
   let stack = Draft {
      kind:  ViewKind::Stack,
      props: vec![fill()],
      kids:  vec![view(
         vec![fill(), align("stretch")],
         vec![text_block],
      )],
      press: None,
   };
   let image = Draft {
      kind:  ViewKind::Image,
      props: vec![
         (Prop::URI, Value::Str(badge.uri.clone())),
         (Prop::MARGIN, Value::Rect([0.0, 0.0, 0.0, 12.0])),
         (Prop::named("width"), Value::F64(120.0)),
         (Prop::HEIGHT, Value::F64(120.0)),
         (Prop::named("scale"), Value::Str("fit".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
   };
   let image_center = view(
      vec![direction("column"), fill(), align("center")],
      vec![image],
   );
   let image_wrap = view(vec![fill(), align("stretch")], vec![image_center]);
   let hero = view(
      vec![
         direction("column"),
         (Prop::PADDING, Value::Rect([0.0, 8.0, 0.0, 0.0])),
         fill(),
         align("center"),
      ],
      vec![image_wrap, stack],
   );
   let description = (!badge.description.is_empty()).then(|| Draft {
      kind:  ViewKind::Text,
      props: vec![
         (Prop::TEXT, Value::Str(badge.description.clone())),
         (Prop::COLOR, Value::I64(DESC)),
         (Prop::FONT_SIZE, Value::F64(16.0)),
         (Prop::named("fontWeight"), Value::I32(500)),
         (Prop::LINE_HEIGHT, Value::F64(20.0)),
         (Prop::named("lineHeightExact"), Value::Bool(true)),
      ],
      kids:  Vec::new(),
      press: None,
   });
   let mut body_kids = vec![hero];
   body_kids.extend(description);
   view(
      vec![
         direction("column"),
         (Prop::PADDING, Value::Rect([16.0, 0.0, 16.0, 0.0])),
         fill(),
         (Prop::GAP, Value::F64(24.0)),
         align("stretch"),
      ],
      body_kids,
   )
}

/// Drops native identity and animation from a cloned tile. The `on` list
/// stays, because the host only dispatches presses for nodes that carry one.
fn strip_tile(node: &mut Draft) {
   node.props.retain(|(key, _)| {
      !matches!(
         key.name(),
         Some(
            "portalSlot"
               | "viewMotion"
               | "worklet"
               | "accessibilityLabel"
               | "accessibilityId"
         )
      )
   });
   for kid in &mut node.kids {
      strip_tile(kid);
   }
}

/// The first image in `node`.
fn first_image_mut(node: &mut Draft) -> Option<&mut Draft> {
   if node.kind == ViewKind::Image {
      return Some(node);
   }
   node.kids.iter_mut().find_map(first_image_mut)
}

/// One badge image at the size Phoenix uses for its own.
fn badge_node(badge: &Badge) -> Draft {
   let mut node = Draft {
      kind:  ViewKind::Image,
      props: Vec::new(),
      kids:  Vec::new(),
      press: None,
   };
   node.set(Prop::URI, Some(Value::Str(badge.uri.clone())));
   node.set(Prop::named("width"), Some(Value::I32(20)));
   node.set(Prop::HEIGHT, Some(Value::I32(20)));
   if !badge.label.is_empty() {
      node.set(
         Prop::ACCESSIBILITY_LABEL,
         Some(Value::Str(badge.label.clone())),
      );
   }
   node
}

/// The user whose avatar `id` or one of its images shows.
fn avatar_at(mirror: &Mirror, id: i32) -> Option<String> {
   let mut stack = vec![id];
   while let Some(node) = stack.pop() {
      if let Some(uri) = mirror.uri(node)
         && let Some(user) = avatar_user(uri)
      {
         return Some(user);
      }
      stack.extend(mirror.kids(node).iter().copied());
   }
   None
}

/// The user whose avatar sits at or under `root`.
fn avatar_under(mirror: &Mirror, root: i32) -> Option<String> {
   let mut stack = vec![root];
   while let Some(node) = stack.pop() {
      if mirror.accessibility_id(node) == Some("Avatar")
         && let Some(user) = avatar_at(mirror, node)
      {
         return Some(user);
      }
      stack.extend(mirror.kids(node).iter().copied());
   }
   None
}

/// The user whose avatar sits in a nearby ancestor of `node`.
fn avatar_above(mirror: &Mirror, node: i32) -> Option<String> {
   let mut current = node;
   for _ in 0..8 {
      let parent = mirror.parent(current)?;
      if let Some(user) = avatar_under(mirror, parent) {
         return Some(user);
      }
      current = parent;
   }
   None
}
