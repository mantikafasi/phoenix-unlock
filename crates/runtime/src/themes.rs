//! Custom theme persistence, installed theme storage, and the UI bridge sink
//! that applies custom colors, fonts, icons and emoji to Phoenix.

use std::{
   fs,
   io::ErrorKind,
   path::PathBuf,
   sync::{LazyLock, Mutex, MutexGuard, PoisonError},
};

use phoenix_ui::themes::{
   ColorsSelection, EmojiSelection, FontsSelection, IconsSelection, STATE_FORMAT, ThemeApplier,
   ThemeBase, ThemesState, parse_theme_json,
};

use crate::{Error, STATE, io_error};

/// An installed `.json` theme in `<home>/themes/installed`.
#[derive(Clone, Debug)]
pub struct InstalledTheme {
   pub id: String,
   pub name: String,
   pub description: Option<String>,
   pub authors: Vec<String>,
   pub active: bool,
}

static APPLIER: LazyLock<Mutex<ThemeApplier>> = LazyLock::new(|| Mutex::new(ThemeApplier::new()));

fn applier() -> MutexGuard<'static, ThemeApplier> {
   APPLIER.lock().unwrap_or_else(PoisonError::into_inner)
}

fn themes_dir() -> Option<PathBuf> {
   Some(STATE.get()?.home.join("themes"))
}

fn state_path() -> Option<PathBuf> {
   Some(themes_dir()?.join("state.json"))
}

fn installed_dir() -> Option<PathBuf> {
   Some(themes_dir()?.join("installed"))
}

/// Loads persisted theme state from `<home>/themes/state.json`.
pub fn load_at_boot() {
   let Some(path) = state_path() else {
      return;
   };
   let bytes = match fs::read(&path) {
      Ok(bytes) => bytes,
      Err(err) if err.kind() == ErrorKind::NotFound => return,
      Err(err) => {
         log::warn!("themes: reading {}: {err}", path.display());
         return;
      },
   };
   let state: ThemesState = match serde_json::from_slice(&bytes) {
      Ok(state) => state,
      Err(err) => {
         log::warn!("themes: parsing {}: {err}", path.display());
         return;
      },
   };
   if state.format != STATE_FORMAT {
      log::warn!(
         "themes: ignoring state.json with unsupported format {}",
         state.format
      );
      return;
   }
   let mut guard = applier();
   guard.set_state(state);
   if guard.is_active() {
      log::info!("themes: loaded saved theme state");
   }
}

fn save_state(state: &ThemesState) {
   let Some(path) = state_path() else {
      return;
   };
   let Ok(json) = serde_json::to_vec(state) else {
      log::warn!("themes: failed to serialize state.json");
      return;
   };
   if let Some(parent) = path.parent()
      && let Err(err) = fs::create_dir_all(parent)
   {
      log::warn!("themes: creating {}: {err}", parent.display());
      return;
   }
   let tmp = path.with_extension("json.tmp");
   if let Err(err) = fs::write(&tmp, &json) {
      log::warn!("themes: writing {}: {err}", tmp.display());
      return;
   }
   if let Err(err) = fs::rename(&tmp, &path) {
      let _ = fs::remove_file(&tmp);
      log::warn!("themes: replacing {}: {err}", path.display());
   }
}

#[must_use]
pub fn state() -> ThemesState {
   applier().state().clone()
}

#[must_use]
pub fn base() -> ThemeBase {
   applier().base()
}

pub fn set_colors(selection: Option<ColorsSelection>) {
   let ops = {
      let mut guard = applier();
      guard.set_colors(selection);
      save_state(guard.state());
      let ops = refresh_ops_for(&guard);
      drop(guard);
      ops
   };
   apply_refresh(&ops);
}

pub fn set_fonts(selection: Option<FontsSelection>) {
   let ops = {
      let mut guard = applier();
      guard.set_fonts(selection);
      save_state(guard.state());
      let ops = refresh_ops_for(&guard);
      drop(guard);
      ops
   };
   apply_refresh(&ops);
}

pub fn set_icons(selection: Option<IconsSelection>) {
   let ops = {
      let mut guard = applier();
      guard.set_icons(selection);
      save_state(guard.state());
      let ops = refresh_ops_for(&guard);
      drop(guard);
      ops
   };
   apply_refresh(&ops);
}

pub fn set_emoji(selection: Option<EmojiSelection>) {
   let ops = {
      let mut guard = applier();
      guard.set_emoji(selection);
      save_state(guard.state());
      let ops = refresh_ops_for(&guard);
      drop(guard);
      ops
   };
   apply_refresh(&ops);
}

