#[cfg(target_os = "android")]
mod background;
#[cfg(target_os = "android")]
mod bridge;
mod hooks;
#[cfg(target_os = "android")]
mod http;
#[cfg(target_os = "android")]
mod intercept;
mod messages;
#[cfg(target_os = "android")]
mod mount;
#[cfg(target_os = "android")]
mod page;
mod settings;
mod storage;
mod themes;
#[cfg(target_os = "android")]
mod ui;
#[cfg(target_os = "android")]
mod view;
#[cfg(target_os = "android")]
mod wire;

use std::{
   borrow::Cow,
   cell::RefCell,
   collections::{BTreeMap, BTreeSet, HashMap},
   fs,
   io::ErrorKind,
};

use log::Level;
use phoenix_sigs::registry::{EventId, GatewayRegistry};
#[cfg(target_os = "android")]
use rquickjs::function::Opt;
use rquickjs::{
   CatchResultExt, CaughtError, Context, Ctx, Exception, Function, JsLifetime, Object, Runtime,
   Value, function::Rest,
};

use crate::{
   Chosen, Error, Header, Plugin, PluginInfo, STATE, Setting, State, gateway, io_error,
   js::{
      hooks::{Hook, add_gateway_api},
      messages::messages_object,
      settings::{Definition, settings_object},
      storage::storage,
      themes::themes_object,
   },
};
#[cfg(target_os = "android")]
use crate::{
   discord,
   js::{
      bridge::bridge_object,
      http::{discord_object, start_fetch},
      messages::patch_api,
      storage::stores_object,
      ui::Callbacks,
      view::EventHook,
   },
   patches::{self, ContributionId},
};

pub struct Engine {
   runtime: Runtime,
   context: Context,
   plugins: BTreeMap<String, Loaded>,
}

struct Loaded {
   source: String,
   enabled: bool,
   error: Option<String>,
   /// Read from `home/plugins`, so the user can delete it.
   installed: bool,
}

type ChosenMap = BTreeMap<String, BTreeMap<String, Chosen>>;

const fn default_true() -> bool {
   true
}

/// Which plugins the user turned off or on against their default and how
/// they set each plugin's settings, kept in `home/plugins.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct Choices<'a> {
   #[serde(default)]
   disabled: BTreeSet<String>,
   #[serde(default)]
   enabled: BTreeSet<String>,
   #[serde(default)]
   settings: Cow<'a, ChosenMap>,
   #[serde(default = "default_true")]
   staff: bool,
   #[serde(default)]
   remote_shell: bool,
}

