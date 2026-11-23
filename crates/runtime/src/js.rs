use std::{
   borrow::Cow,
   cell::RefCell,
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
   },
   fs,
   io::ErrorKind,
   path::PathBuf,
};

use log::Level;
use phoenix_sigs::{
   EventId,
   GatewayRegistry,
};
#[cfg(target_os = "android")] use rquickjs::function::Opt;
use rquickjs::{
   CatchResultExt,
   CaughtError,
   Context,
   Ctx,
   Exception,
   Function,
   IntoJs,
   JsLifetime,
   Object,
   Runtime,
   Value,
   function::Rest,
   object::Accessor,
};
use serde_json::value::RawValue;

use crate::{
   Choice,
   Chosen,
   Control,
   Error,
   Header,
   Plugin,
   PluginInfo,
   STATE,
   Setting,
   State,
   gateway,
   io_error,
   local,
};
#[cfg(target_os = "android")]
use std::str::FromStr;

#[cfg(target_os = "android")]
use crate::{
   discord,
   fetch,
   patches::{
      self,
      Badge,
      Command,
      Contribution,
      ContributionId,
      NodeInfo,
      NodeMatch,
      Outcome,
      PatchTarget,
      Place,
      Rendered,
      Sent,
      UiContext,
      targets::Target,
   },
   store,
};

pub struct Engine {
   runtime: Runtime,
   context: Context,
   plugins: BTreeMap<String, Loaded>,
}

struct Loaded {
   source:    String,
   enabled:   bool,
   error:     Option<String>,
   /// Read from `home/plugins`, so the user can delete it.
   installed: bool,
}

type ChosenMap = BTreeMap<String, BTreeMap<String, Chosen>>;

const fn default_true() -> bool {
   true
}

/// Which plugins the user turned off and how they set each plugin's
/// settings, kept in `home/plugins.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct Choices<'a> {
   #[serde(default)]
   disabled:     BTreeSet<String>,
   #[serde(default)]
   settings:     Cow<'a, ChosenMap>,
   #[serde(default = "default_true")]
   staff:        bool,
   #[serde(default)]
   remote_shell: bool,
}

impl Default for Choices<'_> {
   fn default() -> Self {
      Self {
         disabled:     BTreeSet::new(),
         settings:     Cow::Owned(BTreeMap::new()),
         staff:        true,
         remote_shell: false,
      }
   }
}

/// One setting a plugin defined with `phoenix.settings.define`.
struct Definition<'js> {
   key:         String,
   title:       String,
   description: Option<String>,
   kind:        Kind<'js>,
   on_change:   Option<Function<'js>>,
}

/// A setting's type with its default, or a button's handler.
enum Kind<'js> {
   Switch(bool),
   Text(String),
   Secret(String),
   Select {
      options: Vec<Choice>,
      default: String,
   },
   Number(f64),
   Button(Function<'js>),
}

impl Kind<'_> {
   /// The user's choice when it has this kind's type, else the default.
   fn current(&self, chosen: Option<&Chosen>) -> Control {
      match (self, chosen) {
         (Self::Switch(_), Some(Chosen::Switch(on))) => Control::Switch(*on),
         (Self::Switch(default), _) => Control::Switch(*default),
         (Self::Text(_), Some(Chosen::Text(text))) => Control::Text(text.clone()),
         (Self::Text(default), _) => Control::Text(default.clone()),
         (Self::Secret(_), Some(Chosen::Text(text))) => Control::Secret(text.clone()),
         (Self::Secret(default), _) => Control::Secret(default.clone()),
         (Self::Select { options, default }, chosen) => {
            let chosen = match chosen {
               Some(Chosen::Text(value)) if options.iter().any(|choice| choice.value == *value) => {
                  value.clone()
               },
               _ => default.clone(),
            };
            Control::Select {
               options: options.clone(),
               chosen,
            }
         },
         (Self::Number(_), Some(Chosen::Number(number))) => Control::Number(*number),
         (Self::Number(default), _) => Control::Number(*default),
         (Self::Button(_), _) => Control::Button,
      }
   }
}

pub enum Before {
   Keep,
   Drop,
   Replace(String),
}

/// What plugins registered. It lives in the runtime's userdata, so callbacks
/// stay JS values owned by the runtime, and the only thing plugins can reach is
/// their own `phoenix` object.
struct Registry<'js> {
   gateway:      Object<'js>,
   handles:      Vec<(EventId, Object<'js>)>,
   hooks:        RefCell<HashMap<EventId, Vec<Hook<'js>>>>,
   defined:      RefCell<BTreeMap<String, Vec<Definition<'js>>>>,
   /// Kept across unloads, so a reloaded plugin sees what the user chose.
   chosen:       RefCell<ChosenMap>,
   /// `phoenix.onUnload` callbacks, run when the plugin is turned off,
   /// reloaded or removed.
   unloads:      RefCell<HashMap<String, Vec<Function<'js>>>>,
   /// The resolve and reject of each promise a worker thread will settle, by
   /// id.
   #[cfg(target_os = "android")]
   pending:      RefCell<HashMap<u64, (Function<'js>, Function<'js>)>>,
   #[cfg(target_os = "android")]
   ui:           RefCell<HashMap<ContributionId, Callbacks<'js>>>,
   /// Slash commands by lowercase name, with the plugin that registered each.
   #[cfg(target_os = "android")]
   commands:     RefCell<HashMap<String, (String, Function<'js>)>>,
   /// `phoenix.messages.onSend` interceptors in registration order, by id.
   #[cfg(target_os = "android")]
   interceptors:  RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.discord.onRequest` interceptors, by id.
   #[cfg(target_os = "android")]
   request_hooks: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// `phoenix.messages.onEmojiPick` handlers in registration order, by id.
   #[cfg(target_os = "android")]
   pickers:       RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.ui.onEvent` handlers, by id.
   #[cfg(target_os = "android")]
   event_hooks:   RefCell<Vec<(u64, String, phoenix_wire::Key, bool, Function<'js>)>>,
   /// `phoenix.ui.onNode` watchers, by id.
   #[cfg(target_os = "android")]
   node_watchers: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// Open full-screen plugin pages (`phoenix.ui.openPage`) and their button
   /// callbacks, by page id.
   #[cfg(target_os = "android")]
   open_pages:    RefCell<HashMap<u32, (String, Vec<Function<'js>>)>>,
   /// Active in-app notification banner tap callbacks (`phoenix.ui.notify`), by banner id.
   #[cfg(target_os = "android")]
   banners:       RefCell<HashMap<u32, (String, Function<'js>)>>,
}

/// A contribution's plugin callbacks. `evaluate` is `render`, or `visible`
/// for a message action.
#[cfg(target_os = "android")]
struct Callbacks<'js> {
   plugin:   String,
   target:   Target,
   press:    Option<Function<'js>>,
   evaluate: Option<Function<'js>>,
   /// A profile tab's page callbacks, by profile, as `Block` handlers index
   /// them.
   pages:    HashMap<Place, Vec<Function<'js>>>,
}

/// Profiles whose page callbacks a tab keeps.
#[cfg(target_os = "android")]
const PAGES_CAP: usize = 64;

// SAFETY: every field is bound to 'js, and Changed swaps exactly that lifetime.
unsafe impl<'js> JsLifetime<'js> for Registry<'js> {
   type Changed<'to> = Registry<'to>;
}

#[derive(Clone)]
struct Hook<'js> {
   plugin: String,
   before: Option<Function<'js>>,
   after:  Option<Function<'js>>,
}

#[expect(clippy::needless_pass_by_value, reason = "used as a map_err callback")]
fn js_error(err: CaughtError<'_>) -> Error {
   Error::Js(err.to_string())
}

fn throw(ctx: &Ctx<'_>, err: &Error) -> rquickjs::Error {
   Exception::throw_message(ctx, &err.to_string())
}

fn state() -> &'static State {
   STATE
      .get()
      .expect("natives run after start() publishes the state")
}

impl Engine {
   pub fn new(events: &GatewayRegistry) -> Result<Self, Error> {
      let runtime = Runtime::new().map_err(|err| Error::Js(err.to_string()))?;
      runtime.set_max_stack_size(512 * 1024);
      let context = Context::full(&runtime).map_err(|err| Error::Js(err.to_string()))?;
      context.with(|ctx| {
         let registry = registry(&ctx, events).catch(&ctx).map_err(js_error)?;
         ctx.store_userdata(registry)
            .map_err(|err| Error::Js(err.to_string()))?;
         Ok::<_, Error>(())
      })?;
      Ok(Self {
         runtime,
         context,
         plugins: BTreeMap::new(),
      })
   }

   pub fn source(&self, name: &str) -> Option<String> {
      self.plugins.get(name).map(|loaded| loaded.source.clone())
   }