pub fn clear_all() {
   let ops = {
      let mut guard = applier();
      guard.set_state(ThemesState::default());
      save_state(guard.state());
      let ops = refresh_ops_for(&guard);
      drop(guard);
      ops
   };
   apply_refresh(&ops);
}

/// Lists all installed `.json` themes in `<home>/themes/installed`.
#[must_use]
pub fn list_installed() -> Vec<InstalledTheme> {
   let Some(dir) = installed_dir() else {
      return Vec::new();
   };
   let Ok(entries) = fs::read_dir(&dir) else {
      return Vec::new();
   };
   let active_theme = applier()
      .state()
      .colors
      .as_ref()
      .map(|colors| colors.theme.clone());
   let mut out = Vec::new();
   for entry in entries.flatten() {
      let path = entry.path();
      if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
         continue;
      }
      let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
         continue;
      };
      let Ok(raw) = fs::read_to_string(&path) else {
         continue;
      };
      let Ok(parsed) = parse_theme_json(id, &raw) else {
         continue;
      };
      let active = active_theme.as_deref() == Some(id)
         || active_theme.as_deref() == Some(parsed.name.as_str());
      out.push(InstalledTheme {
         id: id.to_owned(),
         name: parsed.name,
         description: parsed.description,
         authors: parsed.authors,
         active,
      });
   }
   out.sort_by_key(|theme| theme.name.to_lowercase());
   out
}

/// Applies an installed theme by id (filename stem without `.json`).
pub fn apply_installed(id: &str) -> Result<(), Error> {
   let dir = installed_dir().ok_or(Error::NotStarted)?;
   let path = dir.join(format!("{id}.json"));
   let raw = fs::read_to_string(&path).map_err(io_error(&path))?;
   let mut parsed = parse_theme_json(id, &raw).map_err(|err| Error::Theme {
      message: format!("{id}: {err}"),
   })?;
   id.clone_into(&mut parsed.colors.theme);
   set_colors(Some(parsed.colors));
   Ok(())
}

/// Installs a `.json` theme from raw bytes and applies it immediately.
pub fn install_file(file: &str, bytes: &[u8]) -> Result<String, Error> {
   let id = theme_id(file).ok_or_else(|| Error::Theme {
      message: format!("{file}: theme files are .json files named with letters, digits, - or _"),
   })?;
   let text = String::from_utf8(bytes.to_vec()).map_err(|_| Error::NotUtf8 {
      url: file.to_owned(),
   })?;
   let mut parsed = parse_theme_json(id, &text).map_err(|err| Error::Theme {
      message: format!("{file}: invalid theme JSON: {err}"),
   })?;
   let dir = installed_dir().ok_or(Error::NotStarted)?;
   let path = dir.join(format!("{id}.json"));
   let write = || {
      fs::create_dir_all(&dir)?;
      fs::write(&path, &text)
   };
   write().map_err(io_error(&path))?;
   id.clone_into(&mut parsed.colors.theme);
   set_colors(Some(parsed.colors));
   Ok(parsed.name)
}

/// Downloads a `.json` theme from an `https://` URL, saves it, and applies it.
#[cfg(target_os = "android")]
pub fn install_url(url: &str) -> Result<String, Error> {
   let bad_url = || Error::Theme {
      message: format!("{url}: themes install from https:// URLs ending in a .json filename"),
   };
   let rest = url.strip_prefix("https://").ok_or_else(bad_url)?;
   let path = rest.split(['?', '#']).next().unwrap_or_default();
   let file = path.rsplit('/').next().ok_or_else(bad_url)?;
   if theme_id(file).is_none() {
      return Err(bad_url());
   }
   let body = crate::fetch::get(url)?;
   install_file(file, &body)
}

/// Deletes an installed `.json` theme, clearing active colors if it was active.
pub fn uninstall(id: &str) -> Result<(), Error> {
   let dir = installed_dir().ok_or(Error::NotStarted)?;
   let path = dir.join(format!("{id}.json"));
   fs::remove_file(&path).map_err(io_error(&path))?;
   let was_active = applier()
      .state()
      .colors
      .as_ref()
      .is_some_and(|colors| colors.theme == id);
   if was_active {
      set_colors(None);
   }
   Ok(())
}

fn theme_id(file: &str) -> Option<&str> {
   file.strip_suffix(".json").filter(|name| {
      !name.is_empty()
         && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
   })
}

#[cfg(target_os = "android")]
fn refresh_ops_for(guard: &ThemeApplier) -> Vec<phoenix_ui::Op> {
   guard.refresh_ops_with_emoji(Some(&android::draw_cached_emoji))
}

