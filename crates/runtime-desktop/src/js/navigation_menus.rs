use std::sync::atomic::{
   AtomicU64,
   Ordering,
};

use phoenix_runtime::js::{
   Engine,
   checked_icon,
};
use phoenix_sigs::locate::surfaces::Menu;
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use super::Desktop;
use crate::{
   context_menu::ActionMeta,
   navigation_menus,
};

pub struct Handler<'js> {
   id:       u64,
   plugin:   String,
   callback: Function<'js>,
}

fn find_menu(name: &str) -> Option<Menu> {
   Menu::ALL.into_iter().find(|menu| menu.name() == name)
}

pub fn bind<'js>(ctx: &Ctx<'js>, ui: &Object<'js>, plugin: &str) -> rquickjs::Result<()> {
   ui.set(
      "hasContextMenu",
      Function::new(ctx.clone(), |name: String| {
         find_menu(&name).is_some_and(navigation_menus::ready)
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "addContextMenuAction",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, name: String, options: Object<'js>| {
            let menu = find_menu(&name).ok_or_else(|| {
               Exception::throw_type(&ctx, &format!("unknown context menu: {name}"))
            })?;
            register(&ctx, &owner, menu, &options)
         },
      )?,
   )
}

fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   menu: Menu,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT_ID: AtomicU64 = AtomicU64::new(1);
   if !navigation_menus::ready(menu) {
      return Err(Exception::throw_message(
         ctx,
         &format!("{} context menu is unavailable", menu.name()),
      ));
   }
   let registry = ctx
      .userdata::<Desktop<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no desktop registry"))?;
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let callback = options.get::<_, Function<'js>>("onPress")?;
   let action = ActionMeta {
      id,
      label: options.get("label")?,
      icon: checked_icon(ctx, plugin, options.get::<_, Option<String>>("icon")?)?,
      danger: options
         .get::<_, Option<bool>>("danger")?
         .unwrap_or_default(),
      priority: options
         .get::<_, Option<i32>>("priority")?
         .unwrap_or_default(),
      has_visible: false,
      parent: None,
   };
   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'_>>() {
         registry
            .navigation_actions
            .borrow_mut()
            .retain(|entry| entry.id != id);
      }
      navigation_menus::remove(id);
   })?;
   registry.navigation_actions.borrow_mut().push(Handler {
      id,
      plugin: plugin.to_owned(),
      callback,
   });
   navigation_menus::add(menu, action);
   Ok(remove)
}

pub fn unload(registry: &Desktop<'_>, plugin: &str) {
   registry.navigation_actions.borrow_mut().retain(|entry| {
      if entry.plugin == plugin {
         navigation_menus::remove(entry.id);
         false
      } else {
         true
      }
   });
}

pub fn press(engine: &Engine, id: u64, menu: Menu, target: u64) {
   engine.with(|ctx| {
      let entry = ctx.userdata::<Desktop<'_>>().and_then(|registry| {
         let handlers = registry.navigation_actions.borrow();
         let entry = handlers.iter().find(|entry| entry.id == id)?;
         Some((entry.plugin.clone(), entry.callback.clone()))
      });
      let Some((plugin, callback)) = entry else {
         return;
      };
      let result = (|| {
         let argument = Object::new(ctx.clone())?;
         argument.set("menu", menu.name())?;
         if target >= 1 << 48 {
            argument.set("targetId", target.to_string())?;
         } else {
            argument.set("targetId", Value::new_null(ctx.clone()))?;
         }
         callback.call::<_, Value<'_>>((argument,))
      })();
      if let Err(err) = result.catch(&ctx) {
         log::error!("[{plugin}] {} context menu: {err}", menu.name());
      }
   });
}

#[cfg(test)]
mod tests {
   use phoenix_runtime::js::compile;
   use phoenix_sigs::registry::GatewayRegistry;
   use rquickjs::Persistent;

   use super::*;

   #[test]
   fn routes_a_navigation_action_and_drops_it_on_plugin_unload() {
      navigation_menus::test_slot();
      let engine = Engine::new(&GatewayRegistry::default()).unwrap();
      let result = engine.with(|ctx| {
         assert!(ctx.store_userdata(Desktop::default()).is_ok());
         let api = Object::new(ctx.clone()).unwrap();
         bind(&ctx, &api, "test").unwrap();
         let result = Object::new(ctx.clone()).unwrap();
         api.set("result", result.clone()).unwrap();
         let code = r#"
            if (!phoenix.hasContextMenu("user")) throw new Error("missing user context menu");
            phoenix.addContextMenuAction("user", {
               label: "probe",
               onPress(ctx) { phoenix.result.target = ctx.targetId; },
            });
         "#;
         compile(&ctx, code).unwrap().call::<_, ()>((api,)).unwrap();
         Persistent::save(&ctx, result)
      });
      let id = engine.with(|ctx| {
         ctx.userdata::<Desktop<'_>>()
            .unwrap()
            .navigation_actions
            .borrow()[0]
            .id
      });
      press(&engine, id, Menu::User, 9_007_199_254_740_993);
      engine.with(|ctx| {
         let received = result.restore(&ctx).unwrap();
         assert_eq!(
            received.get::<_, String>("target").unwrap(),
            "9007199254740993"
         );
         let registry = ctx.userdata::<Desktop<'_>>().unwrap();
         unload(&registry, "test");
         assert!(registry.navigation_actions.borrow().is_empty());
      });
   }
}