   pub fn plugins(&self) -> Vec<PluginInfo> {
      self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>();
         let defined = registry.as_ref().map(|registry| registry.defined.borrow());
         let chosen = registry.as_ref().map(|registry| registry.chosen.borrow());
         self
            .plugins
            .iter()
            .map(|(name, loaded)| {
               let settings = defined
                  .as_ref()
                  .and_then(|defined| defined.get(name))
                  .into_iter()
                  .flatten()
                  .map(|definition| {
                     let picked = chosen
                        .as_ref()
                        .and_then(|chosen| chosen.get(name)?.get(&definition.key));
                     Setting {
                        key:         definition.key.clone(),
                        title:       definition.title.clone(),
                        description: definition.description.clone(),
                        control:     definition.kind.current(picked),
                     }
                  })
                  .collect();
               PluginInfo {
                  name: name.clone(),
                  enabled: loaded.enabled,
                  error: loaded.error.clone(),
                  installed: loaded.installed,
                  header: Header::from(loaded.source.as_str()),
                  settings,
               }
            })
            .collect()
      })
   }

   pub fn set_setting(&self, plugin: &str, key: &str, value: Chosen) {
      self.context.with(|ctx| {
         let callback = if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry
               .chosen
               .borrow_mut()
               .entry(plugin.to_owned())
               .or_default()
               .insert(key.to_owned(), value.clone());
            redact_secrets(&registry);
            registry
               .defined
               .borrow()
               .get(plugin)
               .and_then(|list| list.iter().find(|definition| definition.key == key))
               .and_then(|definition| definition.on_change.clone())
         } else {
            None
         };
         if let Some(callback) = callback {
            let js_value = match &value {
               Chosen::Switch(on) => Ok(Value::new_bool(ctx.clone(), *on)),
               Chosen::Number(number) => Ok(Value::new_float(ctx.clone(), *number)),
               Chosen::Text(text) => rquickjs::String::from_str(ctx.clone(), text).map(rquickjs::String::into_value),
            };
            if let Ok(js_value) = js_value
               && let Err(err) = callback.call::<_, Value<'_>>((js_value,)).catch(&ctx)
            {
               log::error!("[{plugin}] {key} onChange: {err}");
            }
         }
      });
      drain(&self.runtime);
      self.save_choices();
   }

   pub fn press(&self, plugin: &str, key: &str) {
      self.context.with(|ctx| {
         let handler = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .defined
               .borrow()
               .get(plugin)?
               .iter()
               .find(|definition| definition.key == key)
               .and_then(|definition| {
                  match &definition.kind {
                     Kind::Button(handler) => Some(handler.clone()),
                     _ => None,
                  }
               })
         });
         if let Some(handler) = handler
            && let Err(err) = handler.call::<_, ()>(()).catch(&ctx)
         {
            log::error!("[{plugin}] {key}: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Asks contribution `id` what it shows at `place`. A promise answers
   /// later through [`patches::evaluated`], and this returns `None`.
   #[cfg(target_os = "android")]
   pub fn ui_evaluate(
      &self,
      id: ContributionId,
      place: &Place,
      context: &UiContext,
   ) -> Option<Outcome> {
      let outcome = self.context.with(|ctx| {
         let (plugin, target, evaluate) = {
            let registry = ctx.userdata::<Registry<'_>>()?;
            let callbacks = registry.ui.borrow();
            let callbacks = callbacks.get(&id)?;
            (
               callbacks.plugin.clone(),
               callbacks.target,
               callbacks.evaluate.clone()?,
            )
         };
         let answer = |value| {
            if target == Target::Tab {
               page_of(&ctx, id, place, value)
            } else {
               outcome_of(target, value)
            }
         };
         let failed = || answer(None);
         let argument = match context_object(&ctx, context) {
            Ok(argument) => argument,
            Err(err) => {
               log::error!("[{plugin}] ui context: {err}");
               return Some(failed());
            },
         };
         let result = match evaluate.call::<_, Value<'_>>((argument,)).catch(&ctx) {
            Ok(result) => result,
            Err(err) => {
               log::error!("[{plugin}] render: {err}");
               return Some(failed());
            },
         };
         let Some(promise) = result.as_promise() else {
            return Some(answer(Some(&result)));
         };
         let settle = |place: Place, plugin: String| {
            let settled = place.clone();
            let ok = Function::new(ctx.clone(), move |value: Value<'_>| {
               let outcome = if target == Target::Tab {
                  page_of(value.ctx(), id, &settled, Some(&value))
               } else {
                  outcome_of(target, Some(&value))
               };
               patches::evaluated(id, settled.clone(), outcome);
            });
            let err = Function::new(ctx.clone(), move |reason: Value<'_>| {
               log::error!("[{plugin}] render: {reason:?}");
               let outcome = if target == Target::Tab {
                  page_of(reason.ctx(), id, &place, None)
               } else {
                  outcome_of(target, None)
               };
               patches::evaluated(id, place.clone(), outcome);
            });
            let then = promise.then()?;
            then.call::<_, ()>((rquickjs::function::This(promise.clone()), ok?, err?))
         };
         if let Err(err) = settle(place.clone(), plugin.clone()).catch(&ctx) {
            log::error!("[{plugin}] render: {err}");
            return Some(failed());
         }
         None
      });
      drain(&self.runtime);
      outcome
   }

   /// Runs handler `index` of the page contribution `id` drew at `place`,
   /// with the text of the box it belongs to, if any.
   #[cfg(target_os = "android")]
   pub fn ui_block(&self, id: ContributionId, place: &Place, index: u32, text: Option<String>) {
      self.context.with(|ctx| {
         let Some((plugin, handler)) = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            let callbacks = registry.ui.borrow();
            let callbacks = callbacks.get(&id)?;
            let handler = callbacks.pages.get(place)?.get(usize::try_from(index).ok()?)?;
            Some((callbacks.plugin.clone(), handler.clone()))
         }) else {
            return;
         };
         if let Err(err) = handler.call::<_, Value<'_>>((text,)).catch(&ctx) {
            log::error!("[{plugin}] page: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Runs button `index` on plugin full-screen page `page`.
   #[cfg(target_os = "android")]
   pub fn page_press(&self, page: u32, index: u32) {
      self.context.with(|ctx| {
         let Some((plugin, handler)) = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            let pages = registry.open_pages.borrow();
            let (plugin, handlers) = pages.get(&page)?;
            let handler = handlers.get(usize::try_from(index).ok()?)?;
            Some((plugin.clone(), handler.clone()))
         }) else {
            return;
         };
         if let Err(err) = handler.call::<_, Value<'_>>(()).catch(&ctx) {
            log::error!("[{plugin}] page button: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Drops button callbacks for a closed plugin full-screen page.
   #[cfg(target_os = "android")]
   pub fn page_closed(&self, page: u32) {
      self.context.with(|ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry.open_pages.borrow_mut().remove(&page);
         }
      });
   }

   /// Runs the tap callback for in-app notification banner `id` and cleans it up.
   #[cfg(target_os = "android")]
   pub fn banner_press(&self, id: u32) {
      self.context.with(|ctx| {
         let Some((plugin, handler)) = ctx
            .userdata::<Registry<'_>>()
            .and_then(|registry| registry.banners.borrow_mut().remove(&id))
         else {
            return;
         };
         if let Err(err) = handler.call::<_, Value<'_>>(()).catch(&ctx) {
            log::error!("[{plugin}] notify onPress: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Drops the tap callback for a dismissed in-app notification banner.
   #[cfg(target_os = "android")]
   pub fn banner_closed(&self, id: u32) {
      self.context.with(|ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry.banners.borrow_mut().remove(&id);
         }
      });
   }

   #[cfg(target_os = "android")]
   pub fn ui_press(&self, id: ContributionId, context: &UiContext) {
      self.context.with(|ctx| {
         let Some((plugin, press)) = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            let callbacks = registry.ui.borrow();
            let callbacks = callbacks.get(&id)?;
            Some((callbacks.plugin.clone(), callbacks.press.clone()?))
         }) else {
            return;
         };
         let run = || -> rquickjs::Result<()> {
            press.call::<_, Value<'_>>((context_object(&ctx, context)?,))?;
            Ok(())
         };
         if let Err(err) = run().catch(&ctx) {
            log::error!("[{plugin}] onPress: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Runs the slash command `text` names, if any, then every send
   /// interceptor.
   #[cfg(target_os = "android")]
   pub fn submit(&self, text: &str, context: &UiContext) -> Sent {
      let sent = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
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
         let interceptors = registry
            .interceptors
            .borrow()
            .iter()
            .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
            .collect::<Vec<_>>();
         drop(registry);
         for (plugin, function) in interceptors {
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
      });
      drain(&self.runtime);
      sent
   }

   /// Asks the `onEmojiPick` handlers about a press on custom emoji `id` in
   /// the picker. Returns the text the first one puts in the message box.
   #[cfg(target_os = "android")]
   pub fn pick_emoji(&self, id: &str, context: &UiContext) -> Option<String> {
      let picked = self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>()?;
         let argument = context_object(&ctx, context).ok()?;
         let pickers = registry
            .pickers
            .borrow()
            .iter()
            .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
            .collect::<Vec<_>>();
         drop(registry);
         pickers.into_iter().find_map(|(plugin, function)| {
            match function
               .call::<_, Value<'_>>((id, argument.clone()))
               .catch(&ctx)
            {
               Ok(result) => result.as_string().and_then(|text| text.to_string().ok()),
               Err(err) => {
                  log::error!("[{plugin}] onEmojiPick: {err}");
                  None
               },
            }
         })
      });
      drain(&self.runtime);
      picked
   }

   /// Runs `phoenix.ui.onEvent` handlers matching `event`. When `intercepted` is
   /// true, returns whether any intercepting handler consumed the event (true)
   /// or all returned `false` (false, re-injecting to Phoenix).
   #[cfg(target_os = "android")]
   pub fn view_event(
      &self,
      node: i32,
      event: phoenix_wire::Key,
      payload: &phoenix_wire::Value,
      aid: Option<&str>,
      context: Option<&UiContext>,
      intercepted: bool,
   ) -> bool {
      let consumed = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return false;
         };
         let handlers = registry
            .event_hooks
            .borrow()
            .iter()
            .filter(|(_, _, key, ..)| *key == event)
            .map(|(_, plugin, _, intercept, function)| {
               (plugin.clone(), *intercept, function.clone())
            })
            .collect::<Vec<_>>();
         drop(registry);
         if handlers.is_empty() {
            return false;
         }
         let Ok(ev) = Object::new(ctx.clone()) else {
            return false;
         };
         let event_name = phoenix_ui::PROPS
            .get(usize::from(event.get()))
            .copied()
            .unwrap_or_default();
         let _ = ev.set("node", node);
         let _ = ev.set("event", event_name);
         if let Ok(val) = wire_to_js(&ctx, payload) {
            let _ = ev.set("payload", val);
         }
         let _ = ev.set("accessibilityId", aid);
         if let Some(context) = context {
            let _ = ev.set("channelId", context.channel_id.clone());
            let _ = ev.set("currentUserId", context.current_user_id.clone());
            if let Some(id) = &context.message_id {
               let _ = ev.set("messageId", id.as_str());
            }
            if let Some(message) = &context.message
               && let Ok(parsed) = ctx.json_parse(message.as_str())
            {
               let _ = ev.set("message", parsed);
            }
         }
         let mut taken = false;
         for (plugin, is_interceptor, function) in handlers {
            match function.call::<_, Value<'_>>((ev.clone(),)).catch(&ctx) {
               Ok(result) if intercepted && is_interceptor && result.as_bool() != Some(false) => {
                  taken = true;
               },
               Ok(_) => {},
               Err(err) => log::error!("[{plugin}] onEvent({event_name}): {err}"),
            }
         }
         taken
      });
      drain(&self.runtime);
      consumed
   }

   /// Runs `phoenix.ui.onNode` watchers for a newly created node.
   #[cfg(target_os = "android")]
   pub fn node_created(&self, watcher_ids: &[u64], info: &NodeInfo) {
      self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return;
         };
         let handlers = {
            let watchers = registry.node_watchers.borrow();
            watcher_ids
               .iter()
               .filter_map(|id| {
                  let (plugin, function) = watchers.get(id)?;
                  Some((plugin.clone(), function.clone()))
               })
               .collect::<Vec<_>>()
         };
         drop(registry);
         if handlers.is_empty() {
            return;
         }
         let Ok(obj) = node_info_object(&ctx, info) else {
            return;
         };
         for (plugin, function) in handlers {
            if let Err(err) = function.call::<_, Value<'_>>((obj.clone(),)).catch(&ctx) {
               log::error!("[{plugin}] onNode: {err}");
            }
         }
      });
      drain(&self.runtime);
   }

   /// Evaluates `name` with a fresh `phoenix` object, recording any error.
   fn eval(&mut self, name: &str) {
      let Some(loaded) = self.plugins.get_mut(name) else {
         return;
      };
      let outcome = self.context.with(|ctx| {
         let run = || -> rquickjs::Result<()> {
            let phoenix = api(&ctx, name)?;
            let wrapped = format!("(function (phoenix) {{\n{}\n}})", loaded.source);
            ctx.eval::<Function<'_>, _>(wrapped)?.call((phoenix,))
         };
         run().catch(&ctx).map_err(js_error)
      });
      drain(&self.runtime);
      match outcome {
         Ok(()) => {
            log::info!("loaded plugin {name}");
            loaded.error = None;
         },
         Err(err) => {
            log::error!("plugin {name} failed to load: {err}");
            loaded.error = Some(err.to_string());
         },
      }
   }

   /// Drops every hook `name` registered, and turns off events nobody else
   /// listens to.
   fn unload(&self, name: &str) {
      self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return;
         };
         let cleanups = registry
            .unloads
            .borrow_mut()
            .remove(name)
            .unwrap_or_default();
         for cleanup in cleanups {
            if let Err(err) = cleanup.call::<_, ()>(()).catch(&ctx) {
               log::error!("[{name}] onUnload: {err}");
            }
         }
         registry.defined.borrow_mut().remove(name);
         #[cfg(target_os = "android")]
         {
            registry
               .ui
               .borrow_mut()
               .retain(|_, callbacks| callbacks.plugin != name);
            registry
               .commands
               .borrow_mut()
               .retain(|_, (plugin, _)| plugin != name);
            let mut interceptors = registry.interceptors.borrow_mut();
            interceptors.retain(|(_, plugin, _)| plugin != name);
            patches::set_interceptors(interceptors.len());
            drop(interceptors);
            registry
               .request_hooks
               .borrow_mut()
               .retain(|_, (plugin, _)| plugin != name);
            discord::forget_request_hooks(name);
            let mut pickers = registry.pickers.borrow_mut();
            pickers.retain(|(_, plugin, _)| plugin != name);
            patches::set_emoji_pickers(pickers.len());
            drop(pickers);
            registry
               .event_hooks
               .borrow_mut()
               .retain(|(_, plugin, ..)| plugin != name);
            registry
               .node_watchers
               .borrow_mut()
               .retain(|_, (plugin, _)| plugin != name);
            let mut open_pages = registry.open_pages.borrow_mut();
            let closed = open_pages
               .iter()
               .filter(|(_, (plugin, _))| plugin == name)
               .map(|(id, _)| *id)
               .collect::<Vec<_>>();
            for id in closed {
               open_pages.remove(&id);
               patches::close_page(id);
            }
            drop(open_pages);
            let mut banners = registry.banners.borrow_mut();
            let dismissed = banners
               .iter()
               .filter(|(_, (plugin, _))| plugin == name)
               .map(|(id, _)| *id)
               .collect::<Vec<_>>();
            for id in dismissed {
               banners.remove(&id);
               patches::dismiss_banner(id);
            }
            drop(banners);
            patches::forget(name);
         }
         let mut hooks = registry.hooks.borrow_mut();
         for (event, list) in hooks.iter_mut() {
            list.retain(|hook| hook.plugin != name);
            if list.is_empty() {
               gateway::uninstall(*event);
            }
         }
         hooks.retain(|_, list| !list.is_empty());
      });
      drain(&self.runtime);
   }

   pub fn set_enabled(&mut self, name: &str, enabled: bool) {
      let Some(loaded) = self.plugins.get_mut(name) else {
         return;
      };
      if loaded.enabled == enabled {
         return;
      }
      loaded.enabled = enabled;
      loaded.error = None;
      if enabled {
         self.eval(name);
      } else {
         self.unload(name);
      }
      self.save_choices();
   }

   /// Adds `name` or replaces its source, then runs it unless the user turned
   /// it off.
   #[cfg(target_os = "android")]
   pub fn install(&mut self, name: &str, source: String) {
      let enabled = if let Some(loaded) = self.plugins.get_mut(name) {
         loaded.source = source;
         loaded.installed = true;
         loaded.enabled
      } else {
         self.plugins.insert(name.to_owned(), Loaded {
            source,
            enabled: true,
            error: None,
            installed: true,
         });
         true
      };
      if enabled {
         self.unload(name);
         self.eval(name);
      }
   }

   /// Drops `name` and what the user chose for it. A bundled plugin it
   /// replaced comes back at the next start.
   #[cfg(target_os = "android")]
   pub fn uninstall(&mut self, name: &str) {
      self.unload(name);
      self.plugins.remove(name);
      self.context.with(|ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry.chosen.borrow_mut().remove(name);
         }
      });
      self.save_choices();
   }

   pub fn reload(&mut self, name: &str) {
      if self.plugins.get(name).is_some_and(|loaded| loaded.enabled) {
         self.unload(name);
         self.eval(name);
      }
   }

   pub fn save_choices(&self) {
      let disabled = self
         .plugins
         .iter()
         .filter(|(_, loaded)| !loaded.enabled)
         .map(|(name, _)| name.clone())
         .collect();
      #[cfg(target_os = "android")]
      let remote_shell = crate::remote_shell_enabled();
      #[cfg(not(target_os = "android"))]
      let remote_shell = false;
      let json = self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>();
         let chosen = registry.as_ref().map(|registry| registry.chosen.borrow());
         let empty = BTreeMap::new();
         serde_json::to_vec(&Choices {
            disabled,
            settings: Cow::Borrowed(chosen.as_deref().unwrap_or(&empty)),
            staff: crate::staff_enabled(),
            remote_shell,
         })
      });
      let path = state().choices_path();
      let write = || -> std::io::Result<()> {
         fs::create_dir_all(path.parent().expect("the choices path has a parent"))?;
         fs::write(&path, json?)
      };
      if let Err(err) = write() {
         log::error!("{}: {err}", path.display());
      }
   }

   /// The result as JSON when it has a JSON form, else as `String(result)`.
   /// The prompt sees a `phoenix` object of its own, named "console".
   pub fn evaluate(&self, code: &str) -> Result<String, Error> {
      let result = self.context.with(|ctx| {
         let run = || -> rquickjs::Result<String> {
            if !ctx.globals().contains_key("phoenix")? {
               ctx.globals().set("phoenix", api(&ctx, "console")?)?;
            }
            let result = ctx.eval::<Value<'_>, _>(code)?;
            if let Some(text) = result.as_string() {
               return text.to_string();
            }
            match ctx.json_stringify(result.clone())? {
               Some(json) => json.to_string(),
               None => {
                  ctx.globals()
                     .get::<_, Function<'_>>("String")?
                     .call((result,))
               },
            }
         };
         run().catch(&ctx).map_err(js_error)
      });
      drain(&self.runtime);
      result
   }

   /// Runs matching `phoenix.discord.onRequest` hooks on an outgoing Discord API request.
   #[cfg(target_os = "android")]
   pub fn intercept_request(
      &self,
      matched: &[u64],
      method: &str,
      path: &str,
      body: &[u8],
   ) -> discord::RequestAction {
      let action = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return discord::RequestAction::Pass;
         };
         let hooks = {
            let map = registry.request_hooks.borrow();
            matched
               .iter()
               .filter_map(|id| {
                  let (plugin, function) = map.get(id)?;
                  Some((plugin.clone(), function.clone()))
               })
               .collect::<Vec<_>>()
         };
         drop(registry);
         if hooks.is_empty() {
            return discord::RequestAction::Pass;
         }
         let mut current_body = String::from_utf8_lossy(body).into_owned();
         let mut changed = false;
         for (plugin, function) in hooks {
            let Ok(req) = Object::new(ctx.clone()) else {
               break;
            };
            let _ = req.set("method", method);
            let _ = req.set("path", path);
            let _ = req.set("body", current_body.as_str());
            let json_source = current_body.clone();
            if let Ok(json_fn) = json_parser_fn(&ctx, json_source) {
               let _ = req.set("json", json_fn);
            }
            match function.call::<_, Value<'_>>((req,)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => {
                  return discord::RequestAction::Drop;
               },
               Ok(ref result) if let Some(obj) = result.as_object() => {
                  if let Ok(new_body) = obj.get::<_, Value<'_>>("body")
                     && !new_body.is_undefined()
                     && !new_body.is_null()
                  {
                     if let Some(text) = new_body.as_string() {
                        if let Ok(text) = text.to_string() {
                           current_body = text;
                           changed = true;
                        }
                     } else if let Ok(Some(json)) = ctx.json_stringify(new_body)
                        && let Ok(text) = json.to_string()
                     {
                        current_body = text;
                        changed = true;
                     }
                  }
               },
               Ok(_) => {},
               Err(err) => log::error!("[{plugin}] onRequest: {err}"),
            }
         }
         if changed {
            discord::RequestAction::Replace(current_body.into_bytes())
         } else {
            discord::RequestAction::Pass
         }
      });
      drain(&self.runtime);
      action
   }

   /// Runs the `before` hooks for `event`. Each sees the previous hook's
   /// payload.
   pub fn before(&self, event: EventId, name: &str, json: &str) -> Result<Before, Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.before.as_ref());
         if hooks.is_empty() {
            return Ok(Before::Keep);
         }
         let mut payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, before) in hooks {
            match before.call::<_, Value<'_>>((payload.clone(),)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => return Ok(Before::Drop),
               Ok(result) if result.is_undefined() => {},
               Ok(result) => payload = result,
               Err(err) => log::error!("[{plugin}] before {name}: {err}"),
            }
         }
         let json = ctx
            .json_stringify(payload)
            .catch(&ctx)
            .map_err(js_error)?
            .ok_or_else(|| Error::Js(format!("a before {name} hook returned no JSON value")))?;
         Ok(Before::Replace(
            json.to_string().map_err(|err| Error::Js(err.to_string()))?,
         ))
      });
      drain(&self.runtime);
      result
   }

   pub fn after(&self, event: EventId, name: &str, json: &str, ok: bool) -> Result<(), Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.after.as_ref());
         if hooks.is_empty() {
            return Ok(());
         }
         let payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, after) in hooks {
            if let Err(err) = after.call::<_, ()>((payload.clone(), ok)).catch(&ctx) {
               log::error!("[{plugin}] after {name}: {err}");
            }
         }
         Ok(())
      });
      drain(&self.runtime);
      result
   }

   /// Settles promise `id` with what its worker thread produced, and runs what
   /// awaited it.
   #[cfg(target_os = "android")]
   pub fn settle(&self, id: u64, settlement: Settlement) {
      self.context.with(|ctx| {
         let Some((resolve, reject)) = ctx
            .userdata::<Registry<'_>>()
            .and_then(|registry| registry.pending.borrow_mut().remove(&id))
         else {
            return;
         };
         let value: rquickjs::Result<Result<Value<'_>, String>> = match settlement {
            Settlement::Fetch(Ok(response)) | Settlement::Discord(Ok(response)) => {
               response_object(&ctx, &response).map(|object| Ok(object.into_value()))
            },
            Settlement::Int(Ok(number)) => Ok(Ok(Value::new_int(ctx.clone(), number))),
            Settlement::Int(Err(err)) => Ok(Err(err)),
            Settlement::Text(Ok(text)) => rquickjs::String::from_str(ctx.clone(), &text).map(|text| Ok(text.into_value())),
            Settlement::Rows(Ok(rows)) => {
               serde_json::to_string(&rows)
                  .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))
                  .and_then(|json| ctx.json_parse(json))
                  .map(Ok)
            },
            Settlement::Fetch(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Discord(Err(err)) | Settlement::Text(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Rows(Err(err)) => Ok(Err(err.to_string())),
         };
         let outcome = value.and_then(|value| {
            match value {
               Ok(value) => resolve.call::<_, ()>((value,)),
               Err(message) => {
                  reject.call::<_, ()>((Exception::from_message(ctx.clone(), &message)?,))
               },
            }
         });
         if let Err(err) = outcome.catch(&ctx) {
            log::error!("settling promise {id}: {err}");
         }
      });
      drain(&self.runtime);
   }
}