#[cfg(not(target_os = "android"))]
fn refresh_ops_for(guard: &ThemeApplier) -> Vec<phoenix_ui::Op> {
   guard.refresh_ops()
}

#[cfg(target_os = "android")]
fn apply_refresh(ops: &[phoenix_ui::Op]) {
   if ops.is_empty() {
      return;
   }
   if !crate::bridge::apply_ops(&phoenix_ui::encode(ops)) {
      log::warn!("themes: failed to apply live theme refresh");
   }
}

#[cfg(not(target_os = "android"))]
const fn apply_refresh(_ops: &[phoenix_ui::Op]) {}

#[cfg(not(target_os = "android"))]
#[must_use]
pub const fn system_palettes() -> Option<std::collections::BTreeMap<String, Vec<String>>> {
   None
}

#[cfg(target_os = "android")]
mod android {
   use std::{
      collections::{BTreeMap, HashMap},
      ffi::CString,
      ptr,
      sync::{LazyLock, Mutex, PoisonError},
   };

   use jni_sys::jvalue;
   use phoenix_ui::{decode, encode, themes::ThemeBase};
   use phoenix_wire::{HostCall, Value};

   use crate::{
      bridge::{self, Action, Sink},
      jvm::{clear_exception, env, jni},
      themes::applier,
   };

   /// `AppState.setTheme` in `hh.jb`, versionCode 356207.
   const APP_STATE: u16 = 9;
   const SET_THEME: u16 = 84;

   struct ThemeSink;

   impl Sink for ThemeSink {
      fn ops(&self, batch: &[u8]) -> Action {
         let mut ops = match decode(batch) {
            Ok(ops) => ops,
            Err(_) => return Action::Pass,
         };
         let changed = applier().rewrite_batch_with_emoji(&mut ops, Some(&draw_cached_emoji));
         if changed {
            Action::Replace(encode(&ops))
         } else {
            Action::Pass
         }
      }

      fn dispatch(&self, _frame: &[u8]) -> Action {
         Action::Pass
      }
   }

   /// In-memory cache of `(font, codepoints) -> data:image/png;base64,...` so
   /// repeated emoji in a chat batch avoid JNI round-trips.
   static EMOJI_DATA_URIS: LazyLock<Mutex<HashMap<(String, String), String>>> =
      LazyLock::new(|| Mutex::new(HashMap::new()));

   /// Renders `codepoints` with `font` (`"system"` or a `.ttf` path) into
   /// `<home>/themes/emoji-cache/<hash>/emoji-<codepoints>.png` and returns a
   /// `data:image/png;base64,...` URI for Phoenix's image loader.
   pub fn draw_cached_emoji(codepoints: &str, font: &str) -> Option<String> {
      let key = (font.to_owned(), codepoints.to_owned());
      if let Some(cached) = EMOJI_DATA_URIS
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .get(&key)
      {
         return Some(cached.clone());
      }
      let base = crate::themes::themes_dir()?;
      let hash = crc32fast::hash(font.as_bytes());
      let out = base
         .join("emoji-cache")
         .join(format!("{hash:08x}"))
         .join(format!("emoji-{codepoints}.png"));
      let data_uri = crate::picker::draw_emoji(codepoints, font, &out.to_string_lossy())?;
      EMOJI_DATA_URIS
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .insert(key, data_uri.clone());
      Some(data_uri)
   }

   /// Installs the theme UI-op sink on the bridge.
   pub fn start() {
      bridge::install(Box::new(ThemeSink));
   }

   /// True when a host invoke header targets `AppState.setTheme`.
   pub fn wants(head: &[u8]) -> bool {
      HostCall::target(head) == Some((APP_STATE, SET_THEME))
   }

   /// Tracks Discord's active base theme from `AppState.setTheme` calls.
   /// Phoenix immediately follows `setTheme` with a UI batch in the new base,
   /// which `ThemeSink::ops` rewrites with the rebuilt palette.
   pub fn observe_call(frame: &[u8]) {
      let Ok(call) = HostCall::decode(frame) else {
         return;
      };
      let Some(base) = extract_base(&call.args) else {
         return;
      };
      if applier().set_base(base) {
         log::info!("themes: base theme changed to {}", base.as_str());
      }
   }

   fn extract_base(value: &Value) -> Option<ThemeBase> {
      match value {
         Value::Str(name) => ThemeBase::parse(name),
         Value::I32(index) => Some(ThemeBase::from_index(*index)),
         Value::I64(index) => i32::try_from(*index).ok().map(ThemeBase::from_index),
         Value::Obj(fields) => fields.iter().find_map(|(_, field)| extract_base(field)),
         Value::Arr(items) => items.iter().find_map(extract_base),
         _ => None,
      }
   }

