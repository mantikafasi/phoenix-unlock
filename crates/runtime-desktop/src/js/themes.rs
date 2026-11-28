//! `phoenix.themes` on desktop: `.theme.css` themes through
//! [`crate::gpui_theme`], with the method names Android's themes use.

use std::{
   path::PathBuf,
   sync::{
      Mutex,
      PoisonError,
   },
};

use rquickjs::{
   Array,
   Ctx,
   Exception,
   Function,
   Object,
};

use crate::gpui_theme;

/// The plugin whose theme is showing, and the file it applied, so unloading
/// the plugin brings back Discord's colors.
static ACTIVE: Mutex<Option<(String, Option<String>)>> = Mutex::new(None);

fn themes_dir() -> PathBuf {
   phoenix_runtime::state().map_or_else(
      || PathBuf::from("themes"),
      |state| state.plugins_dir().with_file_name("themes"),
   )
}

fn set_active(active: Option<(String, Option<String>)>) {
   *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = active;
}

pub fn unload(plugin: &str) {
   let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
   if active.as_ref().is_some_and(|(owner, _)| owner == plugin) {
      *active = None;
      drop(active);
      gpui_theme::clear();
   }
}

pub fn object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let themes = Object::new(ctx.clone())?;
   themes.set(
      "supported",
      Function::new(ctx.clone(), gpui_theme::is_supported)?,
   )?;
   themes.set(
      "list",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>| -> rquickjs::Result<Array<'js>> {
            let active = match ACTIVE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .as_ref()
            {
               Some((_, file)) => file.clone(),
               None => crate::mana_theme::active_file(),
            };
            let arr = Array::new(ctx.clone())?;
            for (i, theme) in gpui_theme::list(&themes_dir()).into_iter().enumerate() {
               let item = Object::new(ctx.clone())?;
               item.set("active", active.as_deref() == Some(theme.file.as_str()))?;
               item.set("id", theme.file)?;
               item.set("name", theme.name)?;
               if let Some(description) = theme.description {
                  item.set("description", description)?;
               }
               let authors = Array::new(ctx.clone())?;
               if let Some(author) = theme.author {
                  authors.set(0, author)?;
               }
               item.set("authors", authors)?;
               arr.set(i, item)?;
            }
            Ok(arr)
         },
      )?,
   )?;
   let owner = plugin.to_owned();
   themes.set(
      "apply",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: String| {
         let dir = themes_dir();
         if !crate::mana_theme::is_supported() {
            return Err(Exception::throw_message(
               &ctx,
               "themes are not supported on this build",
            ));
         }
         crate::mana_theme::apply_file(&dir, &id)
            .map_err(|err| Exception::throw_message(&ctx, &err))?;
         set_active(Some((owner.clone(), Some(id))));
         Ok(())
      })?,
   )?;
   let owner = plugin.to_owned();
   themes.set(
      "setCss",
      Function::new(ctx.clone(), move |css: String| {
         set_active(Some((owner.clone(), None)));
         crate::mana_theme::forget_saved();
         gpui_theme::set_css(&css)
      })?,
   )?;
   themes.set(
      "clear",
      Function::new(ctx.clone(), || {
         set_active(None);
         gpui_theme::clear();
      })?,
   )?;
   attach_reload(ctx, &themes, plugin)?;
   themes.set(
      "quickCss",
      Function::new(ctx.clone(), crate::mana_theme::quick_css)?,
   )?;
   themes.set(
      "openThemesDir",
      Function::new(ctx.clone(), || {
         let dir = themes_dir();
         std::thread::spawn(move || gpui_theme::open_dir(&dir));
      })?,
   )?;
   Ok(themes)
}

fn attach_reload<'js>(
   ctx: &Ctx<'js>,
   themes: &Object<'js>,
   plugin: &str,
) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   themes.set(
      "reload",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         if !crate::mana_theme::is_supported() {
            return Ok(false);
         }
         let owns_file = ACTIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(active_owner, file)| active_owner == &owner && file.is_some());
         if !owns_file {
            return Ok(false);
         }
         crate::mana_theme::reload_active()
            .map(|reloaded| reloaded.unwrap_or(false))
            .map_err(|err| Exception::throw_message(&ctx, &err))
      })?,
   )?;
   Ok(())
}