/// Runs promise reactions until none are left. `QuickJS` queues them, and
/// nothing else runs the queue.
fn drain(runtime: &Runtime) {
   loop {
      match runtime.execute_pending_job() {
         Ok(true) => {},
         Ok(false) => break,
         Err(err) => log::error!("a promise job threw: {err}"),
      }
   }
}

/// `{status, ok, text(), json()}`, a subset of the web's `Response`.
#[cfg(target_os = "android")]
fn response_object<'js>(
   ctx: &Ctx<'js>,
   response: &fetch::Response,
) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("status", response.status)?;
   object.set("ok", (200..300).contains(&response.status))?;
   let text = String::from_utf8_lossy(&response.body).into_owned();
   let json_text = text.clone();
   object.set("text", Function::new(ctx.clone(), move || text.clone())?)?;
   object.set(
      "json",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         ctx.json_parse(json_text.clone())
      })?,
   )?;
   Ok(object)
}

/// Snapshot of one phase's callbacks, so a callback that registers a hook does
/// not run into the borrow held while iterating.
fn registered<'js>(
   ctx: &Ctx<'js>,
   event: EventId,
   phase: impl for<'hook> Fn(&'hook Hook<'js>) -> Option<&'hook Function<'js>>,
) -> Vec<(String, Function<'js>)> {
   let Some(registry) = ctx.userdata::<Registry<'js>>() else {
      return Vec::new();
   };
   let hooks = registry.hooks.borrow();
   hooks
      .get(&event)
      .into_iter()
      .flatten()
      .filter_map(|hook| Some((hook.plugin.clone(), phase(hook)?.clone())))
      .collect()
}

