//! Plugin tabs on profiles: the tab next to Phoenix's own, and the page of
//! blocks it shows.
//!
//! - The tabs sit in a row with Phoenix's, before its sliding indicator, and
//!   the pages go after Phoenix's in the paging `hscroll` (`profile-tab-pages`).
//! - Phoenix's tab presses set the pager's `page`; a plugin tab sets it too,
//!   past Phoenix's pages. The pager's `pageSelect`/`pageSettle`/`pageScroll`
//!   events about those pages never reach Rust, which knows only its own.
//! - The indicator follows the pager on the host side and has no stop for a
//!   plugin tab, so it hides while one is open and the tab draws its own bar.
//! - Rust sizes the pager to its open page. A plugin page reports its height
//!   with `layout`, and Rust's height comes back when one of its tabs opens.

use phoenix_ui::{
   Node,
   Prop,
   ViewKind,
   templates,
};
use phoenix_wire::Value;

use crate::patches::{
   Block,
   Contribution,
   Draft,
   Handler,
   Outcome,
};

const DANGER: i64 = 4_294_475_386;
pub const ACCENT: i64 = 4_286_156_281;
pub const MUTED: i64 = 4_289_440_946;
pub const TEXT: i64 = 4_294_704_123;

/// Colors from the profile's own tabs, so pages follow its theme.
#[derive(Clone, Copy)]
pub struct Palette {
   /// The open tab's label.
   pub text:  i64,
   /// The other tabs' labels.
   pub muted: i64,
}

impl Palette {
   /// `muted` at `alpha`, for fills that work on light and dark themes.
   fn wash(self, alpha: i64) -> Value {
      Value::I64((self.muted & 0x00FF_FFFF) | (alpha << 24))
   }
}

/// Props Phoenix animates a tab with, which a plugin tab must not share.
const ANIMATED: [&str; 3] = ["viewMotion", "worklet", "portalSlot"];

fn str(text: &str) -> Value {
   Value::Str(text.to_owned())
}

fn node(kind: ViewKind, props: Vec<(&str, Value)>) -> Draft {
   Draft {
      kind,
      props: props
         .into_iter()
         .map(|(key, value)| (Prop::named(key), value))
         .collect(),
      kids: Vec::new(),
      press: None,
      src: None,
   }
}

fn view(props: Vec<(&str, Value)>, kids: Vec<Draft>) -> Draft {
   let mut view = node(ViewKind::View, props);
   view.kids = kids;
   view
}

fn label(text: &str, size: f64, weight: i32, color: i64) -> Draft {
   node(ViewKind::Text, vec![
      ("text", str(text)),
      ("color", Value::I64(color)),
      ("fontSize", Value::F64(size)),
      ("fontWeight", Value::I32(weight)),
      ("lineHeight", Value::F64(size + 4.0)),
      ("lineHeightExact", Value::Bool(true)),
   ])
}

fn pressable(mut draft: Draft, press: Handler) -> Draft {
   draft.set(Prop::ROLE, Some(str("button")));
   draft.set(Prop::ON, Some(Value::Arr(vec![str("press")])));
   draft.press = Some(press);
   draft
}

/// A plugin tab: Phoenix's unselected `tab` without its animation, holding
/// its label and a bar under it, `[inner > text, bar]`.
pub fn tab(template: &Node, contribution: &Contribution) -> Draft {
   let mut tab = Draft::from_node(template);
   for key in ANIMATED {
      tab.set(Prop::named(key), None);
   }
   tab.set(Prop::named("accessibilitySelected"), Some(Value::Bool(false)));
   tab.set(Prop::ACCESSIBILITY_LABEL, Some(str(&contribution.label)));
   tab.set(
      Prop::ACCESSIBILITY_ID,
      Some(str(&format!("phoenix.{}.{}", contribution.plugin, contribution.id))),
   );
   tab.set(Prop::named("direction"), Some(str("column")));
   tab.set(Prop::named("align"), Some(str("center")));
   tab.set(Prop::PADDING, None);
   let mut tab = pressable(tab, Handler::Tab {
      contribution: contribution.id,
   });
   let mut inner = tab.kids.swap_remove(0);
   inner.kids.truncate(1);
   if let Some(text) = inner.kids.first_mut() {
      for key in ANIMATED {
         text.set(Prop::named(key), None);
      }
      text.set(Prop::named("spans"), None);
      text.set(Prop::TEXT, Some(str(&contribution.label)));
   }
   let bar = view(
      vec![
         ("width", str("fill")),
         ("height", Value::F64(2.0)),
         ("margin", Value::Rect([0.0, 12.0, 0.0, 0.0])),
         ("radiusTopLeft", Value::F64(4.0)),
         ("radiusTopRight", Value::F64(4.0)),
         ("background", Value::I64(0)),
      ],
      Vec::new(),
   );
   tab.kids = vec![inner, bar];
   tab
}

