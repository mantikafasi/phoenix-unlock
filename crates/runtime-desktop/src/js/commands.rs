//! `phoenix.commands` on desktop: the commands [`crate::commands`] lists in
//! Discord's `/` list, and running them when picked or typed out.

use phoenix_runtime::js::restart_budget;
use rquickjs::{
   Array,
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use crate::{
   commands::{
      self,
      Entry,
   },
   discord,
   js::{
      Desktop,
      ui,
   },
};

pub fn object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   let owner = plugin.to_owned();
   object.set(
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
         let description = options
            .get::<_, Option<String>>("description")?
            .unwrap_or_default();
         let registry = ctx
            .userdata::<Desktop<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?;
         registry
            .commands
            .borrow_mut()
            .insert(name.clone(), Registered {
               plugin: owner.clone(),
               description,
               execute: execute.clone(),
            });
         sync(&registry);
         drop(registry);
         Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
               return;
            };
            let mut registered = registry.commands.borrow_mut();
            if registered
               .get(&name)
               .is_some_and(|current| current.execute == execute)
            {
               registered.remove(&name);
               drop(registered);
               sync(&registry);
            }
         })
      })?,
   )?;
   object.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let list = Array::new(ctx.clone())?;
         for (at, entry) in commands::entries().into_iter().enumerate() {
            let info = Object::new(ctx.clone())?;
            info.set("name", entry.name)?;
            info.set("description", entry.description)?;
            info.set("plugin", entry.plugin)?;
            list.set(at, info)?;
         }
         Ok::<_, rquickjs::Error>(list)
      })?,
   )?;
   Ok(object)
}

/// A `phoenix.commands.register` call still in effect.
pub struct Registered<'js> {
   pub plugin:      String,
   pub description: String,
   pub execute:     Function<'js>,
}

fn sync(registry: &Desktop<'_>) {
   let mut entries = registry
      .commands
      .borrow()
      .iter()
      .map(|(name, registered)| {
         Entry {
            name:        name.clone(),
            description: registered.description.clone(),
            plugin:      registered.plugin.clone(),
         }
      })
      .collect::<Vec<_>>();
   entries.sort_by(|left, right| left.name.cmp(&right.name));
   commands::sync(entries);
}

pub fn unload(registry: &Desktop<'_>, plugin: &str) {
   let mut registered = registry.commands.borrow_mut();
   let before = registered.len();
   registered.retain(|_, command| command.plugin != plugin);
   let changed = registered.len() != before;
   drop(registered);
   if changed {
      sync(registry);
   }
}

/// What a command's `execute` asked to send.
pub enum Outcome {
   /// No plugin registered the name.
   Unknown,
   Nothing,
   Send(String),
}

/// The command `text` types out, like `/name args`.
pub fn typed(text: &str) -> Option<(&str, &str)> {
   let rest = text.strip_prefix('/')?;
   let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
   Some((name, args.trim()))
}

/// Runs the command `name` with `args` for a message to `channel`.
pub fn run(ctx: &Ctx<'_>, name: &str, args: &str, channel: &str) -> Outcome {
   let Some(registry) = ctx.userdata::<Desktop<'_>>() else {
      return Outcome::Unknown;
   };
   let Some((plugin, execute)) = registry
      .commands
      .borrow()
      .get(&name.to_lowercase())
      .map(|command| (command.plugin.clone(), command.execute.clone()))
   else {
      return Outcome::Unknown;
   };
   drop(registry);
   let Ok(context) = ui::send_context(ctx, channel) else {
      return Outcome::Nothing;
   };
   restart_budget(ctx);
   let result = match execute.call::<_, Value<'_>>((args, context)).catch(ctx) {
      Ok(result) => result,
      Err(err) => {
         log::error!("[{plugin}] /{name}: {err}");
         return Outcome::Nothing;
      },
   };
   if result.is_promise() {
      return Outcome::Nothing;
   }
   let outgoing = match result.as_object() {
      Some(object) if !result.is_string() => {
         object.get::<_, Option<String>>("content").ok().flatten()
      },
      _ => result.as_string().and_then(|text| text.to_string().ok()),
   };
   outgoing
      .filter(|text| !text.is_empty())
      .map_or(Outcome::Nothing, Outcome::Send)
}

/// Runs a command picked from Discord's list, then sends what it returns
/// through the `onSend` hooks as the user's message.
pub fn run_and_send(name: &str, args: &str, channel: &str) {
   let Some(text) = phoenix_runtime::engine().and_then(|engine| {
      engine.with(|ctx| {
         let Outcome::Send(text) = run(&ctx, name, args, channel) else {
            return None;
         };
         ui::intercept_text(&ctx, channel, text)
      })
   }) else {
      return;
   };
   let Ok(body) = serde_json::to_vec(&Message { content: &text }) else {
      return;
   };
   let headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
   if let Err(err) = discord::send(
      "POST",
      &format!("/channels/{channel}/messages"),
      headers,
      body,
   ) {
      log::error!("/{name}: {err}");
   }
}

#[derive(serde::Serialize)]
struct Message<'a> {
   content: &'a str,
}