/// One frozen handle per live registry entry. Plugins compare nothing and parse
/// nothing, the runtime recognizes a handle by identity.
fn registry<'js>(ctx: &Ctx<'js>, events: &GatewayRegistry) -> rquickjs::Result<Registry<'js>> {
   let freeze = ctx
      .globals()
      .get::<_, Object>("Object")?
      .get::<_, Function>("freeze")?;
   let gateway = Object::new(ctx.clone())?;
   let mut handles = Vec::new();
   for (event, entry) in events.iter() {
      let key = camel_case(&entry.name);
      let handle = Object::new(ctx.clone())?;
      let label = format!("gateway.{key}");
      handle.set(
         "toString",
         Function::new(ctx.clone(), move || label.clone())?,
      )?;
      let handle = freeze.call::<_, Object<'js>>((handle,))?;
      gateway.set(key, handle.clone())?;
      handles.push((event, handle));
   }
   let gateway = freeze.call((gateway,))?;
   Ok(Registry {
      gateway,
      handles,
      hooks: RefCell::default(),
      defined: RefCell::default(),
      chosen: RefCell::default(),
      unloads: RefCell::default(),
      #[cfg(target_os = "android")]
      pending: RefCell::default(),
      #[cfg(target_os = "android")]
      ui: RefCell::default(),
      #[cfg(target_os = "android")]
      commands: RefCell::default(),
      #[cfg(target_os = "android")]
      interceptors: RefCell::default(),
      #[cfg(target_os = "android")]
      request_hooks: RefCell::default(),
      #[cfg(target_os = "android")]
      pickers: RefCell::default(),
      #[cfg(target_os = "android")]
      event_hooks: RefCell::default(),
      #[cfg(target_os = "android")]
      node_watchers: RefCell::default(),
      #[cfg(target_os = "android")]
      open_pages: RefCell::default(),
      #[cfg(target_os = "android")]
      banners: RefCell::default(),
   })
}

/// `MESSAGE_CREATE` to `messageCreate`, the key plugins and `phoenix.d.ts` use.
fn camel_case(name: &str) -> String {
   name
      .split('_')
      .enumerate()
      .map(|(index, word)| {
         let lower = word.to_ascii_lowercase();
         let mut chars = lower.chars();
         match chars.next() {
            Some(first) if index > 0 => first.to_ascii_uppercase().to_string() + chars.as_str(),
            _ => lower,
         }
      })
      .collect()
}

fn event_of<'js>(ctx: &Ctx<'js>, sig: &Value<'js>) -> rquickjs::Result<EventId> {
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .handles
      .iter()
      .find(|(_, handle)| handle.as_value() == sig)
      .map(|(event, _)| *event)
      .ok_or_else(|| Exception::throw_type(ctx, "expected a hook point from phoenix.gateway"))
}

fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   sig: &Value<'js>,
   before: Option<Function<'js>>,
   after: Option<Function<'js>>,
) -> rquickjs::Result<()> {
   let event = event_of(ctx, sig)?;
   gateway::install(event);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .hooks
      .borrow_mut()
      .entry(event)
      .or_default()
      .push(Hook {
         plugin: plugin.to_owned(),
         before,
         after,
      });
   Ok(())
}

/// The `phoenix` object one plugin receives.
fn api<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let api = Object::new(ctx.clone())?;
   let gateway = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   api.set("gateway", gateway.gateway.clone())?;
   drop(gateway);

   let name = plugin.to_owned();
   api.set(
      "on",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, listener: Function<'js>| {
            register(&ctx, &name, &sig, None, Some(listener))
         },
      )?,
   )?;
   let name = plugin.to_owned();
   api.set(
      "hook",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, hooks: Object<'js>| {
            register(&ctx, &name, &sig, hooks.get("before")?, hooks.get("after")?)
         },
      )?,
   )?;
   api.set(
      "dispatch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "dispatch needs a JSON payload"))?
               .to_string()?;
            gateway::dispatch(event, &json).ok_or_else(|| {
               Exception::throw_message(&ctx, "dispatch runs only inside a gateway hook")
            })
         },
      )?,
   )?;

   api.set(
      "enqueue",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "enqueue needs a JSON payload"))?
               .to_string()?;
            gateway::enqueue(event, json);
            Ok::<_, rquickjs::Error>(())
         },
      )?,
   )?;

   let messages = messages_object(ctx)?;
   #[cfg(target_os = "android")]
   patch_api(ctx, plugin, &api, &messages)?;
   api.set("messages", messages)?;
   api.set("log", log_object(ctx, plugin)?)?;
   api.set("storage", storage(ctx, plugin)?)?;
   let settings = Object::new(ctx.clone())?;
   let name = plugin.to_owned();
   settings.set(
      "define",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, schema: Object<'js>| {
         define(&ctx, &name, &schema)
      })?,
   )?;
   api.set("settings", settings)?;
   let name = plugin.to_owned();
   api.set(
      "onUnload",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, cleanup: Function<'js>| {
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .unloads
            .borrow_mut()
            .entry(name.clone())
            .or_default()
            .push(cleanup);
         Ok::<_, rquickjs::Error>(())
      })?,
   )?;
   #[cfg(target_os = "android")]
   api.set("stores", stores_object(ctx)?)?;
   #[cfg(target_os = "android")]
   api.set("discord", discord_object(ctx, plugin)?)?;
   #[cfg(target_os = "android")]
   api.set(
      "fetch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, url: String, init: Opt<Object<'js>>| start_fetch(&ctx, url, init.0),
      )?,
   )?;
   Ok(api)
}

/// `phoenix.messages`, local-only messages.
fn messages_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let messages = Object::new(ctx.clone())?;
   messages.set(
      "addLocal",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>,
          channel_id: String,
          content: String,
          options: rquickjs::function::Opt<Object<'js>>| {
            let (author, guild_id, embeds) = match options.0 {
               Some(options) => {
                  (
                     options.get::<_, Option<String>>("author")?,
                     options.get::<_, Option<String>>("guildId")?,
                     options.get::<_, Option<Value<'js>>>("embeds")?,
                  )
               },
               None => (None, None, None),
            };
            let embeds = match embeds {
               Some(embeds) => {
                  ctx.json_stringify(embeds)?
                     .map(|json| json.to_string())
                     .transpose()?
               },
               None => None,
            };
            let embeds = RawValue::from_string(embeds.unwrap_or_else(|| "[]".to_owned()))
               .map_err(|err| Exception::throw_type(&ctx, &format!("embeds: {err}")))?;
            let options = local::Options {
               author:   author.as_deref().unwrap_or("Phoenix"),
               guild_id: guild_id.as_deref(),
               embeds:   &embeds,
            };
            local::add(&channel_id, &content, &options)
               .ok_or_else(|| Exception::throw_message(&ctx, "this build has no MESSAGE_CREATE"))
         },
      )?,
   )?;
   messages.set(
      "updateLocal",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, message: Value<'js>| {
         let json = ctx
            .json_stringify(message)?
            .ok_or_else(|| Exception::throw_type(&ctx, "updateLocal needs a message object"))?
            .to_string()?;
         Ok::<_, rquickjs::Error>(local::update(json))
      })?,
   )?;
   messages.set(
      "deleteLocal",
      Function::new(ctx.clone(), |channel_id: String, id: String| {
         local::delete(&channel_id, &id)
      })?,
   )?;
   Ok(messages)
}

/// `phoenix.ui`, `phoenix.commands` and `phoenix.messages.onSend`.
#[cfg(target_os = "android")]
fn patch_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
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
         move |ctx: Ctx<'js>, function: Function<'js>| on_emoji_pick(&ctx, &name, function),
      )?,
   )?;
   api.set("ui", ui_object(ctx, plugin)?)?;
   api.set("commands", commands_object(ctx, plugin)?)
}

/// The `ctx` a plugin callback receives.
#[cfg(target_os = "android")]
fn context_object<'js>(ctx: &Ctx<'js>, context: &UiContext) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("channelId", context.channel_id.clone())?;
   object.set("currentUserId", context.current_user_id.clone())?;
   if let Some(id) = &context.message_id {
      object.set("messageId", id.as_str())?;
   }
   if let Some(id) = &context.user_id {
      object.set("userId", id.as_str())?;
   }
   if let Some(message) = &context.message {
      object.set("message", ctx.json_parse(message.as_str())?)?;
   }
   let channel = context.channel_id.clone();
   object.set(
      "reply",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String| {
         let channel = channel
            .as_deref()
            .ok_or_else(|| Exception::throw_message(&ctx, "no open channel to reply in"))?;
         let embeds = RawValue::from_string("[]".to_owned())
            .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))?;
         let options = local::Options {
            author:   "Phoenix",
            guild_id: None,
            embeds:   &embeds,
         };
         local::add(channel, &text, &options)
            .ok_or_else(|| Exception::throw_message(&ctx, "this build has no MESSAGE_CREATE"))
      })?,
   )?;
   Ok(object)
}

/// A `render` or `visible` result as the patches draw it.
#[cfg(target_os = "android")]
fn outcome_of(target: Target, value: Option<&Value<'_>>) -> Outcome {
   if target == Target::ProfileBadges {
      let badges = value
         .and_then(|value| value.as_array())
         .map(|array| {
            array
               .iter::<Value<'_>>()
               .filter_map(Result::ok)
               .filter_map(|item| badge_of(&item))
               .take(MAX_BADGES)
               .collect()
         })
         .unwrap_or_default();
      return Outcome::Badges(badges);
   }
   if target == Target::Action {
      return Outcome::Shown(value.is_some_and(|value| {
         value
            .as_bool()
            .unwrap_or_else(|| !value.is_null() && !value.is_undefined())
      }));
   }
   let rendered = value.and_then(|value| {
      if value.is_null() || value.is_undefined() || value.as_bool() == Some(false) {
         return None;
      }
      if let Some(text) = value.as_string() {
         return Some(Rendered {
            text:  text.to_string().ok()?,
            color: None,
            tint:  None,
         });
      }
      if let Some(object) = value.as_object() {
         let argb = |key: &str| {
            object.get::<_, Option<f64>>(key).ok().flatten().map(|color| {
               #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
               let color = color as i64;
               color
            })
         };
         return Some(Rendered {
            text:  object.get::<_, Option<String>>("text").ok().flatten().unwrap_or_default(),
            color: argb("color"),
            tint:  argb("tint"),
         });
      }
      Some(Rendered {
         text:  value.as_number()?.to_string(),
         color: None,
         tint:  None,
      })
   });
   Outcome::Text(rendered)
}

