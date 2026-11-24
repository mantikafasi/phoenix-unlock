//! The plugin runtime the loader starts.
//!
//! [`start`] picks the target library among the loaded images, boots `QuickJS`
//! with the `phoenix` API, and evaluates every bundle in `<home>/plugins`.
//! Every gateway registry entry points at phoenix's detour from the start, and
//! a plugin's hook turns its event on.

use std::{
   path::{Path, PathBuf},
   sync::{Mutex, OnceLock},
};

use phoenix_hook::image::{BuildId, Image};
use phoenix_sigs::{
   LIBRARIES,
   registry::{self, GatewayRegistry},
};

#[cfg(target_os = "android")]
mod bridge;
pub mod console;
#[cfg(target_os = "android")]
mod discord;
#[cfg(target_os = "android")]
mod fetch;
mod gateway;
#[cfg(target_os = "android")]
mod highlight;
mod js;
#[cfg(target_os = "android")]
mod jvm;
mod local;
#[cfg(target_os = "android")]
mod patches;
#[cfg(target_os = "android")]
mod picker;
#[cfg(target_os = "android")]
mod remote;
#[cfg(target_os = "android")]
mod settings;
#[cfg(target_os = "android")]
mod socket;
mod staff;
#[cfg(target_os = "android")]
mod store;
pub mod themes;
#[cfg(target_os = "android")]
mod update;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("no loaded library is one of {LIBRARIES:?}")]
   NoTarget,
   #[error("the runtime is already running")]
   AlreadyStarted,
   #[error("the runtime has not started")]
   NotStarted,
   #[error("{path}: {source}")]
   Io {
      path: PathBuf,
      #[source]
      source: std::io::Error,
   },
   #[error("javascript: {0}")]
   Js(String),
   #[error(transparent)]
   Registry(#[from] registry::Error),
   #[error("{event}: the registry slot held {found:#x}, the walk read {expected:#x}")]
   SwapConflict {
      event: String,
      expected: usize,
      found: usize,
   },
   #[error(transparent)]
   Hook(#[from] phoenix_hook::Error),
   #[error("{url}: plugins install from https:// URLs ending in a name like plugin.js")]
   PluginUrl { url: String },
   #[error("{name}: plugin files are .js files named with letters, digits, - or _")]
   PluginFile { name: String },
   #[error("{url}: the download is not UTF-8 text")]
   NotUtf8 { url: String },
   #[cfg(target_os = "android")]
   #[error(transparent)]
   Fetch(#[from] fetch::FetchError),
   #[error("update Phoenix Unlock to {version} first, the site's plugins need it")]
   LoaderOutdated { version: String },
   #[error("{message}")]
   Theme { message: String },
}

struct State {
   image: Image<'static>,
   home: PathBuf,
   js: Mutex<js::Engine>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// A plugin bundle, an esbuild IIFE that receives the `phoenix` API.
pub struct Plugin {
   pub name: String,
   pub source: String,
}

/// A plugin as the settings page shows it.
#[derive(Clone, Debug)]
pub struct PluginInfo {
   pub name: String,
   pub enabled: bool,
   pub error: Option<String>,
   pub installed: bool,
   pub header: Header,
   pub settings: Vec<Setting>,
}

/// The `// @key value` lines a bundle starts with.
#[derive(Clone, Debug, Default)]
pub struct Header {
   pub description: Option<String>,
   pub version: Option<String>,
   pub author: Option<String>,
   /// `// @enabled false`, for plugins that stay off until the user turns
   /// them on.
   pub default_off: bool,
}

impl From<&str> for Header {
   fn from(source: &str) -> Self {
      let mut header = Self::default();
      for line in source
         .lines()
         .map(str::trim)
         .filter(|line| !line.is_empty())
      {
         let Some(comment) = line.strip_prefix("//") else {
            break;
         };
         let Some((key, value)) = comment
            .trim()
            .strip_prefix('@')
            .and_then(|tag| tag.split_once(' '))
         else {
            continue;
         };
         let value = value.trim();
         match key {
            "description" => header.description = Some(value.to_owned()),
            "version" => header.version = Some(value.to_owned()),
            "author" => header.author = Some(value.to_owned()),
            "enabled" => header.default_off = value == "false",
            _ => {},
         }
      }
      header
   }
}

/// A plugin setting as the user currently has it.
#[derive(Clone, Debug)]
pub struct Setting {
   pub key: String,
   pub title: String,
   pub description: Option<String>,
   pub control: Control,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
   Switch(bool),
   Text(String),
   /// Text shown masked, and redacted from the plugin's log lines.
   Secret(String),
   Select {
      options: Vec<Choice>,
      chosen: String,
   },
   Number(f64),
   Button,
}

/// One option of a select setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
   pub value: String,
   pub label: String,
}

/// A value the user picked for a setting, as `plugins.json` stores it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Chosen {
   Switch(bool),
   Number(f64),
   Text(String),
}

fn engine() -> Option<std::sync::MutexGuard<'static, js::Engine>> {
   Some(
      STATE
         .get()?
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner),
   )
}

