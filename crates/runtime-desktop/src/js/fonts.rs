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

/// The plugins whose main and code fonts, and whose Japanese fallback, are
/// showing, so unloading one brings back Discord's.
struct Owners {
   fonts:    Option<String>,
   japanese: Option<String>,
}

static OWNERS: Mutex<Owners> = Mutex::new(Owners {
   fonts:    None,
   japanese: None,
});

/// Brings back Discord's fonts where `plugin` set them.
pub fn unload(plugin: &str) {
   let mut owners = OWNERS.lock().unwrap_or_else(PoisonError::into_inner);
   let fonts = owners.fonts.take_if(|owner| owner == plugin).is_some();
   let japanese = owners.japanese.take_if(|owner| owner == plugin).is_some();
   drop(owners);
   if fonts {
      gpui_font::clear();
   }
   if japanese {
      gpui_font::set_japanese(&[]);
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
         };
         let (main, mono) = (get("main"), get("mono"));
         let japanese = choice
            .0
            .as_ref()
            .and_then(|o| o.get::<_, Option<Vec<String>>>("japanese").ok().flatten());
         let fonts = main.is_some() || mono.is_some() || japanese.is_none();
         if fonts {
            gpui_font::set(&main.unwrap_or_default(), &mono.unwrap_or_default());
         }
         if let Some(families) = &japanese {
            gpui_font::set_japanese(families);
         }
         let mut owners = OWNERS.lock().unwrap_or_else(PoisonError::into_inner);
         if fonts {
            owners.fonts = Some(owner.clone());
         }
         if japanese.is_some() {
            owners.japanese = Some(owner.clone());
         }
      })?,
   )?;
   let owner = plugin.to_owned();
   fonts.set("clear", Function::new(ctx.clone(), move || unload(&owner))?)?;
   Ok(fonts)
}