/// A profile tab's `render` result as the page it draws. Its callbacks stay
/// here, numbered in the order the blocks name them.
#[cfg(target_os = "android")]
fn page_of<'js>(
   ctx: &Ctx<'js>,
   id: ContributionId,
   place: &Place,
   value: Option<&Value<'js>>,
) -> Outcome {
   let mut handlers = Vec::new();
   let blocks = match value.and_then(Value::as_array) {
      Some(array) => {
         array
            .iter::<Value<'js>>()
            .filter_map(|block| block_of(block.ok()?.as_object()?, &mut handlers))
            .collect()
      },
      None => {
         vec![patches::Block::Text {
            title: None,
            text:  "This tab could not be drawn.".to_owned(),
            color: None,
         }]
      },
   };
   if let Some(registry) = ctx.userdata::<Registry<'js>>()
      && let Some(callbacks) = registry.ui.borrow_mut().get_mut(&id)
   {
      if callbacks.pages.len() >= PAGES_CAP {
         callbacks.pages.clear();
      }
      callbacks.pages.insert(place.clone(), handlers);
   }
   Outcome::Page(blocks)
}

/// The `{label, danger?, color?, onPress?}` objects in `object[key]`.
#[cfg(target_os = "android")]
fn actions_of<'js>(
   object: &Object<'js>,
   key: &str,
   handler: &mut impl FnMut(&Object<'js>, &str) -> Option<u32>,
) -> Vec<patches::BlockAction> {
   let Some(array) = object.get::<_, Option<rquickjs::Array<'js>>>(key).ok().flatten() else {
      return Vec::new();
   };
   array
      .iter::<Object<'js>>()
      .filter_map(|action| {
         let action = action.ok()?;
         Some(patches::BlockAction {
            label:   action.get::<_, Option<String>>("label").ok().flatten()?,
            danger:  action.get::<_, Option<bool>>("danger").ok().flatten().unwrap_or_default(),
            color:   action.get::<_, Option<f64>>("color").ok().flatten().map(|color| {
               #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
               let color = color as i64;
               color
            }),
            handler: handler(&action, "onPress"),
         })
      })
      .collect()
}

#[cfg(target_os = "android")]
fn block_of<'js>(object: &Object<'js>, handlers: &mut Vec<Function<'js>>) -> Option<patches::Block> {
   let text = |object: &Object<'js>, key: &str| object.get::<_, Option<String>>(key).ok().flatten();
   let flag = |object: &Object<'js>, key: &str| {
      object
         .get::<_, Option<bool>>(key)
         .ok()
         .flatten()
         .unwrap_or_default()
   };
   let mut handler = |object: &Object<'js>, key: &str| {
      let function = object.get::<_, Option<Function<'js>>>(key).ok().flatten()?;
      handlers.push(function);
      u32::try_from(handlers.len() - 1).ok()
   };
   let block = match text(object, "type")?.as_str() {
      "text" => {
         patches::Block::Text {
            title: text(object, "title"),
            text:  text(object, "text")?,
            color: object.get::<_, Option<f64>>("color").ok().flatten().map(|color| {
               #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
               let color = color as i64;
               color
            }),
         }
      },
      "entry" => {
         let actions = actions_of(object, "actions", &mut handler);
         let strings = |key: &str| {
            object
               .get::<_, Option<Vec<String>>>(key)
               .ok()
               .flatten()
               .unwrap_or_default()
         };
         patches::Block::Entry {
            avatar: text(object, "avatar"),
            title: text(object, "title")?,
            subtitle: text(object, "subtitle"),
            text: text(object, "text").unwrap_or_default(),
            badges: strings("badges"),
            images: strings("images"),
            actions,
            side: actions_of(object, "side", &mut handler),
            hold: handler(object, "onLongPress"),
         }
      },
      "buttons" => patches::Block::Buttons(actions_of(object, "buttons", &mut handler)),
      "input" => {
         let submit = handler(object, "onSubmit")?;
         patches::Block::Input {
            placeholder: text(object, "placeholder").unwrap_or_default(),
            handler: submit,
            send: flag(object, "sendButton"),
         }
      },
      "button" => {
         patches::Block::Button {
            label:   text(object, "label")?,
            danger:  flag(object, "danger"),
            handler: handler(object, "onPress")?,
         }
      },
      _ => return None,
   };
   Some(block)
}

/// At most this many badge images per profile, so a bad API cannot flood the row.
const MAX_BADGES: usize = 32;

/// One badge from a render result: `"uri"`, or
/// `{uri, label, description, source}`.
#[cfg(target_os = "android")]
fn badge_of(value: &Value<'_>) -> Option<Badge> {
   if let Some(uri) = value.as_string() {
      let uri = uri.to_string().ok()?;
      let uri = uri.trim();
      return (!uri.is_empty()).then(|| Badge {
         uri:         uri.to_owned(),
         label:       String::new(),
         description: String::new(),
         source:      String::new(),
      });
   }
   let object = value.as_object()?;
   let text = |key: &str| {
      object
         .get::<_, Option<String>>(key)
         .ok()
         .flatten()
         .unwrap_or_default()
   };
   let uri = object.get::<_, String>("uri").ok()?;
   let uri = uri.trim();
   (!uri.is_empty()).then(|| Badge {
      uri:         uri.to_owned(),
      label:       text("label"),
      description: text("description"),
      source:      text("source"),
   })
}

/// A function that undoes a registration, with `update` to redraw it. For a
/// message accessory or profile tab, `update(id)` redraws one message or
/// profile.
#[cfg(target_os = "android")]
fn remover<'js>(
   ctx: &Ctx<'js>,
   remove: impl Fn(&Ctx<'js>) + 'js,
   update: Option<(ContributionId, Target)>,
) -> rquickjs::Result<Function<'js>> {
   let function = Function::new(ctx.clone(), move |ctx: Ctx<'js>| remove(&ctx))?;
   if let Some((id, target)) = update {
      function.set(
         "update",
         Function::new(ctx.clone(), move |arg: rquickjs::function::Opt<Value<'_>>| {
            if matches!(target, Target::Header | Target::Composer)
               && let Some(object) = arg.0.as_ref().and_then(Value::as_object)
            {
               let icon = object.get::<_, Option<String>>("icon").ok().flatten();
               let label = object.get::<_, Option<String>>("label").ok().flatten();
               let tint = match object.get::<_, Value<'_>>("tint") {
                  Ok(value) if value.is_null() => Some(None),
                  Ok(value) if value.is_undefined() => None,
                  Ok(value) => value.as_number().map(|color| Some(color as i64)),
                  Err(_) => None,
               };
               patches::update_button(id, icon, label, tint);
               return;
            }
            let at = arg
               .0
               .and_then(|value| value.as_string().and_then(|text| text.to_string().ok()));
            let place = at.map(|at| {
               if target == Target::Tab {
                  Place::User(Some(at))
               } else {
                  Place::Message(at)
               }
            });
            patches::update(id, place);
         })?,
      )?;
   }
   Ok(function)
}

