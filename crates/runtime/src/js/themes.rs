//! `phoenix.themes`, exposing Valenced-compatible theme selection and installed
//! theme management to plugins.

use phoenix_ui::themes::{
   ColorsSelection, EmojiSelection, FontsSelection, IconsSelection, TokenTable,
};
use rquickjs::{Array, Ctx, Exception, Function, Object, Value, function::Opt};
use serde::de::DeserializeOwned;

use crate::themes;

pub fn themes_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let obj = Object::new(ctx.clone())?;
   add_selection_methods(ctx, &obj)?;
   add_info_methods(ctx, &obj)?;
   Ok(obj)
}

fn add_selection_methods<'js>(ctx: &Ctx<'js>, obj: &Object<'js>) -> rquickjs::Result<()> {
   obj.set(
      "getState",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let json = serde_json::to_string(&themes::state())
            .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))?;
         ctx.json_parse(json)
      })?,
   )?;

   obj.set(
      "setColors",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, selection: Opt<Value<'js>>| -> rquickjs::Result<()> {
            let parsed: Option<ColorsSelection> = parse_optional(&ctx, selection.0)?;
            themes::set_colors(parsed);
            Ok(())
         },
      )?,
   )?;

   obj.set(
      "setFonts",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, selection: Opt<Value<'js>>| -> rquickjs::Result<()> {
            let parsed: Option<FontsSelection> = parse_optional(&ctx, selection.0)?;
            themes::set_fonts(parsed);
            Ok(())
         },
      )?,
   )?;

   obj.set(
      "setIcons",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, selection: Opt<Value<'js>>| -> rquickjs::Result<()> {
            let parsed: Option<IconsSelection> = parse_optional(&ctx, selection.0)?;
            themes::set_icons(parsed);
            Ok(())
         },
      )?,
   )?;

   obj.set(
      "setEmoji",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, selection: Opt<Value<'js>>| -> rquickjs::Result<()> {
            let parsed: Option<EmojiSelection> = parse_optional(&ctx, selection.0)?;
            themes::set_emoji(parsed);
            Ok(())
         },
      )?,
   )?;

   Ok(())
}

fn add_info_methods<'js>(ctx: &Ctx<'js>, obj: &Object<'js>) -> rquickjs::Result<()> {

   obj.set(
      "base",
      Function::new(ctx.clone(), || themes::base().as_str())?,
   )?;

   obj.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let list = Array::new(ctx.clone())?;
         for (index, theme) in themes::list_installed().into_iter().enumerate() {
            let item = Object::new(ctx.clone())?;
            item.set("id", theme.id)?;
            item.set("name", theme.name)?;
            if let Some(description) = theme.description {
               item.set("description", description)?;
            }
            let authors = Array::new(ctx.clone())?;
            for (i, author) in theme.authors.into_iter().enumerate() {
               authors.set(i, author)?;
            }
            item.set("authors", authors)?;
            item.set("active", theme.active)?;
            list.set(index, item)?;
         }
         Ok::<_, rquickjs::Error>(list)
      })?,
   )?;

   obj.set(
      "apply",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, id: String| {
         themes::apply_installed(&id)
            .map_err(|err| Exception::throw_message(&ctx, &err.to_string()))
      })?,
   )?;

   obj.set("clear", Function::new(ctx.clone(), themes::clear_all)?)?;

   obj.set(
      "systemPalettes",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let Some(palettes) = themes::system_palettes() else {
            return Ok(Value::new_null(ctx));
         };
         let json = serde_json::to_string(&palettes)
            .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))?;
         ctx.json_parse(json)
      })?,
   )?;

   obj.set(
      "tokens",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let raw = TokenTable::global().raw_hex_map();
         let json = serde_json::to_string(&serde_json::json!({ "raw": raw }))
            .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))?;
         ctx.json_parse(json)
      })?,
   )?;

   Ok(())
}

fn parse_optional<'js, T: DeserializeOwned>(
   ctx: &Ctx<'js>,
   value: Option<Value<'js>>,
) -> rquickjs::Result<Option<T>> {
   let Some(value) = value else {
      return Ok(None);
   };
   if value.is_null() || value.is_undefined() {
      return Ok(None);
   }
   let json = if let Some(text) = value.as_string() {
      text.to_string()?
   } else {
      let Some(stringified) = ctx.json_stringify(value)? else {
         return Err(Exception::throw_type(
            ctx,
            "expected a theme object or null",
         ));
      };
      stringified.to_string()?
   };
   serde_json::from_str(&json)
      .map(Some)
      .map_err(|err| Exception::throw_type(ctx, &format!("invalid theme payload: {err}")))
}
