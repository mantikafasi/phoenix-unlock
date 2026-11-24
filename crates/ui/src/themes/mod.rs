//! Custom theme state, Discord token remapping, and UI-op batch rewriting,
//! ported from Valenced's theme system.

pub mod applier;
pub mod color;
pub mod emoji;
pub mod fonts;
pub mod state;

pub use applier::ThemeApplier;
pub use color::{Category, ColorPalette, TokenTable, parse_color};
pub use state::{
   ColorValues, ColorsSelection, EmojiSelection, FontSource, FontVariant, FontsSelection,
   IconsSelection, ParsedTheme, STATE_FORMAT, SYSTEM_EMOJI_FONT, ThemeBase, ThemesState,
   parse_theme_json,
};

#[cfg(test)]
mod tests {
   use std::collections::BTreeMap;

   use phoenix_wire::Value;

   use super::*;
   use crate::{Op, Prop, ViewKind, get_prop, prop};

   #[test]
   fn parses_hex_and_rgb_colors() {
      assert_eq!(parse_color("#fff"), Some(0xFFFF_FFFF));
      assert_eq!(parse_color("#f0a8"), Some(0x88FF_00AA));
      assert_eq!(parse_color("#5865f2"), Some(0xFF58_65F2));
      assert_eq!(parse_color("#1e1e2e80"), Some(0x801E_1E2E));
      assert_eq!(parse_color("rgb(255, 0, 128)"), Some(0xFFFF_0080));
      assert_eq!(parse_color("rgba(30, 30, 46, 0.5)"), Some(0x801E_1E2E));
      assert_eq!(parse_color("not-a-color"), None);
   }

   #[test]
   fn resolves_semantic_and_raw_tokens_in_ui_batches() {
      let mut semantic = BTreeMap::new();
      semantic.insert(
         "darker".to_owned(),
         BTreeMap::from([
            ("BACKGROUND_BASE_LOW".to_owned(), "#1e1e2e".to_owned()),
            ("TEXT_DEFAULT".to_owned(), "#cdd6f4".to_owned()),
         ]),
      );
      let raw = BTreeMap::from([("BLURPLE_50".to_owned(), "#cba6f7".to_owned())]);

      let mut applier = ThemeApplier::new();
      applier.set_base(ThemeBase::Darker);
      applier.set_colors(Some(ColorsSelection {
         theme: "Catppuccin".to_owned(),
         values: ColorValues { semantic, raw },
         ..ColorsSelection::default()
      }));

      let (_, bg_default) = TokenTable::global()
         .semantic_argb("BACKGROUND_BASE_LOW", ThemeBase::Darker)
         .expect("BACKGROUND_BASE_LOW exists");
      let (_, text_default) = TokenTable::global()
         .semantic_argb("TEXT_DEFAULT", ThemeBase::Darker)
         .expect("TEXT_DEFAULT exists");
      let blurple_default = TokenTable::global()
         .raw_argb("BLURPLE_50")
         .expect("BLURPLE_50 exists");

      let mut ops = vec![
         Op::Create {
            id: 1,
            kind: ViewKind::View,
            props: vec![(Prop::BACKGROUND, Value::I64(i64::from(bg_default)))],
         },
         Op::Create {
            id: 2,
            kind: ViewKind::Text,
            props: vec![
               (Prop::COLOR, Value::I64(i64::from(text_default))),
               (
                  prop!("spans"),
                  Value::Arr(vec![Value::Obj(vec![(
                     Prop::COLOR.key(),
                     Value::I64(i64::from(blurple_default)),
                  )])]),
               ),
            ],
         },
      ];

      assert!(applier.rewrite_batch(&mut ops));

      let Op::Create { props: p1, .. } = &ops[0] else {
         panic!("expected Create");
      };
      assert_eq!(
         get_prop(p1, Prop::BACKGROUND),
         Some(&Value::I64(0xFF1E_1E2E))
      );

      let Op::Create { props: p2, .. } = &ops[1] else {
         panic!("expected Create");
      };
      assert_eq!(get_prop(p2, Prop::COLOR), Some(&Value::I64(0xFFCD_D6F4)));
      let Some(Value::Arr(spans)) = get_prop(p2, prop!("spans")) else {
         panic!("expected spans");
      };
      assert_eq!(
         spans[0].field(Prop::COLOR.key()),
         Some(&Value::I64(0xFFCB_A6F7))
      );

      // Clearing colors restores original props via refresh_ops.
      applier.set_colors(None);
      let restored = applier.refresh_ops();
      assert_eq!(restored.len(), 2);
      let restored_bg = restored.iter().find_map(|op| match op {
         Op::Update { id: 1, props } => get_prop(props, Prop::BACKGROUND),
         _ => None,
      });
      assert_eq!(restored_bg, Some(&Value::I64(i64::from(bg_default))));
   }