/// `phoenix.ui`, the UI patch targets.
#[cfg(target_os = "android")]
fn ui_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let ui = Object::new(ctx.clone())?;
   for (method, target) in [
      ("addHeaderButton", Target::Header),
      ("addComposerButton", Target::Composer),
      ("addMessageAccessory", Target::Content),
      ("addMessageAction", Target::Action),
      ("addProfileSection", Target::Profile),
      ("addProfileTab", Target::Tab),
      ("addProfileBadges", Target::ProfileBadges),
   ] {
      let name = plugin.to_owned();
      ui.set(
         method,
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
            add_contribution(&ctx, &name, target, &options)
         })?,
      )?;
   }
   ui.set(
      "choose",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options.get::<_, Option<String>>("title")?;
         let message = options.get::<_, Option<String>>("message")?;
         let items = parse_menu_items(&options)?;
         if message.is_none() && items.is_empty() {
            return Err(Exception::throw_type(&ctx, "choose needs options or a message"));
         }
         in_background(&ctx, move || {
            let labels = items.iter().map(|item| item.label.clone()).collect::<Vec<_>>();
            // A list goes in a sheet like Discord's own menus when one can be drawn.
            if message.is_none()
               && let Some(index) = patches::menu(title.clone(), items)
            {
               return Settlement::Int(Ok(index));
            }
            Settlement::Int(crate::picker::choose(title.as_deref(), message.as_deref(), &labels))
         })
      })?,
   )?;
   ui.set(
      "showSheet",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options.get::<_, Option<String>>("title")?;
         let text = options.get::<_, String>("text")?;
         let monospace = options
            .get::<_, Option<bool>>("monospace")?
            .unwrap_or_default();
         let highlight = options
            .get::<_, Option<bool>>("highlight")?
            .unwrap_or_default();
         let items = parse_menu_items(&options)?;
         in_background(&ctx, move || {
            let labels = items.iter().map(|item| item.label.clone()).collect::<Vec<_>>();
            let body = patches::SheetBody {
               text: text.clone(),
               monospace,
               highlight,
            };
            if let Some(index) = patches::sheet(title.clone(), body, items) {
               return Settlement::Int(Ok(index));
            }
            Settlement::Int(crate::picker::view(
               title.as_deref(),
               &text,
               monospace,
               &labels,
            ))
         })
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "openPage",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
         let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
         let title = options
            .get::<_, Option<String>>("title")?
            .unwrap_or_else(|| owner.clone());
         let raw_sections = options
            .get::<_, Option<Vec<Object<'js>>>>("sections")?
            .unwrap_or_default();
         let mut handlers = Vec::new();
         let mut sections = Vec::new();
         for sec in raw_sections {
            let kind = sec.get::<_, Option<String>>("type")?.unwrap_or_default();
            let heading = sec.get::<_, Option<String>>("title")?;
            match kind.as_str() {
               "code" => {
                  sections.push(patches::PageSection::Code {
                     title:     heading,
                     text:      sec.get::<_, Option<String>>("text")?.unwrap_or_default(),
                     monospace: sec.get::<_, Option<bool>>("monospace")?.unwrap_or(true),
                     highlight: sec.get::<_, Option<bool>>("highlight")?.unwrap_or(false),
                  });
               },
               "text" => {
                  sections.push(patches::PageSection::Text {
                     title: heading,
                     text:  sec.get::<_, Option<String>>("text")?.unwrap_or_default(),
                  });
               },
               "buttons" => {
                  let raw_buttons = sec
                     .get::<_, Option<Vec<Object<'js>>>>("buttons")?
                     .unwrap_or_default();
                  let mut buttons = Vec::new();
                  for btn in raw_buttons {
                     let Some(label) = btn.get::<_, Option<String>>("label")? else {
                        continue;
                     };
                     let Some(on_press) = btn.get::<_, Option<Function<'js>>>("onPress")? else {
                        continue;
                     };
                     #[expect(clippy::cast_possible_truncation, reason = "pages have few buttons")]
                     let index = handlers.len() as u32;
                     handlers.push(on_press);
                     buttons.push(patches::PageButton {
                        label,
                        icon: btn.get::<_, Option<String>>("icon")?,
                        danger: btn.get::<_, Option<bool>>("danger")?.unwrap_or(false),
                        index,
                     });
                  }
                  sections.push(patches::PageSection::Buttons {
                     title: heading,
                     buttons,
                  });
               },
               _ => {},
            }
         }
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .open_pages
            .borrow_mut()
            .insert(id, (owner.clone(), handlers));
         patches::open_page(id, title, sections);
         let handle = Object::new(ctx.clone())?;
         handle.set("close", Function::new(ctx, move || patches::close_page(id))?)?;
         Ok::<_, rquickjs::Error>(handle)
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "hide",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, aid: String| {
         let handle = patches::hide(owner.clone(), aid);
         remover(&ctx, move |_| patches::unhide(handle), None)
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "onEvent",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>,
               event: String,
               handler: Function<'js>,
               options: Opt<Object<'js>>| {
            on_view_event(&ctx, &owner, &event, handler, options.0)
         },
      )?,
   )?;
   ui.set(
      "dispatch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, node: i32, event: String, payload: Opt<Value<'js>>| {
            let prop = phoenix_ui::Prop::from_str(&event).map_err(|_| {
               Exception::throw_type(&ctx, &format!("unknown Phoenix event {event:?}"))
            })?;
            let wire = match payload.0 {
               Some(value) if !value.is_undefined() && !value.is_null() => {
                  js_to_wire(&ctx, value)?
               },
               _ => phoenix_wire::Value::Obj(Vec::new()),
            };
            Ok::<_, rquickjs::Error>(patches::dispatch_event(node, prop.key(), &wire))
         },
      )?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "onNode",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, matcher: Value<'js>, handler: Function<'js>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            on_node_created(&ctx, &owner, matcher, handler)
         },
      )?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "patchProps",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>,
               matcher: Value<'js>,
               patch: Object<'js>,
               options: Opt<Object<'js>>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            let add_on = patch
               .get::<_, Option<Vec<String>>>("addOn")?
               .unwrap_or_default();
            let props = parse_props(&ctx, &patch, Some("addOn"))?;
            let (target, once, timeout) = match options.0 {
               Some(opts) => {
                  let target = match opts.get::<_, Option<String>>("target")?.as_deref() {
                     Some("sheet") => PatchTarget::Sheet,
                     Some("list") => PatchTarget::List,
                     Some("node") | None => PatchTarget::Node,
                     Some(other) => {
                        return Err(Exception::throw_type(
                           &ctx,
                           &format!("unknown patchProps target {other:?}"),
                        ));
                     },
                  };
                  let once = opts.get::<_, Option<bool>>("once")?.unwrap_or(false);
                  let timeout = opts
                     .get::<_, Option<f64>>("timeoutMs")?
                     .filter(|ms| *ms > 0.0)
                     .map(|ms| std::time::Duration::from_millis(ms as u64));
                  (target, once, timeout)
               },
               None => (PatchTarget::Node, false, None),
            };
            let handle =
               patches::add_prop_patch(owner.clone(), matcher, target, props, add_on, once, timeout);
            remover(&ctx, move |_| patches::remove_prop_patch(handle), None)
         },
      )?,
   )?;
   ui.set(
      "update",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, node: i32, patch: Object<'js>| {
         let props = parse_props(&ctx, &patch, None)?;
         patches::update_node(node, props);
         Ok::<_, rquickjs::Error>(())
      })?,
   )?;
   ui.set(
      "find",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, matcher: Value<'js>| {
         let matcher = parse_node_match(&ctx, &matcher)?;
         Ok::<_, rquickjs::Error>(patches::find_nodes(&matcher))
      })?,
   )?;
   ui.set(
      "findDown",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, root: i32, matcher: Value<'js>, depth: Opt<usize>| {
            let matcher = parse_node_match(&ctx, &matcher)?;
            Ok::<_, rquickjs::Error>(patches::find_down(root, &matcher, depth.0.unwrap_or(8)))
         },
      )?,
   )?;
   ui.set(
      "node",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, id: i32| -> rquickjs::Result<Value<'js>> {
         match patches::node_info(id) {
            Some(info) => Ok(node_info_object(&ctx, &info)?.into_value()),
            None => Ok(Value::new_null(ctx)),
         }
      })?,
   )?;
   let owner = plugin.to_owned();
   ui.set(
      "notify",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options
            .get::<_, Option<String>>("title")?
            .filter(|text| !text.is_empty())
            .ok_or_else(|| Exception::throw_type(&ctx, "notify({ title }) requires a title"))?;
         let body = options.get::<_, Option<String>>("body")?;
         let icon = options.get::<_, Option<String>>("icon")?;
         let accent = options
            .get::<_, Option<f64>>("accent")?
            .map(|color| color as i64);
         let duration_ms = options
            .get::<_, Option<f64>>("durationMs")?
            .map_or(5000, |ms| if ms <= 0.0 { 0 } else { ms as u64 });
         let on_press = options.get::<_, Option<Function<'js>>>("onPress")?;
         let action = if on_press.is_some() {
            patches::BannerAction::JsCallback
         } else {
            patches::BannerAction::None
         };
         let id = patches::show_banner(title, body, icon, accent, duration_ms, action);
         if let Some(callback) = on_press {
            ctx.userdata::<Registry<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .banners
               .borrow_mut()
               .insert(id, (owner.clone(), callback));
         }
         remover(&ctx, move |_| patches::dismiss_banner(id), None)
      })?,
   )?;
   ui.set(
      "copy",
      Function::new(ctx.clone(), |text: String| {
         if let Err(err) = crate::picker::copy(&text) {
            log::warn!("copy: {err}");
         }
      })?,
   )?;
   ui.set(
      "toast",
      Function::new(ctx.clone(), |text: String| {
         if let Err(err) = crate::picker::toast(&text) {
            log::warn!("toast: {err}");
         }
      })?,
   )?;
   Ok(ui)
}

#[cfg(target_os = "android")]
fn parse_menu_items<'js>(options: &Object<'js>) -> rquickjs::Result<Vec<patches::MenuItem>> {
   Ok(options
      .get::<_, Option<Vec<Value<'js>>>>("options")?
      .unwrap_or_default()
      .into_iter()
      .filter_map(|option| {
         if let Some(label) = option.as_string() {
            return Some(patches::MenuItem {
               label:  label.to_string().ok()?,
               icon:   None,
               danger: false,
            });
         }
         let option = option.as_object()?;
         Some(patches::MenuItem {
            label:  option.get::<_, Option<String>>("label").ok().flatten()?,
            icon:   option.get::<_, Option<String>>("icon").ok().flatten(),
            danger: option.get::<_, Option<bool>>("danger").ok().flatten().unwrap_or_default(),
         })
      })
      .collect())
}

#[cfg(target_os = "android")]
fn add_contribution<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   target: Target,
   options: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   let placement = options.get::<_, Option<String>>("placement")?;
   let target = match (target, placement.as_deref()) {
      (Target::Content, Some("header")) => Target::Timestamp,
      _ => target,
   };
   let press = options.get::<_, Option<Function<'js>>>("onPress")?;
   let evaluate = if target == Target::Action {
      options.get::<_, Option<Function<'js>>>("visible")?
   } else {
      options.get::<_, Option<Function<'js>>>("render")?
   };
   let label = match target {
      Target::Profile | Target::Tab => options.get::<_, Option<String>>("title")?,
      _ => options.get::<_, Option<String>>("label")?,
   };
   let needs = match target {
      Target::Header | Target::Composer | Target::Action => (label.is_some(), "label and onPress"),
      Target::Content | Target::Timestamp => (evaluate.is_some(), "render"),
      Target::Profile | Target::Tab | Target::TabPage => {
         (label.is_some() && evaluate.is_some(), "title and render")
      },
      Target::ProfileBadges | Target::BadgeDirectory => (evaluate.is_some(), "render"),
   };
   let missing_press =
      matches!(target, Target::Header | Target::Composer | Target::Action) && press.is_none();
   if !needs.0 || missing_press {
      return Err(Exception::throw_type(
         ctx,
         &format!("expected an object with {}", needs.1),
      ));
   }

   let id = ContributionId::next();
   let conditional = target == Target::Action && evaluate.is_some();
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .ui
      .borrow_mut()
      .insert(id, Callbacks {
         plugin: plugin.to_owned(),
         target,
         press,
         evaluate,
         pages: HashMap::new(),
      });
   patches::add(Contribution {
      id,
      plugin: plugin.to_owned(),
      target,
      label: label.unwrap_or_default(),
      icon: options.get("icon")?,
      tint: options
         .get::<_, Option<f64>>("tint")?
         .map(|color| color as i64),
      danger: options
         .get::<_, Option<bool>>("danger")?
         .unwrap_or_default(),
      priority: options
         .get::<_, Option<i32>>("priority")?
         .unwrap_or_default(),
      conditional,
   });
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            registry.ui.borrow_mut().remove(&id);
         }
         patches::remove(id);
      },
      Some((id, target)),
   )
}

/// `phoenix.commands`, slash commands typed in the message box.
#[cfg(target_os = "android")]
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
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .commands
            .borrow_mut()
            .insert(name.clone(), (owner.clone(), execute.clone()));
         patches::add_command(Command {
            name:        name.clone(),
            description: options
               .get::<_, Option<String>>("description")?
               .unwrap_or_default(),
            icon:        options.get("icon")?,
            plugin:      owner.clone(),
         });
         remover(
            &ctx,
            move |ctx| {
               if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                  let mut commands = registry.commands.borrow_mut();
                  if commands
                     .get(&name)
                     .is_some_and(|(_, current)| *current == execute)
                  {
                     commands.remove(&name);
                     patches::remove_command(&name);
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
         for (index, command) in patches::commands().into_iter().enumerate() {
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

#[cfg(target_os = "android")]
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

#[cfg(target_os = "android")]
fn on_emoji_pick<'js>(
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
      .pickers
      .borrow_mut()
      .push((id, plugin.to_owned(), function));
   patches::set_emoji_pickers(registry.pickers.borrow().len());
   drop(registry);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            let mut pickers = registry.pickers.borrow_mut();
            pickers.retain(|(existing, ..)| *existing != id);
            patches::set_emoji_pickers(pickers.len());
         }
      },
      None,
   )
}

#[cfg(target_os = "android")]
fn on_view_event<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   event: &str,
   function: Function<'js>,
   options: Option<Object<'js>>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

   let prop = phoenix_ui::Prop::from_str(event)
      .map_err(|_| Exception::throw_type(ctx, &format!("unknown Phoenix event {event:?}")))?;
   let intercept = options
      .and_then(|opts| opts.get::<_, Option<bool>>("intercept").ok().flatten())
      .unwrap_or(false);
   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let key = prop.key();
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .event_hooks
      .borrow_mut()
      .push((id, plugin.to_owned(), key, intercept, function));
   drop(registry);
   patches::add_event_hook(id, plugin.to_owned(), key, intercept);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            registry
               .event_hooks
               .borrow_mut()
               .retain(|(existing, ..)| *existing != id);
         }
         patches::remove_event_hook(id);
      },
      None,
   )
}

#[cfg(target_os = "android")]
fn on_node_created<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   matcher: NodeMatch,
   function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .node_watchers
      .borrow_mut()
      .insert(id, (plugin.to_owned(), function));
   drop(registry);
   patches::add_node_watcher(id, plugin.to_owned(), matcher);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            registry.node_watchers.borrow_mut().remove(&id);
         }
         patches::remove_node_watcher(id);
      },
      None,
   )
}

#[cfg(target_os = "android")]
fn parse_node_match<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<NodeMatch> {
   if let Some(text) = value.as_string() {
      return Ok(NodeMatch::Exact(text.to_string()?));
   }
   if let Some(obj) = value.as_object() {
      if let Some(exact) = obj.get::<_, Option<String>>("exact")? {
         return Ok(NodeMatch::Exact(exact));
      }
      if let Some(prefix) = obj.get::<_, Option<String>>("prefix")? {
         return Ok(NodeMatch::Prefix(prefix));
      }
   }
   Err(Exception::throw_type(
      ctx,
      "expected accessibilityId string or { exact | prefix }",
   ))
}

