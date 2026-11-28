//! `phoenix.fonts` on desktop: Discord's fonts swapped through
//! [`crate::gpui_font`].

use std::sync::{
   Mutex,
   PoisonError,
};

use rquickjs::{
   Ctx,
   Function,
   Object,
   prelude::Opt,
};

use crate::gpui_font;

/// The plugin whose fonts are showing, so unloading it brings back Discord's.
static OWNER: Mutex<Option<String>> = Mutex::new(None);

pub fn unload(plugin: &str) {
   let mut owner = OWNER.lock().unwrap_or_else(PoisonError::into_inner);
   if owner.as_deref() == Some(plugin) {
      *owner = None;
      drop(owner);
      gpui_font::clear();
   }
}

pub fn object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let fonts = Object::new(ctx.clone())?;
   fonts.set(
      "supported",
      Function::new(ctx.clone(), gpui_font::is_supported)?,
   )?;
   fonts.set("list", Function::new(ctx.clone(), gpui_font::families)?)?;
   let owner = plugin.to_owned();
   fonts.set(
      "set",
      Function::new(ctx.clone(), move |choice: Opt<Object<'js>>| {
         let get = |key: &str| {
            choice
               .0
               .as_ref()
               .and_then(|o| o.get::<_, Option<String>>(key).ok().flatten())
               .unwrap_or_default()
         };
         gpui_font::set(&get("main"), &get("mono"));
         *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = Some(owner.clone());
      })?,
   )?;
   fonts.set(
      "clear",
      Function::new(ctx.clone(), || {
         *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = None;
         gpui_font::clear();
      })?,
   )?;
   Ok(fonts)
}
