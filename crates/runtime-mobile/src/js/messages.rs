//! `phoenix.commands`, send interceptors and emoji and sticker picks.

use std::cell::RefCell;

use phoenix_runtime::{
   js::{
      Engine,
      Registry,
      checked_icon,
      restart_budget,
   },
   permissions::{
      Permission,
      Permissions,
   },
};
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use crate::{
   js::{
      Mobile,
      ui::{
         context_object,
         remover,
         ui_object,
      },
   },
   patches::{
      self,
      Sent,
      UiContext,
      commands::Command,
   },
};

/// Runs the slash command `text` names, if any, then every send
/// interceptor.
pub fn submit(engine: &Engine, text: &str, context: &UiContext) -> Sent {
   engine.with(|ctx| {
      let (Some(core), Some(registry)) =
         (ctx.userdata::<Registry<'_>>(), ctx.userdata::<Mobile<'_>>())
      else {
         return Sent::Unchanged;
      };
      let argument = match context_object(&ctx, context) {
         Ok(argument) => argument,
         Err(err) => {
            log::error!("send context: {err}");
            return Sent::Unchanged;
         },
      };
      let mut value = text.to_owned();
      let invoked = text.strip_prefix('/').and_then(|rest| {
         let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
         let commands = registry.commands.borrow();
         let (plugin, execute) = commands.get(&name.to_lowercase())?;
         Some((
            name.to_owned(),
            args.trim().to_owned(),
            plugin.clone(),
            execute.clone(),
         ))
      });
      if let Some((name, args, plugin, execute)) = invoked {
         let result = match execute
            .call::<_, Value<'_>>((args, argument.clone()))
            .catch(&ctx)
         {
            Ok(result) => result,
            Err(err) => {
               log::error!("[{plugin}] /{name}: {err}");
               return Sent::Cancel;
            },
         };
         if result.is_promise() {
            return Sent::Cancel;
         }
         let outgoing = match result.as_object() {
            Some(object) if !result.is_string() => {
               object.get::<_, Option<String>>("content").ok().flatten()
            },
            _ => result.as_string().and_then(|text| text.to_string().ok()),
         };
         match outgoing {
            Some(outgoing) if !outgoing.is_empty() => value = outgoing,
            _ => return Sent::Cancel,
         }
      }
      let interceptors = core
         .interceptors
         .borrow()
         .iter()
         .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
         .collect::<Vec<_>>();
      drop((core, registry));
      for (plugin, function) in interceptors {
         restart_budget(&ctx);
         match function
            .call::<_, Value<'_>>((value.clone(), argument.clone()))
            .catch(&ctx)
         {
            Ok(result) if result.as_bool() == Some(false) => return Sent::Cancel,
            Ok(result) => {
               if let Some(text) = result.as_string().and_then(|text| text.to_string().ok()) {
                  value = text;
               }
            },
            Err(err) => log::error!("[{plugin}] onSend: {err}"),
         }
      }
      if value == text {
         Sent::Unchanged
      } else {
         Sent::Text(value)
      }
   })
}

/// Asks the `onEmojiPick` handlers about a press on custom emoji `id` in
/// the picker. Returns the text the first one puts in the message box.
pub fn pick_emoji(engine: &Engine, id: &str, context: &UiContext) -> Option<String> {
   ask(engine, Picker::Emoji, id, context, |result| {
      result.as_string().and_then(|text| text.to_string().ok())
   })
}

/// Whether an `onStickerPick` handler took the press on sticker `id`.
pub fn pick_sticker(engine: &Engine, id: &str, context: &UiContext) -> bool {
   ask(engine, Picker::Sticker, id, context, |result| {
      result.as_bool().filter(|taken| *taken)
   })
   .is_some()
}

/// The first answer of `picker`'s handlers that `answer` takes.
#[expect(
   clippy::needless_collect,
   reason = "the registry borrow must drop before the handlers run"
)]
fn ask<T>(
   engine: &Engine,
   picker: Picker,
   id: &str,
   context: &UiContext,
   answer: impl Fn(Value<'_>) -> Option<T>,
) -> Option<T> {
   engine.with(|ctx| {
      let registry = ctx.userdata::<Mobile<'_>>()?;
      let argument = context_object(&ctx, context).ok()?;
      let handlers = picker
         .handlers(&registry)
         .borrow()
         .iter()
         .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
         .collect::<Vec<_>>();
      drop(registry);
      handlers.into_iter().find_map(|(plugin, function)| {
         match function
            .call::<_, Value<'_>>((id, argument.clone()))
            .catch(&ctx)
         {
            Ok(result) => answer(result),
            Err(err) => {
               log::error!("[{plugin}] {}: {err}", picker.api());
               None
            },
         }
      })
   })
}

/// `phoenix.ui`, `phoenix.commands` and `phoenix.messages.onSend`.
pub fn patch_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   permissions: &Permissions,
   api: &Object<'js>,
   messages: &Object<'js>,
) -> rquickjs::Result<()> {
   let name = plugin.to_owned();
   messages.set(
      "onSend",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| on_send(&ctx, &name, function),
      )?,
   )?;
   let name = plugin.to_owned();
   messages.set(
      "onEmojiPick",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| {
            on_pick(&ctx, &name, function, Picker::Emoji)
         },
      )?,
   )?;
   let name = plugin.to_owned();
   messages.set(
      "onStickerPick",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| {
            on_pick(&ctx, &name, function, Picker::Sticker)
         },
      )?,
   )?;
   api.set(
      "ui",
      ui_object(ctx, plugin, permissions.allows(Permission::Hooks))?,
   )?;
   api.set("commands", commands_object(ctx, plugin)?)
}