#[cfg(target_os = "android")]
fn is_float_prop(name: &str) -> bool {
   matches!(
      name,
      "opacity"
         | "scrimOpacity"
         | "width"
         | "height"
         | "minWidth"
         | "maxWidth"
         | "minHeight"
         | "maxHeight"
         | "radius"
         | "radiusTopLeft"
         | "radiusTopRight"
         | "radiusBottomLeft"
         | "radiusBottomRight"
         | "fontSize"
         | "lineHeight"
         | "gap"
         | "gapX"
         | "gapY"
         | "flex"
         | "scale"
         | "scaleX"
         | "scaleY"
         | "translateX"
         | "translateY"
         | "rotation"
         | "elevation"
         | "borderWidth"
         | "hitSlop"
   )
}

#[cfg(target_os = "android")]
fn parse_props<'js>(
   ctx: &Ctx<'js>,
   object: &Object<'js>,
   skip: Option<&str>,
) -> rquickjs::Result<phoenix_ui::Props> {
   let mut out = Vec::new();
   for entry in object.props::<String, Value<'js>>() {
      let (name, value) = entry?;
      if skip == Some(name.as_str()) || value.is_undefined() {
         continue;
      }
      let prop = phoenix_ui::Prop::from_str(&name)
         .map_err(|_| Exception::throw_type(ctx, &format!("unknown Phoenix prop {name:?}")))?;
      let wire = if is_float_prop(&name)
         && let Some(num) = value.as_number()
      {
         phoenix_wire::Value::F64(num)
      } else {
         js_to_wire(ctx, value)?
      };
      out.push((prop, wire));
   }
   Ok(out)
}

#[cfg(target_os = "android")]
fn js_to_wire<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<phoenix_wire::Value> {
   if value.is_null() || value.is_undefined() {
      return Ok(phoenix_wire::Value::Null);
   }
   if let Some(flag) = value.as_bool() {
      return Ok(phoenix_wire::Value::Bool(flag));
   }
   if let Some(int) = value.as_int() {
      return Ok(phoenix_wire::Value::I32(int));
   }
   if let Some(num) = value.as_float() {
      if num.fract() == 0.0 && num > f64::from(i32::MAX) && num <= (1i64 << 53) as f64 {
         #[expect(clippy::cast_possible_truncation, reason = "checked finite integer range")]
         return Ok(phoenix_wire::Value::I64(num as i64));
      }
      return Ok(phoenix_wire::Value::F64(num));
   }
   if let Some(text) = value.as_string() {
      return Ok(phoenix_wire::Value::Str(text.to_string()?));
   }
   if let Some(array) = value.as_array() {
      let mut items = Vec::with_capacity(array.len());
      for item in array.iter::<Value<'js>>() {
         items.push(js_to_wire(ctx, item?)?);
      }
      return Ok(phoenix_wire::Value::Arr(items));
   }
   if let Some(object) = value.as_object() {
      let mut fields = Vec::new();
      for entry in object.props::<String, Value<'js>>() {
         let (name, field_value) = entry?;
         if field_value.is_undefined() {
            continue;
         }
         let prop = phoenix_ui::Prop::from_str(&name).map_err(|_| {
            Exception::throw_type(ctx, &format!("unknown Phoenix prop/field {name:?}"))
         })?;
         fields.push((prop.key(), js_to_wire(ctx, field_value)?));
      }
      return Ok(phoenix_wire::Value::Obj(fields));
   }
   Ok(phoenix_wire::Value::Null)
}

#[cfg(target_os = "android")]
fn wire_to_js<'js>(ctx: &Ctx<'js>, value: &phoenix_wire::Value) -> rquickjs::Result<Value<'js>> {
   match value {
      phoenix_wire::Value::Null => Ok(Value::new_null(ctx.clone())),
      phoenix_wire::Value::Bool(flag) => Ok(Value::new_bool(ctx.clone(), *flag)),
      phoenix_wire::Value::I32(int) => Ok(Value::new_int(ctx.clone(), *int)),
      phoenix_wire::Value::I64(int) => Ok(Value::new_float(ctx.clone(), *int as f64)),
      phoenix_wire::Value::F64(num) => Ok(Value::new_float(ctx.clone(), *num)),
      phoenix_wire::Value::Str(text) => text.as_str().into_js(ctx),
      phoenix_wire::Value::Arr(items) => {
         let arr = rquickjs::Array::new(ctx.clone())?;
         for (idx, item) in items.iter().enumerate() {
            arr.set(idx, wire_to_js(ctx, item)?)?;
         }
         Ok(arr.into_value())
      },
      phoenix_wire::Value::Obj(fields) => {
         let obj = Object::new(ctx.clone())?;
         for (key, val) in fields {
            let name = phoenix_ui::PROPS
               .get(usize::from(key.get()))
               .copied()
               .map(ToOwned::to_owned)
               .unwrap_or_else(|| key.get().to_string());
            obj.set(name, wire_to_js(ctx, val)?)?;
         }
         Ok(obj.into_value())
      },
      phoenix_wire::Value::Map(entries) => {
         let obj = Object::new(ctx.clone())?;
         for (key, val) in entries {
            obj.set(key.as_str(), wire_to_js(ctx, val)?)?;
         }
         Ok(obj.into_value())
      },
      phoenix_wire::Value::Rect(rect) => {
         let arr = rquickjs::Array::new(ctx.clone())?;
         for (idx, val) in rect.iter().enumerate() {
            arr.set(idx, f64::from(*val))?;
         }
         Ok(arr.into_value())
      },
      phoenix_wire::Value::Words(words) => words.clone().into_js(ctx),
      phoenix_wire::Value::WPatch { .. } => Ok(Value::new_null(ctx.clone())),
   }
}

#[cfg(target_os = "android")]
fn json_parser_fn<'js>(ctx: &Ctx<'js>, source: String) -> rquickjs::Result<Function<'js>> {
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      ctx.json_parse(source.as_str())
         .unwrap_or_else(|_| Value::new_null(ctx))
   })
}

#[cfg(target_os = "android")]
fn node_info_object<'js>(ctx: &Ctx<'js>, info: &NodeInfo) -> rquickjs::Result<Object<'js>> {
   let obj = Object::new(ctx.clone())?;
   obj.set("id", info.id)?;
   obj.set("accessibilityId", info.accessibility_id.clone())?;
   obj.set("parent", info.parent)?;
   obj.set("children", info.children.clone())?;
   obj.set("sheet", info.sheet)?;
   obj.set("list", info.list)?;
   obj.set("on", info.on.clone())?;
   Ok(obj)
}

/// `phoenix.log`, whose lines carry the plugin's name.
fn log_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let log = Object::new(ctx.clone())?;
   for (method, level) in [
      ("debug", Level::Debug),
      ("info", Level::Info),
      ("warn", Level::Warn),
      ("error", Level::Error),
   ] {
      let name = plugin.to_owned();
      log.set(
         method,
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
            let parts = args
               .0
               .into_iter()
               .map(|arg| {
                  if let Some(text) = arg.as_string() {
                     return text.to_string();
                  }
                  ctx.json_stringify(arg)?
                     .map_or_else(|| Ok("undefined".to_owned()), |json| json.to_string())
               })
               .collect::<rquickjs::Result<Vec<_>>>()?;
            log::log!(level, "[{name}] {}", parts.join(" "));
            Ok::<_, rquickjs::Error>(())
         })?,
      )?;
   }
   Ok(log)
}

/// A fetch-style `{method, headers, body}`. A body that is not a string is
/// sent as JSON.
#[cfg(target_os = "android")]
struct Init {
   method:  String,
   headers: Vec<(String, String)>,
   body:    Option<Vec<u8>>,
}

#[cfg(target_os = "android")]
impl Init {
   fn read<'js>(ctx: &Ctx<'js>, init: Option<Object<'js>>) -> rquickjs::Result<Self> {
      let mut out = Self {
         method:  "GET".to_owned(),
         headers: Vec::new(),
         body:    None,
      };
      let Some(init) = init else {
         return Ok(out);
      };
      if let Some(method) = init.get::<_, Option<String>>("method")? {
         out.method = method.to_ascii_uppercase();
      }
      if let Some(headers) = init.get::<_, Option<Object<'js>>>("headers")? {
         out.headers = headers
            .props::<String, String>()
            .collect::<rquickjs::Result<_>>()?;
      }
      out.set_body(ctx, init.get("body")?)?;
      Ok(out)
   }

   fn set_body<'js>(&mut self, ctx: &Ctx<'js>, body: Value<'js>) -> rquickjs::Result<()> {
      if body.is_undefined() || body.is_null() {
         return Ok(());
      }
      if let Some(text) = body.as_string() {
         self.body = Some(text.to_string()?.into_bytes());
         return Ok(());
      }
      if !self
         .headers
         .iter()
         .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
      {
         self
            .headers
            .push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      self.body = ctx
         .json_stringify(body)?
         .map(|json| json.to_string())
         .transpose()?
         .map(String::into_bytes);
      Ok(())
   }
}

/// Sends the request on its own thread, so neither the gateway nor the UI
/// waits on the network, and returns a promise that settles when it answers.
#[cfg(target_os = "android")]
fn start_fetch<'js>(
   ctx: &Ctx<'js>,
   url: String,
   init: Option<Object<'js>>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let Init {
      method,
      headers,
      body,
   } = Init::read(ctx, init)?;
   let request = fetch::Request {
      url,
      method,
      headers,
      body,
   };
   in_background(ctx, move || Settlement::Fetch(fetch::send(&request)))
}

/// `phoenix.discord`, Discord's API on the app's own client and session:
/// `request(path, init)`, `get` and `delete(path, init)`, `post`, `put` and
/// `patch(path, body, init)`, and `onRequest(filter, handler)`.
#[cfg(target_os = "android")]
fn discord_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   fn send<'js>(
      ctx: &Ctx<'js>,
      path: String,
      init: Init,
   ) -> rquickjs::Result<rquickjs::Promise<'js>> {
      in_background(ctx, move || {
         Settlement::Discord(discord::send(
            &init.method,
            &path,
            init.headers,
            init.body.unwrap_or_default(),
         ))
      })
   }

   let object = Object::new(ctx.clone())?;
   let owner = plugin.to_owned();
   object.set(
      "onRequest",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, filter: Object<'js>, handler: Function<'js>| {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let parsed = discord::RequestFilter {
               method:        filter.get::<_, Option<String>>("method")?,
               path_prefix:   filter.get::<_, Option<String>>("pathPrefix")?,
               path_suffix:   filter.get::<_, Option<String>>("pathSuffix")?,
               path_includes: filter.get::<_, Option<String>>("pathIncludes")?,
            };
            ctx.userdata::<Registry<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
               .request_hooks
               .borrow_mut()
               .insert(id, (owner.clone(), handler));
            discord::add_request_hook(id, owner.clone(), parsed);
            remover(
               &ctx,
               move |ctx| {
                  if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                     registry.request_hooks.borrow_mut().remove(&id);
                  }
                  discord::remove_request_hook(id);
               },
               None,
            )
         },
      )?,
   )?;
   object.set(
      "authorize",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let client_id = options.get::<_, Option<String>>("clientId")?;
         let redirect_uri = options.get::<_, Option<String>>("redirectUri")?;
         let (Some(client_id), Some(redirect_uri)) = (client_id, redirect_uri) else {
            return Err(Exception::throw_type(&ctx, "authorize needs clientId and redirectUri"));
         };
         let request = discord::Authorization {
            client_id,
            redirect_uri,
            scopes: options
               .get::<_, Option<Vec<String>>>("scopes")?
               .unwrap_or_else(|| vec!["identify".to_owned()]),
            response_type: options
               .get::<_, Option<String>>("responseType")?
               .unwrap_or_else(|| "code".to_owned()),
         };
         in_background(&ctx, move || Settlement::Text(discord::authorize(&request)))
      })?,
   )?;
   object.set(
      "request",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
            send(&ctx, path, Init::read(&ctx, init.0)?)
         },
      )?,
   )?;
   for verb in ["GET", "DELETE"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   for verb in ["POST", "PUT", "PATCH"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, body: Opt<Value<'js>>, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               if let Some(body) = body.0 {
                  init.set_body(&ctx, body)?;
               }
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   Ok(object)
}