impl Default for Choices<'_> {
   fn default() -> Self {
      Self {
         disabled: BTreeSet::new(),
         enabled: BTreeSet::new(),
         settings: Cow::Owned(BTreeMap::new()),
         staff: true,
         remote_shell: false,
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
   gateway: Object<'js>,
   handles: Vec<(EventId, Object<'js>)>,
   hooks: RefCell<HashMap<EventId, Vec<Hook<'js>>>>,
   defined: RefCell<BTreeMap<String, Vec<Definition<'js>>>>,
   /// Kept across unloads, so a reloaded plugin sees what the user chose.
   chosen: RefCell<ChosenMap>,
   /// `phoenix.onUnload` callbacks, run when the plugin is turned off,
   /// reloaded or removed.
   unloads: RefCell<HashMap<String, Vec<Function<'js>>>>,
   /// The resolve and reject of each promise a worker thread will settle, by
   /// id.
   #[cfg(target_os = "android")]
   pending: RefCell<HashMap<u64, (Function<'js>, Function<'js>)>>,
   #[cfg(target_os = "android")]
   ui: RefCell<HashMap<ContributionId, Callbacks<'js>>>,
   /// Slash commands by lowercase name, with the plugin that registered each.
   #[cfg(target_os = "android")]
   commands: RefCell<HashMap<String, (String, Function<'js>)>>,
   /// `phoenix.messages.onSend` interceptors in registration order, by id.
   #[cfg(target_os = "android")]
   interceptors: RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.discord.onRequest` interceptors, by id.
   #[cfg(target_os = "android")]
   request_hooks: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// `phoenix.discord.onResponse` interceptors, by id.
   #[cfg(target_os = "android")]
   response_hooks: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// `phoenix.messages.onEmojiPick` handlers in registration order, by id.
   #[cfg(target_os = "android")]
   pickers: RefCell<Vec<(u64, String, Function<'js>)>>,
   /// `phoenix.ui.onEvent` handlers, by id.
   #[cfg(target_os = "android")]
   event_hooks: RefCell<Vec<EventHook<'js>>>,
   /// `phoenix.ui.onNode` watchers, by id.
   #[cfg(target_os = "android")]
   node_watchers: RefCell<HashMap<u64, (String, Function<'js>)>>,
   /// Open full-screen plugin pages (`phoenix.ui.openPage`) and their button
   /// callbacks, by page id.
   #[cfg(target_os = "android")]
   open_pages: RefCell<HashMap<u32, (String, Vec<Function<'js>>)>>,
   /// Active in-app notification banner tap callbacks (`phoenix.ui.notify`), by
   /// banner id.
   #[cfg(target_os = "android")]
   banners: RefCell<HashMap<u32, (String, Function<'js>)>>,
   /// Active raw JS-mounted subtrees (`phoenix.ui.mount`) and their callbacks,
   /// by mount id.
   #[cfg(target_os = "android")]
   mounts: RefCell<HashMap<u32, (String, Vec<Function<'js>>)>>,
}

// SAFETY: every field is bound to 'js, and Changed swaps exactly that lifetime.
unsafe impl<'js> JsLifetime<'js> for Registry<'js> {
   type Changed<'to> = Registry<'to>;
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
                        key: definition.key.clone(),
                        title: definition.title.clone(),
                        description: definition.description.clone(),
                        control: definition.kind.current(picked),
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
            registry
               .response_hooks
               .borrow_mut()
               .retain(|_, (plugin, _)| plugin != name);
            discord::forget_response_hooks(name);
            let mut pickers = registry.pickers.borrow_mut();
            pickers.retain(|(_, plugin, _)| plugin != name);
            patches::set_emoji_pickers(pickers.len());
            drop(pickers);
            registry
               .event_hooks
               .borrow_mut()
               .retain(|hook| hook.plugin != name);
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
               patches::pages::close_page(id);
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
               patches::banners::dismiss_banner(id);
            }
            drop(banners);
            let mut mounts = registry.mounts.borrow_mut();
            let unmounted = mounts
               .iter()
               .filter(|(_, (plugin, _))| plugin == name)
               .map(|(id, _)| *id)
               .collect::<Vec<_>>();
            for id in unmounted {
               mounts.remove(&id);
               patches::mount::unmount(id);
            }
            drop(mounts);
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
         self.plugins.insert(
            name.to_owned(),
            Loaded {
               source,
               enabled: true,
               error: None,
               installed: true,
            },
         );
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
      let (mut disabled, mut enabled) = (BTreeSet::new(), BTreeSet::new());
      for (name, loaded) in &self.plugins {
         if Header::from(loaded.source.as_str()).default_off {
            if loaded.enabled {
               enabled.insert(name.clone());
            }
         } else if !loaded.enabled {
            disabled.insert(name.clone());
         }
      }
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
            enabled,
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
               None => ctx
                  .globals()
                  .get::<_, Function<'_>>("String")?
                  .call((result,)),
            }
         };
         run().catch(&ctx).map_err(js_error)
      });
      drain(&self.runtime);
      result
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
      response_hooks: RefCell::default(),
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
      #[cfg(target_os = "android")]
      mounts: RefCell::default(),
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

/// The `phoenix` object one plugin receives.
fn api<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let api = Object::new(ctx.clone())?;
   let gateway = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   api.set("gateway", gateway.gateway.clone())?;
   drop(gateway);

   add_gateway_api(ctx, plugin, &api)?;

   let messages = messages_object(ctx)?;
   #[cfg(target_os = "android")]
   patch_api(ctx, plugin, &api, &messages)?;
   api.set("messages", messages)?;
   api.set("log", log_object(ctx, plugin)?)?;
   api.set("storage", storage(ctx, plugin)?)?;
   api.set("settings", settings_object(ctx, plugin)?)?;
   api.set("themes", themes_object(ctx)?)?;
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
   api.set("bridge", bridge_object(ctx)?)?;
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

/// Evaluates `bundled` plus every `*.js` file in the plugins directory, in name
/// order, skipping the ones the user turned off.
#[expect(
   clippy::significant_drop_tightening,
   reason = "plugins load under one engine lock so no gateway event sees a half-loaded set"
)]
pub fn load_plugins(state: &'static State, bundled: Vec<Plugin>) -> Result<Vec<String>, Error> {
   let choices_path = state.choices_path();
   let choices = match fs::read(&choices_path) {
      Ok(bytes) => serde_json::from_slice::<Choices>(&bytes).unwrap_or_else(|err| {
         log::error!("{}: {err}", choices_path.display());
         Choices::default()
      }),
      Err(err) if err.kind() == ErrorKind::NotFound => Choices::default(),
      Err(err) => return Err(io_error(&choices_path)(err)),
   };
   crate::staff::start(choices.staff, &state.image);
   #[cfg(target_os = "android")]
   crate::set_remote_shell_flag(choices.remote_shell);

   let dir = state.plugins_dir();
   let mut sources = bundled
      .into_iter()
      .map(|plugin| (plugin.name, (plugin.source, false)))
      .collect::<BTreeMap<String, (String, bool)>>();
   let paths = match fs::read_dir(&dir) {
      Ok(entries) => entries
         .collect::<Result<Vec<_>, _>>()
         .map_err(io_error(&dir))?,
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
         let enabled = if Header::from(source.as_str()).default_off {
            choices.enabled.contains(&name)
         } else {
            !choices.disabled.contains(&name)
         };
         (
            name,
            Loaded {
               source,
               enabled,
               error: None,
               installed,
            },
         )
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