/// The page of `contribution`'s tab: `page > scroll > content > blocks`. `content`
/// reports its height.
pub fn page(
   width: Option<Value>,
   colors: Palette,
   contribution: &Contribution,
   outcome: Option<Outcome>,
) -> Draft {
   let blocks = match outcome {
      Some(Outcome::Page(blocks)) => blocks,
      None => {
         vec![Block::Text {
            title: None,
            text:  "Loading…".to_owned(),
            color: Some(colors.muted),
         }]
      },
      Some(_) => Vec::new(),
   };
   let id = contribution.id;
   let mut kids = Vec::new();
   let mut after_entry = false;
   for block in blocks {
      let entry = matches!(block, Block::Entry { .. });
      if entry && after_entry {
         kids.push(view(
            vec![("width", str("fill")), ("height", Value::F64(1.0)), ("background", colors.wash(0x26))],
            Vec::new(),
         ));
      }
      after_entry = entry;
      kids.push(draw(block, id, colors));
   }
   let mut content = view(
      vec![
         ("direction", str("column")),
         ("width", str("fill")),
         ("padding", Value::Rect([16.0, 16.0, 16.0, 16.0])),
         ("gap", Value::F64(12.0)),
         ("align", str("stretch")),
         ("on", Value::Arr(vec![str("layout")])),
      ],
      kids,
   );
   content.press = Some(Handler::Page { contribution: id });
   // Like Phoenix's pages: a locked scroll lets the content take its full
   // height, which it reports, instead of being squeezed to the pager's.
   let mut scroll = node(ViewKind::Scroll, vec![
      ("maxHeight", str("100%")),
      ("scrollIndicator", Value::Bool(false)),
      ("bounces", Value::Bool(false)),
      ("scrollLocked", Value::Bool(true)),
      ("keyboardShouldPersistTaps", str("handled")),
   ]);
   scroll.kids = vec![content];
   view(
      vec![
         ("direction", str("column")),
         ("width", width.unwrap_or_else(|| str("fill"))),
         ("height", str("fill")),
         ("align", str("stretch")),
      ],
      vec![scroll],
   )
}

fn block_handler(contribution: crate::patches::ContributionId, index: u32) -> Handler {
   Handler::Block {
      contribution,
      index,
   }
}