   const PALETTES: [&str; 5] = ["accent1", "accent2", "accent3", "neutral1", "neutral2"];
   const SHADES: [u16; 13] = [0, 10, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

   /// Reads Android 12+ Material You dynamic palettes (`android:color/system_*`)
   /// from the application's `Resources`. Returns `None` on Android < 12 or
   /// when system resources are unavailable.
   #[must_use]
   pub fn system_palettes() -> Option<BTreeMap<String, Vec<String>>> {
      let env = env()?;
      // SAFETY: `env` belongs to the current thread, and PushLocalFrame frees
      // all local references created inside the block.
      unsafe {
         if jni!(env, PushLocalFrame, 128) != 0 {
            clear_exception(env);
            return None;
         }
         let result = read_palettes_in_frame(env);
         jni!(env, PopLocalFrame, ptr::null_mut());
         result
      }
   }

   /// # Safety
   ///
   /// Caller must hold a live `JNIEnv` inside a local frame.
   unsafe fn read_palettes_in_frame(
      env: *mut jni_sys::JNIEnv,
   ) -> Option<BTreeMap<String, Vec<String>>> {
      // SAFETY: JNI calls use valid class names, method signatures and object
      // references checked for null and exceptions at each step.
      unsafe {
         let thread_cls = jni!(env, FindClass, c"android/app/ActivityThread".as_ptr());
         if thread_cls.is_null() {
            clear_exception(env);
            return None;
         }
         let current_app = jni!(
            env,
            GetStaticMethodID,
            thread_cls,
            c"currentApplication".as_ptr(),
            c"()Landroid/app/Application;".as_ptr()
         );
         if current_app.is_null() {
            clear_exception(env);
            return None;
         }
         let app = jni!(
            env,
            CallStaticObjectMethodA,
            thread_cls,
            current_app,
            ptr::null()
         );
         if app.is_null() || jni!(env, ExceptionCheck) {
            clear_exception(env);
            return None;
         }

         let ctx_cls = jni!(env, GetObjectClass, app);
         let get_res = jni!(
            env,
            GetMethodID,
            ctx_cls,
            c"getResources".as_ptr(),
            c"()Landroid/content/res/Resources;".as_ptr()
         );
         if get_res.is_null() {
            clear_exception(env);
            return None;
         }
         let res = jni!(env, CallObjectMethodA, app, get_res, ptr::null());
         if res.is_null() || jni!(env, ExceptionCheck) {
            clear_exception(env);
            return None;
         }

         let res_cls = jni!(env, GetObjectClass, res);
         let get_id = jni!(
            env,
            GetMethodID,
            res_cls,
            c"getIdentifier".as_ptr(),
            c"(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I".as_ptr()
         );
         let get_color = jni!(
            env,
            GetMethodID,
            res_cls,
            c"getColor".as_ptr(),
            c"(ILandroid/content/res/Resources$Theme;)I".as_ptr()
         );
         if get_id.is_null() || get_color.is_null() {
            clear_exception(env);
            return None;
         }

         let def_type = jni!(env, NewStringUTF, c"color".as_ptr());
         let def_pkg = jni!(env, NewStringUTF, c"android".as_ptr());
         let mut map = BTreeMap::new();
         for palette in PALETTES {
            let mut shades = Vec::with_capacity(SHADES.len());
            for shade in SHADES {
               let Ok(name) = CString::new(format!("system_{palette}_{shade}")) else {
                  return None;
               };
               let jname = jni!(env, NewStringUTF, name.as_ptr());
               let args = [
                  jvalue { l: jname },
                  jvalue { l: def_type },
                  jvalue { l: def_pkg },
               ];
               let id = jni!(env, CallIntMethodA, res, get_id, args.as_ptr());
               jni!(env, DeleteLocalRef, jname);
               if id == 0 || jni!(env, ExceptionCheck) {
                  clear_exception(env);
                  return None;
               }
               let color_args = [jvalue { i: id }, jvalue { l: ptr::null_mut() }];
               let color = jni!(env, CallIntMethodA, res, get_color, color_args.as_ptr());
               if jni!(env, ExceptionCheck) {
                  clear_exception(env);
                  return None;
               }
               let rgb = color.cast_unsigned() & 0x00FF_FFFF;
               shades.push(format!("#{rgb:06x}"));
            }
            map.insert(palette.to_owned(), shades);
         }
         Some(map)
      }
   }
}

#[cfg(target_os = "android")]
pub use android::{observe_call, start, system_palettes, wants};