/// Runs matching `phoenix.discord.onRequest` hooks on an outgoing Discord API
/// request.
#[cfg(target_os = "android")]
pub(crate) fn intercept_request(
   matched: &[u64],
   method: &str,
   path: &str,
   body: &[u8],
) -> discord::RequestAction {
   engine().map_or(discord::RequestAction::Pass, |engine| {
      engine.intercept_request(matched, method, path, body)
   })
}

/// Runs matching `phoenix.discord.onResponse` hooks (sync or async) on an
/// incoming Discord API reply and delivers the final body through `done`.
#[cfg(target_os = "android")]
pub(crate) fn intercept_response(
   matched: Vec<u64>,
   method: String,
   path: String,
   status: i32,
   body: Vec<u8>,
   done: std::sync::mpsc::Sender<Vec<u8>>,
) {
   if let Some(engine) = engine() {
      engine.intercept_response(matched, method, path, status, body, done);
   } else {
      let _ = done.send(body);
   }
}

/// Every plugin found at start, enabled or not.
#[must_use]
pub fn plugins() -> Vec<PluginInfo> {
   engine().map_or_default(|engine| engine.plugins())
}

/// Whether the signed-in user has the staff flag.
#[must_use]
pub fn staff_enabled() -> bool {
   staff::enabled()
}

/// Turns staff unlock on or off and remembers the choice across restarts.
pub fn set_staff_enabled(enabled: bool) {
   staff::set_enabled(enabled);
   if let Some(engine) = engine() {
      engine.save_choices();
   }
}

/// Whether the localhost debug shell (`127.0.0.1:4242`) is enabled.
#[cfg(target_os = "android")]
#[must_use]
pub fn remote_shell_enabled() -> bool {
   remote::enabled()
}

#[cfg(target_os = "android")]
pub(crate) fn set_remote_shell_flag(enabled: bool) {
   remote::set_flag(enabled);
}

/// Turns the localhost debug shell on or off and remembers the choice.
#[cfg(target_os = "android")]
pub fn set_remote_shell_enabled(enabled: bool) {
   remote::set_enabled(enabled);
   if let Some(engine) = engine() {
      engine.save_choices();
   }
}

/// Runs `code` in the plugin runtime's global scope, logs it, and returns its
/// formatted result or error.
pub fn evaluate_capture(code: &str) -> Result<String, String> {
   log::info!("> {code}");
   let result = engine().map(|engine| engine.evaluate(code));
   match result {
      Some(Ok(result)) => {
         log::info!("{result}");
         Ok(result)
      },
      Some(Err(err)) => {
         let message = err.to_string();
         log::error!("{message}");
         Err(message)
      },
      None => {
         let message = "the runtime is not running".to_owned();
         log::error!("{message}");
         Err(message)
      },
   }
}

/// Turns a plugin on or off and remembers the choice across restarts.
pub fn set_enabled(name: &str, enabled: bool) {
   if let Some(mut engine) = engine() {
      engine.set_enabled(name, enabled);
   }
}

/// Sets one of a plugin's settings and remembers it across restarts.
pub fn set_setting(plugin: &str, key: &str, value: Chosen) {
   if let Some(engine) = engine() {
      engine.set_setting(plugin, key, value);
   }
}

/// Runs a button setting's `onPress`.
pub fn press(plugin: &str, key: &str) {
   if let Some(engine) = engine() {
      engine.press(plugin, key);
   }
}

/// Runs `code` in the plugin runtime's global scope and logs it with its
/// result.
pub fn evaluate(code: &str) {
   log::info!("> {code}");
   let result = engine().map(|engine| engine.evaluate(code));
   match result {
      Some(Ok(result)) => log::info!("{result}"),
      Some(Err(err)) => log::error!("{err}"),
      None => log::error!("the runtime is not running"),
   }
}