fn draw(
   block: Block,
   contribution: crate::patches::ContributionId,
   colors: Palette,
) -> Draft {
   let (text_color, muted) = (colors.text, colors.muted);
   match block {
      Block::Text { title, text, color } => {
         let mut kids = Vec::new();
         if let Some(title) = title {
            kids.push(label(&title, 12.0, 700, muted));
         }
         kids.push(label(&text, 14.0, 400, color.unwrap_or(text_color)));
         view(
            vec![("direction", str("column")), ("gap", Value::F64(4.0)), ("align", str("stretch"))],
            kids,
         )
      },
      Block::Entry {
         avatar,
         title,
         subtitle,
         text,
         badges,
         images,
         actions,
         side,
         hold,
      } => {
         // Like Vencord's: avatar, then name, badges and date over the text.
         let mut head = vec![{
            let mut name = label(&title, 15.0, 600, text_color);
            name.set(Prop::named("maxLines"), Some(Value::I32(1)));
            name.set(Prop::named("shrink"), Some(Value::Bool(true)));
            name
         }];
         for badge in badges {
            head.push(image(&badge, 16.0, 0.0));
         }
         if let Some(subtitle) = subtitle {
            head.push(label(&subtitle, 12.0, 500, muted));
         }
         let mut kids = vec![
            view(
               vec![("direction", str("row")), ("gap", Value::F64(6.0)), ("align", str("center"))],
               head,
            ),
            rich(&text, text_color),
         ];
         for uri in images {
            let mut picture = image(&uri, 0.0, 8.0);
            picture.set(Prop::named("width"), Some(Value::F64(220.0)));
            picture.set(Prop::named("height"), Some(Value::F64(160.0)));
            picture.set(Prop::named("scale"), Some(str("fit")));
            picture.set(Prop::named("align"), Some(str("start")));
            kids.push(picture);
         }
         if !actions.is_empty() {
            let row = actions
               .into_iter()
               .map(|action| {
                  let color = action.color.unwrap_or(if action.danger { DANGER } else { muted });
                  let mut piece = label(&action.label, 13.0, 600, color);
                  piece.set(Prop::PADDING, Some(Value::Rect([2.0, 4.0, 2.0, 4.0])));
                  match action.handler {
                     Some(handler) => pressable(piece, block_handler(contribution, handler)),
                     None => piece,
                  }
               })
               .collect();
            kids.push(view(
               vec![("direction", str("row")), ("gap", Value::F64(14.0)), ("align", str("center"))],
               row,
            ));
         }
         let body = view(
            vec![
               ("direction", str("column")),
               ("flex", Value::F64(1.0)),
               ("gap", Value::F64(4.0)),
               ("align", str("stretch")),
            ],
            kids,
         );
         let mut row = Vec::new();
         if let Some(avatar) = avatar {
            row.push(image(&avatar, 40.0, 20.0));
         }
         row.push(body);
         if !side.is_empty() {
            let column = side
               .into_iter()
               .map(|action| {
                  let color = action.color.unwrap_or(if action.danger { DANGER } else { muted });
                  let mut piece = label(&action.label, 14.0, 700, color);
                  piece.set(Prop::PADDING, Some(Value::Rect([8.0, 2.0, 8.0, 2.0])));
                  match action.handler {
                     Some(handler) => pressable(piece, block_handler(contribution, handler)),
                     None => piece,
                  }
               })
               .collect();
            row.push(view(
               vec![("direction", str("column")), ("align", str("center")), ("gap", Value::F64(2.0))],
               column,
            ));
         }
         let mut card = view(
            vec![
               ("direction", str("row")),
               ("padding", Value::Rect([0.0, 4.0, 0.0, 4.0])),
               ("gap", Value::F64(12.0)),
               ("align", str("start")),
            ],
            row,
         );
         if let Some(index) = hold {
            card.set(Prop::ON, Some(Value::Arr(vec![str("longPress")])));
            card.press = Some(Handler::Hold {
               contribution,
               index,
            });
         }
         card
      },
      Block::Buttons(buttons) => {
         let kids = buttons
            .into_iter()
            .filter_map(|action| {
               let mut button = button(&action.label, action.danger, colors);
               button.set(Prop::named("flex"), Some(Value::F64(1.0)));
               Some(pressable(button, block_handler(contribution, action.handler?)))
            })
            .collect();
         view(vec![("direction", str("row")), ("gap", Value::F64(8.0))], kids)
      },
      Block::Button {
         label: text,
         danger,
         handler,
      } => pressable(button(&text, danger, colors), block_handler(contribution, handler)),
      Block::Input {
         placeholder,
         handler,
         send,
      } => input(&placeholder, contribution, handler, send, colors),
   }
}

fn image(uri: &str, size: f64, radius: f64) -> Draft {
   let mut props = vec![("uri", str(uri))];
   if size > 0.0 {
      props.push(("width", Value::F64(size)));
      props.push(("height", Value::F64(size)));
   }
   if radius > 0.0 {
      props.push(("radius", Value::F64(radius)));
   }
   node(ViewKind::Image, props)
}

/// `text` with Discord's custom emoji (`<:name:id>`, `<a:name:id>`) drawn as
/// images, the words wrapping around them.
fn rich(text: &str, color: i64) -> Draft {
   let pieces = emoji_pieces(text);
   if !pieces.iter().any(|piece| matches!(piece, Piece::Emoji(_))) {
      return label(text, 14.0, 400, color);
   }
   let mut kids = Vec::new();
   for piece in pieces {
      match piece {
         Piece::Text(text) => {
            for word in text.split_inclusive(' ') {
               kids.push(label(word, 14.0, 400, color));
            }
         },
         Piece::Emoji(uri) => kids.push(image(&uri, 20.0, 0.0)),
      }
   }
   let mut flow = view(vec![("direction", str("row")), ("align", str("center"))], kids);
   flow.kind = ViewKind::Other("flow".to_owned());
   flow
}

#[derive(Debug, PartialEq)]
enum Piece {
   Text(String),
   /// The emoji's image URL.
   Emoji(String),
}