/// What a worker thread hands back to settle a promise with.
#[cfg(target_os = "android")]
pub enum Settlement {
   Fetch(Result<fetch::Response, fetch::FetchError>),
   Discord(Result<fetch::Response, discord::DiscordError>),
   Text(Result<String, discord::DiscordError>),
   Int(Result<i32, String>),
   Rows(Result<Vec<serde_json::Map<String, serde_json::Value>>, store::StoreError>),
}

/// Runs `work` on its own thread and returns a promise it settles. The
/// resolvers stay in the registry, since JS values cannot leave the thread
/// that holds the engine.
#[cfg(target_os = "android")]
fn in_background<'js>(
   ctx: &Ctx<'js>,
   work: impl FnOnce() -> Settlement + Send + 'static,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let (promise, resolve, reject) = ctx.promise()?;
   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .pending
      .borrow_mut()
      .insert(id, (resolve, reject));
   std::thread::spawn(move || {
      let settlement = work();
      if let Some(engine) = crate::engine() {
         engine.settle(id, settlement);
      }
   });
   Ok(promise)
}

/// `phoenix.stores`, read-only SQL over Phoenix's cache.
#[cfg(target_os = "android")]
fn stores_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let stores = Object::new(ctx.clone())?;
   stores.set(
      "sql",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sql: String, args: Rest<Value<'js>>| {
            let args = args
               .0
               .into_iter()
               .map(|arg| {
                  if let Some(text) = arg.as_string() {
                     return text.to_string();
                  }
                  ctx.json_stringify(arg)?
                     .map_or_else(|| Ok("null".to_owned()), |json| json.to_string())
               })
               .collect::<rquickjs::Result<Vec<String>>>()?;
            let files = state().files_dir();
            in_background(&ctx, move || {
               Settlement::Rows(
                  store::database(&files).and_then(|db| store::query(&db, &sql, &args)),
               )
            })
         },
      )?,
   )?;
   Ok(stores)
}

/// Hands every secret setting's current value to the logger to scrub.
fn redact_secrets(registry: &Registry<'_>) {
   let chosen = registry.chosen.borrow();
   let secrets = registry
      .defined
      .borrow()
      .iter()
      .flat_map(|(plugin, definitions)| {
         let picked = chosen.get(plugin);
         definitions.iter().filter_map(move |definition| {
            match definition
               .kind
               .current(picked.and_then(|values| values.get(&definition.key)))
            {
               Control::Secret(value) => Some(value),
               _ => None,
            }
         })
      })
      .collect::<Vec<String>>();
   crate::console::redact(secrets);
}

/// The kind of setting `key` that `spec` describes.
fn setting_kind<'js>(ctx: &Ctx<'js>, key: &str, spec: &Object<'js>) -> rquickjs::Result<Kind<'js>> {
   Ok(match spec.get::<_, String>("type")?.as_str() {
      "switch" => Kind::Switch(spec.get::<_, Option<bool>>("default")?.unwrap_or_default()),
      "text" => {
         Kind::Text(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "secret" => {
         Kind::Secret(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "select" => {
         let options = spec
            .get::<_, Vec<Object<'js>>>("options")?
            .into_iter()
            .map(|option| {
               let value: String = option.get("value")?;
               let label = option.get::<_, Option<String>>("label")?;
               Ok(Choice {
                  label: label.unwrap_or_else(|| value.clone()),
                  value,
               })
            })
            .collect::<rquickjs::Result<Vec<Choice>>>()?;
         let Some(first) = options.first() else {
            return Err(Exception::throw_type(
               ctx,
               &format!("setting {key}: a select needs at least one option"),
            ));
         };
         let default = spec
            .get::<_, Option<String>>("default")?
            .unwrap_or_else(|| first.value.clone());
         Kind::Select { options, default }
      },
      "number" => Kind::Number(spec.get::<_, Option<f64>>("default")?.unwrap_or_default()),
      "button" => Kind::Button(spec.get("onPress")?),
      other => {
         return Err(Exception::throw_type(
            ctx,
            &format!(
               "setting {key}: unknown type {other:?}, expected switch, text, secret, select, \
                number or button"
            ),
         ));
      },
   })
}

/// Records `schema` as the plugin's settings and returns an object with one
/// getter per non-button key that reads the user's current choice.
fn define<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   schema: &Object<'js>,
) -> rquickjs::Result<Object<'js>> {
   let mut definitions = Vec::new();
   for entry in schema.props::<String, Object<'js>>() {
      let (key, spec) = entry?;
      let kind = setting_kind(ctx, &key, &spec)?;
      definitions.push(Definition {
         title: spec.get("title")?,
         description: spec.get("description")?,
         on_change: spec.get("onChange")?,
         kind,
         key,
      });
   }

   let values = Object::new(ctx.clone())?;
   for definition in &definitions {
      if matches!(definition.kind, Kind::Button(_)) {
         continue;
      }
      let (plugin, key) = (plugin.to_owned(), definition.key.clone());
      let read = move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
         let registry = ctx
            .userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?;
         let defined = registry.defined.borrow();
         let chosen = registry.chosen.borrow();
         let Some(definition) = defined
            .get(&plugin)
            .and_then(|list| list.iter().find(|definition| definition.key == key))
         else {
            return Ok(Value::new_undefined(ctx.clone()));
         };
         let picked = chosen.get(&plugin).and_then(|values| values.get(&key));
         match definition.kind.current(picked) {
            Control::Switch(on) => on.into_js(&ctx),
            Control::Text(text) | Control::Secret(text) => text.into_js(&ctx),
            Control::Select { chosen, .. } => chosen.into_js(&ctx),
            Control::Number(number) => number.into_js(&ctx),
            Control::Button => Ok(Value::new_undefined(ctx.clone())),
         }
      };
      values.prop(
         definition.key.as_str(),
         Accessor::new_get(read).enumerable(),
      )?;
   }
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .defined
      .borrow_mut()
      .insert(plugin.to_owned(), definitions);
   if let Some(registry) = ctx.userdata::<Registry<'js>>() {
      redact_secrets(&registry);
   }
   Ok(values)
}

/// A JSON object per plugin, loaded once and rewritten on every change.
fn storage<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let path = state().storage_path(plugin);
   let text = match fs::read_to_string(&path) {
      Ok(text) => text,
      Err(err) if err.kind() == ErrorKind::NotFound => "{}".to_owned(),
      Err(err) => return Err(throw(ctx, &io_error(&path)(err))),
   };
   let store = ctx.json_parse(text)?.into_object().ok_or_else(|| {
      Exception::throw_type(ctx, &format!("{} is not a JSON object", path.display()))
   })?;

   let persist = move |ctx: &Ctx<'js>, store: &Object<'js>, path: &PathBuf| {
      let json = ctx
         .json_stringify(store.clone())?
         .map(|json| json.to_string())
         .transpose()?;
      let write = || {
         fs::create_dir_all(path.parent().expect("storage paths have a parent"))?;
         fs::write(path, json.unwrap_or_default())
      };
      write().map_err(|err| throw(ctx, &io_error(path)(err)))
   };

   let storage = Object::new(ctx.clone())?;
   let read = store.clone();
   storage.set(
      "get",
      Function::new(ctx.clone(), move |key: String| {
         read.get::<_, Value<'js>>(key)
      })?,
   )?;
   let (write, file) = (store.clone(), path.clone());
   storage.set(
      "set",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, key: String, value: Value<'js>| {
            write.set(key, value)?;
            persist(&ctx, &write, &file)
         },
      )?,
   )?;
   let (write, file) = (store.clone(), path);
   storage.set(
      "delete",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String| {
         write.remove(key)?;
         persist(&ctx, &write, &file)
      })?,
   )?;
   storage.set(
      "keys",
      Function::new(ctx.clone(), move || {
         store.keys::<String>().collect::<rquickjs::Result<Vec<_>>>()
      })?,
   )?;
   Ok(storage)
}

/// Evaluates `bundled` plus every `*.js` file in the plugins directory, in name
/// order, skipping the ones the user turned off.
#[expect(
   clippy::significant_drop_tightening,
   reason = "plugins load under one engine lock so no gateway event sees a half-loaded set"
)]
pub fn load_plugins(state: &State, bundled: Vec<Plugin>) -> Result<Vec<String>, Error> {
   let choices_path = state.choices_path();
   let choices = match fs::read(&choices_path) {
      Ok(bytes) => {
         serde_json::from_slice::<Choices>(&bytes).unwrap_or_else(|err| {
            log::error!("{}: {err}", choices_path.display());
            Choices::default()
         })
      },
      Err(err) if err.kind() == ErrorKind::NotFound => Choices::default(),
      Err(err) => return Err(io_error(&choices_path)(err)),
   };
   gateway::set_staff_enabled(choices.staff);
   #[cfg(target_os = "android")]
   crate::set_remote_shell_flag(choices.remote_shell);

   let dir = state.plugins_dir();
   let mut sources = bundled
      .into_iter()
      .map(|plugin| (plugin.name, (plugin.source, false)))
      .collect::<BTreeMap<String, (String, bool)>>();
   let paths = match fs::read_dir(&dir) {
      Ok(entries) => {
         entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error(&dir))?
      },
      Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
      Err(err) => return Err(io_error(&dir)(err)),
   };
   for path in paths.iter().map(fs::DirEntry::path) {
      let Some(name) = path
         .file_stem()
         .and_then(|stem| stem.to_str())
         .filter(|_| path.extension().is_some_and(|ext| ext == "js"))
      else {
         continue;
      };
      let source = fs::read_to_string(&path).map_err(io_error(&path))?;
      sources.insert(name.to_owned(), (source, true));
   }

   let mut engine = state
      .js
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   engine.context.with(|ctx| {
      if let Some(registry) = ctx.userdata::<Registry<'_>>() {
         *registry.chosen.borrow_mut() = choices.settings.into_owned();
      }
   });
   engine.plugins = sources
      .into_iter()
      .map(|(name, (source, installed))| {
         let enabled = !choices.disabled.contains(&name);
         (name, Loaded {
            source,
            enabled,
            error: None,
            installed,
         })
      })
      .collect();
   let enabled = engine
      .plugins
      .iter()
      .filter(|(_, loaded)| loaded.enabled)
      .map(|(name, _)| name.clone())
      .collect::<Vec<String>>();
   for name in &enabled {
      engine.eval(name);
   }
   Ok(enabled
      .into_iter()
      .filter(|name| {
         engine
            .plugins
            .get(name)
            .is_some_and(|loaded| loaded.error.is_none())
      })
      .collect())
}