/// Downloads a plugin bundle into `home/plugins` and loads it, replacing a
/// plugin of the same name. Returns the plugin's name.
#[cfg(target_os = "android")]
pub fn install(url: &str) -> Result<String, Error> {
   let bad_url = || Error::PluginUrl {
      url: url.to_owned(),
   };
   let rest = url.strip_prefix("https://").ok_or_else(bad_url)?;
   let path = rest.split(['?', '#']).next().unwrap_or_default();
   let name = path
      .rsplit('/')
      .next()
      .and_then(plugin_name)
      .ok_or_else(bad_url)?;

   let body = fetch::get(url)?;
   let source = String::from_utf8(body).map_err(|_| Error::NotUtf8 {
      url: url.to_owned(),
   })?;
   save_source(name, source)?;
   Ok(name.to_owned())
}

/// The plugin name a file name like `plugin.js` gives.
#[cfg(target_os = "android")]
fn plugin_name(file: &str) -> Option<&str> {
   file.strip_suffix(".js").filter(|name| {
      !name.is_empty()
         && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
   })
}

/// Installs a bundle the user picked from the device, named by its file name.
/// Returns the plugin's name.
#[cfg(target_os = "android")]
pub fn install_file(file: &str, bytes: Vec<u8>) -> Result<String, Error> {
   let name = plugin_name(file).ok_or_else(|| Error::PluginFile {
      name: file.to_owned(),
   })?;
   let source = String::from_utf8(bytes).map_err(|_| Error::NotUtf8 {
      url: file.to_owned(),
   })?;
   save_source(name, source)?;
   Ok(name.to_owned())
}

/// Writes `source` to `home/plugins/<name>.js` and loads it, replacing the
/// plugin's code, bundled or not.
#[cfg(target_os = "android")]
pub fn save_source(name: &str, source: String) -> Result<(), Error> {
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   let write = || {
      std::fs::create_dir_all(state.plugins_dir())?;
      std::fs::write(&file, &source)
   };
   write().map_err(io_error(&file))?;
   if let Some(mut engine) = engine() {
      engine.install(name, source);
   }
   Ok(())
}

/// A plugin's current code.
#[must_use]
pub fn source(name: &str) -> Option<String> {
   engine()?.source(name)
}

/// Deletes a plugin installed into `home/plugins` and unloads it.
#[cfg(target_os = "android")]
pub fn uninstall(name: &str) -> Result<(), Error> {
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   std::fs::remove_file(&file).map_err(io_error(&file))?;
   if let Some(mut engine) = engine() {
      engine.uninstall(name);
   }
   Ok(())
}

/// Evaluates an enabled plugin again with its hooks cleared.
pub fn reload(name: &str) {
   if let Some(mut engine) = engine() {
      engine.reload(name);
   }
}

/// What [`start`] found and loaded.
pub struct Started {
   pub target: String,
   pub build: Option<BuildId>,
   pub plugins: Vec<String>,
}

/// Starts phoenix against the first loaded library named in [`LIBRARIES`].
/// `bundled` plugins load first, and a file in `home/plugins` with the same
/// name replaces one.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   // SAFETY: the target is the host app's own library, which stays mapped for
   // the life of the process, and scans run before the host's threads touch its
   // data.
   let images = unsafe { Image::loaded() };
   let image = images
      .into_iter()
      .find(|image| LIBRARIES.contains(&image.file_name()))
      .ok_or(Error::NoTarget)?;
   let walked = GatewayRegistry::locate(&image)?;
   log::info!("gateway registry: {} events", walked.iter().count());

   let registry = gateway::swap(&image, walked)?;
   let engine = js::Engine::new(registry)?;
   let state = State {
      image,
      home,
      js: Mutex::new(engine),
   };
   STATE.set(state).map_err(|_| Error::AlreadyStarted)?;
   let state = STATE.get().expect("STATE was set above");

   themes::load_at_boot();
   let plugins = js::load_plugins(state, bundled)?;
   #[cfg(target_os = "android")]
   {
      if remote_shell_enabled() {
         remote::ensure_started();
      }
      settings::start();
      patches::start();
      themes::start();
      bridge::install(Box::new(socket::Socket));
   }
   Ok(Started {
      target: state.image.path.clone(),
      build: state.image.build_id,
      plugins,
   })
}

impl State {
   fn storage_path(&self, plugin: &str) -> PathBuf {
      self.home.join("storage").join(format!("{plugin}.json"))
   }

   fn choices_path(&self) -> PathBuf {
      self.home.join("plugins.json")
   }

   /// The app's `files` directory, which holds phoenix's home and Phoenix's
   /// store.
   #[cfg(target_os = "android")]
   fn files_dir(&self) -> PathBuf {
      self
         .home
         .parent()
         .map_or_else(|| self.home.clone(), Path::to_path_buf)
   }

   fn plugins_dir(&self) -> PathBuf {
      self.home.join("plugins")
   }
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
   move |source| Error::Io {
      path: path.to_owned(),
      source,
   }
}