/// `phoenix.commands`, slash commands typed in the message box.
fn commands_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let commands = Object::new(ctx.clone())?;
   let owner = plugin.to_owned();
   commands.set(
      "register",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         let name = options
            .get::<_, Option<String>>("name")?
            .map(|name| name.trim_start_matches('/').to_lowercase())
            .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace));
         let execute = options.get::<_, Option<Function<'js>>>("execute")?;
         let (Some(name), Some(execute)) = (name, execute) else {
            return Err(Exception::throw_type(
               &ctx,
               "commands.register({name, execute}) expected, with a name without spaces",
            ));
         };
         ctx.userdata::<Mobile<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .commands
            .borrow_mut()
            .insert(name.clone(), (owner.clone(), execute.clone()));
         patches::commands::add_command(Command {
            name:        name.clone(),
            description: options
               .get::<_, Option<String>>("description")?
               .unwrap_or_default(),
            icon:        checked_icon(&ctx, &owner, options.get("icon")?)?,
            plugin:      owner.clone(),
         });
         remover(
            &ctx,
            move |ctx| {
               if let Some(registry) = ctx.userdata::<Mobile<'js>>() {
                  let mut commands = registry.commands.borrow_mut();
                  if commands
                     .get(&name)
                     .is_some_and(|(_, current)| *current == execute)
                  {
                     commands.remove(&name);
                     patches::commands::remove_command(&name);
                  }
               }
            },
            None,
         )
      })?,
   )?;
   commands.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let list = rquickjs::Array::new(ctx.clone())?;
         for (index, command) in patches::commands::commands().into_iter().enumerate() {
            let entry = Object::new(ctx.clone())?;
            entry.set("name", command.name)?;
            entry.set("description", command.description)?;
            entry.set("plugin", command.plugin)?;
            list.set(index, entry)?;
         }
         Ok::<_, rquickjs::Error>(list)
      })?,
   )?;
   Ok(commands)
}

fn on_send<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .interceptors
      .borrow_mut()
      .push((id, plugin.to_owned(), function));
   patches::set_interceptors(registry.interceptors.borrow().len());
   drop(registry);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            let mut interceptors = registry.interceptors.borrow_mut();
            interceptors.retain(|(existing, ..)| *existing != id);
            patches::set_interceptors(interceptors.len());
         }
      },
      None,
   )
}

#[derive(Clone, Copy)]
pub enum Picker {
   Emoji,
   Sticker,
}

impl Picker {
   pub const ALL: [Self; 2] = [Self::Emoji, Self::Sticker];

   pub const fn handlers<'a, 'js>(
      self,
      registry: &'a Mobile<'js>,
   ) -> &'a RefCell<Vec<(u64, String, Function<'js>)>> {
      match self {
         Self::Emoji => &registry.pickers,
         Self::Sticker => &registry.sticker_picks,
      }
   }

   const fn api(self) -> &'static str {
      match self {
         Self::Emoji => "onEmojiPick",
         Self::Sticker => "onStickerPick",
      }
   }

   pub fn count(self, count: usize) {
      match self {
         Self::Emoji => patches::set_emoji_pickers(count),
         Self::Sticker => patches::set_sticker_pickers(count),
      }
   }
}

fn on_pick<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   function: Function<'js>,
   picker: Picker,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Mobile<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   let mut handlers = picker.handlers(&registry).borrow_mut();
   handlers.push((id, plugin.to_owned(), function));
   picker.count(handlers.len());
   drop(handlers);
   drop(registry);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Mobile<'js>>() {
            let mut handlers = picker.handlers(&registry).borrow_mut();
            handlers.retain(|(existing, ..)| *existing != id);
            picker.count(handlers.len());
         }
      },
      None,
   )
}