   #[test]
   fn parses_vendetta_and_valenced_theme_json() {
      let vendetta = r##"{
         "name": "Mocha",
         "description": "Soothing pastel theme",
         "authors": [{"name": "Catppuccin"}],
         "semanticColors": {
            "BACKGROUND_BASE_LOW": ["#1e1e2e", "#eff1f5", "#181825", "#11111b"]
         },
         "rawColors": {
            "BLURPLE_50": "#cba6f7"
         }
      }"##;
      let parsed = parse_theme_json("fallback", vendetta).expect("valid vendetta theme");
      assert_eq!(parsed.name, "Mocha");
      assert_eq!(parsed.authors, vec!["Catppuccin"]);
      assert_eq!(
         parsed.colors.values.semantic["dark"]["BACKGROUND_BASE_LOW"],
         "#1e1e2e"
      );
      assert_eq!(
         parsed.colors.values.semantic["light"]["BACKGROUND_BASE_LOW"],
         "#eff1f5"
      );
      assert_eq!(
         parsed.colors.values.semantic["darker"]["BACKGROUND_BASE_LOW"],
         "#181825"
      );
      assert_eq!(
         parsed.colors.values.semantic["midnight"]["BACKGROUND_BASE_LOW"],
         "#11111b"
      );
      assert_eq!(parsed.colors.values.raw["BLURPLE_50"], "#cba6f7");
   }

   #[test]
   fn emoji_codepoints_and_fallbacks_match_valenced() {
      assert_eq!(
         emoji::codepoints_from_asset("emoji-1f44d-1f3fd-fe0f@2x.png").as_deref(),
         Some("1f44d-1f3fd-fe0f")
      );
      assert_eq!(
         emoji::candidate_image_names("1f44d-1f3fd-fe0f"),
         vec![
            "emoji-1f44d-1f3fd-fe0f.png",
            "emoji-1f44d-1f3fd.png",
            "emoji-1f44d-fe0f.png",
            "emoji-1f44d.png",
         ]
      );
   }

   #[test]
   fn font_variant_scoring_prefers_matching_italic_and_weight() {
      let variants = vec![
         FontVariant {
            path: "/regular.ttf".to_owned(),
            weight: 400,
            italic: false,
            axes: BTreeMap::new(),
         },
         FontVariant {
            path: "/bold.ttf".to_owned(),
            weight: 700,
            italic: false,
            axes: BTreeMap::new(),
         },
         FontVariant {
            path: "/italic.ttf".to_owned(),
            weight: 400,
            italic: true,
            axes: BTreeMap::new(),
         },
      ];
      assert_eq!(
         fonts::closest_variant(&variants, 600, false).map(|v| v.path.as_str()),
         Some("/bold.ttf")
      );
      assert_eq!(
         fonts::closest_variant(&variants, 700, true).map(|v| v.path.as_str()),
         Some("/italic.ttf")
      );
   }

   #[test]
   fn rewrites_pascal_case_icons_and_font_rendered_emoji() {
      let mut applier = ThemeApplier::new();
      applier.set_icons(Some(IconsSelection {
         icons: BTreeMap::from([(
            "ArrowLargeLeftIcon".to_owned(),
            "https://example.com/ArrowLargeLeftIcon.png".to_owned(),
         )]),
         ..IconsSelection::default()
      }));
      applier.set_emoji(Some(EmojiSelection {
         font: Some("system".to_owned()),
         ..EmojiSelection::default()
      }));

      let mut ops = vec![
         Op::Create {
            id:    10,
            kind:  ViewKind::Image,
            props: vec![(
               Prop::URI,
               Value::Str("res://ic_mana_arrow_large_left".to_owned()),
            )],
         },
         Op::Create {
            id:    11,
            kind:  ViewKind::Image,
            props: vec![(Prop::URI, Value::Str("emoji://1f600".to_owned()))],
         },
         Op::Create {
            id:    12,
            kind:  ViewKind::Text,
            props: vec![(
               prop!("spans"),
               Value::Arr(vec![Value::Obj(vec![(
                  prop!("image").key(),
                  Value::Obj(vec![(
                     Prop::URI.key(),
                     Value::Str("emoji://1f44d-1f3fd".to_owned()),
                  )]),
               )])]),
            )],
         },
      ];

      let render = |cp: &str, font: &str| Some(format!("/cache/{font}/emoji-{cp}.png"));
      assert!(applier.rewrite_batch_with_emoji(&mut ops, Some(&render)));

      let Op::Create { props: p10, .. } = &ops[0] else {
         panic!("expected Create");
      };
      assert_eq!(
         get_prop(p10, Prop::URI),
         Some(&Value::Str(
            "https://example.com/ArrowLargeLeftIcon.png".to_owned()
         ))
      );

      let Op::Create { props: p11, .. } = &ops[1] else {
         panic!("expected Create");
      };
      assert_eq!(
         get_prop(p11, Prop::URI),
         Some(&Value::Str("file:///cache/system/emoji-1f600.png".to_owned()))
      );

      let Op::Create { props: p12, .. } = &ops[2] else {
         panic!("expected Create");
      };
      let Some(Value::Arr(spans)) = get_prop(p12, prop!("spans")) else {
         panic!("expected spans");
      };
      assert_eq!(
         spans[0]
            .field(prop!("image").key())
            .and_then(|image| image.field(Prop::URI.key())),
         Some(&Value::Str(
            "file:///cache/system/emoji-1f44d-1f3fd.png".to_owned()
         ))
      );
   }

   #[test]
   fn clearing_fonts_emits_null_font_family_for_text_nodes() {
      let mut applier = ThemeApplier::new();
      applier.set_fonts(Some(FontsSelection {
         families: BTreeMap::from([(
            "systemFont".to_owned(),
            FontSource {
               variants: vec![FontVariant {
                  path:   "sans-serif".to_owned(),
                  weight: 400,
                  italic: false,
                  axes:   BTreeMap::new(),
               }],
               ..FontSource::default()
            },
         )]),
      }));

      let mut ops = vec![Op::Create {
         id:    42,
         kind:  ViewKind::Text,
         props: Vec::new(),
      }];
      assert!(applier.rewrite_batch(&mut ops));
      let Op::Create { props, .. } = &ops[0] else {
         panic!("expected Create");
      };
      assert_eq!(
         get_prop(props, Prop::FONT_FAMILY),
         Some(&Value::Str("sans-serif".to_owned()))
      );

      applier.set_fonts(None);
      let refresh = applier.refresh_ops();
      assert_eq!(refresh.len(), 1);
      let Op::Update { id: 42, props } = &refresh[0] else {
         panic!("expected Update for node 42");
      };
      assert_eq!(get_prop(props, Prop::FONT_FAMILY), Some(&Value::Null));
   }
}