fn emoji_pieces(text: &str) -> Vec<Piece> {
   let mut pieces = Vec::new();
   let mut rest = text;
   while let Some(start) = rest.find('<') {
      let tail = &rest[start + 1..];
      let parsed = tail.split_once('>').and_then(|(inner, after)| {
         let (animated, inner) = match inner.strip_prefix("a:") {
            Some(inner) => (true, inner),
            None => (false, inner.strip_prefix(':')?),
         };
         let (name, id) = inner.split_once(':')?;
         let valid = !name.is_empty() && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
         valid.then(|| {
            let ext = if animated { "gif" } else { "webp" };
            (format!("https://cdn.discordapp.com/emojis/{id}.{ext}?size=48"), after)
         })
      });
      match parsed {
         Some((uri, after)) => {
            if start > 0 {
               pieces.push(Piece::Text(rest[..start].to_owned()));
            }
            pieces.push(Piece::Emoji(uri));
            rest = after;
         },
         None => {
            pieces.push(Piece::Text(rest[..=start].to_owned()));
            rest = tail;
         },
      }
   }
   if !rest.is_empty() {
      pieces.push(Piece::Text(rest.to_owned()));
   }
   pieces
}

#[cfg(test)]
mod tests {
   use super::{
      Piece,
      emoji_pieces,
   };

   #[test]
   fn splits_custom_emoji() {
      assert_eq!(emoji_pieces("hi <:wave:123> a<b <a:spin:9>"), vec![
         Piece::Text("hi ".to_owned()),
         Piece::Emoji("https://cdn.discordapp.com/emojis/123.webp?size=48".to_owned()),
         Piece::Text(" a<".to_owned()),
         Piece::Text("b ".to_owned()),
         Piece::Emoji("https://cdn.discordapp.com/emojis/9.gif?size=48".to_owned()),
      ]);
   }
}

fn button(text: &str, danger: bool, colors: Palette) -> Draft {
   view(
            vec![
               ("direction", str("row")),
               ("padding", Value::Rect([16.0, 10.0, 16.0, 10.0])),
               ("minHeight", Value::F64(40.0)),
               ("align", str("center")),
               ("justify", str("center")),
               ("background", colors.wash(0x1F)),
               ("activeBackground", colors.wash(0x33)),
               ("radius", Value::F64(8.0)),
               ("borderColor", colors.wash(0x26)),
               ("borderWidth", Value::F64(1.0)),
               ("accessibilityLabel", str(text)),
            ],
            vec![label(text, 14.0, 600, if danger { DANGER } else { colors.text })],
   )
}

/// Phoenix's search box without its icon, `root > frame > row > input`.
fn input(
   placeholder: &str,
   contribution: crate::patches::ContributionId,
   index: u32,
   send: bool,
   colors: Palette,
) -> Draft {
   let mut root = Draft::from_node(&templates().input);
   let Some(row) = root.kids.first_mut().and_then(|frame| frame.kids.first_mut()) else {
      return root;
   };
   if row.kids.len() > 1 {
      row.kids.remove(0);
   }
   if let Some(input) = row.kids.first_mut() {
      input.set(Prop::MARGIN, None);
      input.set(Prop::PLACEHOLDER, Some(str(placeholder)));
      input.set(Prop::VALUE, Some(str("")));
      input.set(Prop::RETURN_KEY, Some(str("send")));
      input.set(Prop::COLOR, Some(Value::I64(colors.text)));
      input.set(Prop::named("placeholderColor"), Some(Value::I64(colors.muted)));
      input.set(Prop::ROLE, None);
      input.set(Prop::ACCESSIBILITY_ID, None);
      input.set(Prop::named("autoCapitalize"), Some(str("sentences")));
      input.set(Prop::named("autoCorrect"), Some(Value::Bool(true)));
      input.set(Prop::ON, Some(Value::Arr(vec![str("changeText"), str("submit")])));
      input.press = Some(Handler::Input {
         contribution,
         index,
      });
   }
   if send {
      let mut icon = image("res://ic_mana_send_message", 22.0, 0.0);
      icon.set(Prop::TINT, Some(Value::I64(colors.text)));
      let wrap = view(
         vec![
            ("padding", Value::Rect([8.0, 8.0, 10.0, 8.0])),
            ("align", str("center")),
            ("justify", str("center")),
            ("radius", Value::F64(16.0)),
            ("activeBackground", colors.wash(0x33)),
            ("accessibilityLabel", str("Send")),
         ],
         vec![icon],
      );
      row.kids.push(pressable(wrap, Handler::Send { contribution, index }));
   }
   root
}
