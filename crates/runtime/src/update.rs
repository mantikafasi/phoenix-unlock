//! Tells the user when the site ships a newer Phoenix Unlock: a newer loader,
//! or plugin bundles that differ from the installed ones. The site publishes
//! both in `version.json` (`web/tools/bundle-plugins.mjs`).
//!
//! Applying an update works in-app:
//! - Plugin-only updates download each changed `plugins/<name>.js`, verify its
//!   CRC-32 against `version.json`, write it to `files/phoenix/plugins/`, and
//!   hot-reload it live with zero APK reinstall.
//! - Loader updates download `plugins/libphoenix-arm64.so`, stream-repack the
//!   currently installed APK swapping only `lib/<abi>/libplm.so` (16 KiB
//!   aligned, zero re-compression), sign with APK Signature Scheme v3, and open
//!   Android's native `PackageInstaller` update prompt.

use std::{
   collections::BTreeMap,
   sync::{
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
};

use memchr::memmem;

/// This build's version, `<UTC commit date>-<short hash>` (`build.rs`).
pub const VERSION: &str = env!("PHOENIX_VERSION");
pub const SITE: &str = "https://mantikafasi.github.io/phoenix-unlock/";

/// Where `bundle-plugins.mjs` reads the version from the loader the site ships.
#[used]
static MARKER: &str = concat!("phoenix-version:", env!("PHOENIX_VERSION"), ";");

/// Public signing key used by the web patcher and local CLI so in-app updates
/// preserve the signing identity (and Discord login).
const SIGNING_KEY_JSON: &str = include_str!("../../../web/signing-key.json");

const LOADER_ASSET: &str = if cfg!(target_arch = "x86_64") {
   "plugins/libphoenix-x86_64.so"
} else {
   "plugins/libphoenix-arm64.so"
};

#[derive(serde::Deserialize)]
struct Site {
   loader:  String,
   plugins: BTreeMap<String, u32>,
}

#[derive(serde::Deserialize)]
struct SigningKeyPair {
   pkcs8: String,
   spki:  String,
   cert:  String,
}

#[derive(serde::Deserialize)]
struct SigningKey {
   new:     SigningKeyPair,
   lineage: String,
}

/// What the site has that this install lacks.
#[derive(Clone)]
pub struct Update {
   /// The site's version, when it is newer than [`VERSION`].
   pub loader:      Option<String>,
   /// Installed plugins whose code differs from the site's.
   pub plugins:     Vec<String>,
   /// Expected CRC-32 for each plugin in [`Self::plugins`].
   pub plugin_crcs: BTreeMap<String, u32>,
}

static FOUND: Mutex<Option<Update>> = Mutex::new(None);
static STATUS: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// The update the last check found, if any.
pub fn found() -> Option<Update> {
   FOUND
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Human-readable status of the current or last update attempt, if any.
pub fn status() -> Option<String> {
   STATUS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Whether an update download/install is currently running.
pub fn busy() -> bool {
   BUSY.load(Ordering::Relaxed)
}

fn set_status(message: Option<String>) {
   *STATUS.lock().unwrap_or_else(PoisonError::into_inner) = message;
   crate::settings::refresh();
}

/// Asks the site in the background. Failures (offline) are only logged.
pub fn check() {
   thread::spawn(|| {
      let site = match crate::fetch::get(&format!("{SITE}version.json")) {
         Ok(body) => serde_json::from_slice::<Site>(&body),
         Err(err) => {
            log::info!("update check: {err}");
            return;
         },
      };
      let site = match site {
         Ok(site) => site,
         Err(err) => {
            log::info!("update check: version.json: {err}");
            return;
         },
      };
      let mut plugins = Vec::new();
      let mut plugin_crcs = BTreeMap::new();
      for plugin in crate::plugins() {
         if let Some(&crc) = site.plugins.get(&plugin.name)
            && crate::source(&plugin.name)
               .is_some_and(|source| crc32fast::hash(source.as_bytes()) != crc)
         {
            plugin_crcs.insert(plugin.name.clone(), crc);
            plugins.push(plugin.name);
         }
      }
      let update = Update {
         loader: (site.loader.as_str() > VERSION).then_some(site.loader),
         plugins,
         plugin_crcs,
      };
      if update.loader.is_none() && update.plugins.is_empty() {
         log::info!("up to date ({VERSION})");
         *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = None;
         crate::settings::refresh();
         return;
      }
      log::info!(
         "update available: loader {:?}, plugins {:?}",
         update.loader,
         update.plugins
      );
      let body = match (&update.loader, update.plugins.is_empty()) {
         (Some(version), true) => format!("Build {version} — tap to update in-app"),
         (Some(version), false) => {
            format!(
               "Build {version} & {} plugin update{} — tap to update",
               update.plugins.len(),
               if update.plugins.len() == 1 { "" } else { "s" }
            )
         },
         (None, false) => format!("Updated plugins: {} — tap to update now", update.plugins.join(", ")),
         (None, true) => String::new(),
      };
      *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = Some(update);
      crate::settings::refresh();
      crate::patches::show_banner(
         "Phoenix Unlock update available".to_owned(),
         Some(body),
         Some("res://ic_mana_puzzle_piece_retry".to_owned()),
         Some(0xFF23_A55A),
         8000,
         crate::patches::BannerAction::ApplyUpdate,
      );
   });
}

/// Applies the pending update found by [`check`].
///
/// Updates changed plugins over-the-air first (hot-reloading them live), then
/// runs the in-app APK self-updater if a newer loader is available.
pub fn apply() {
   let Some(update) = found() else {
      log::info!("update apply: no pending update");
      return;
   };
   if BUSY.swap(true, Ordering::AcqRel) {
      log::info!("update apply: already running");
      let _ = crate::picker::toast("Update already in progress...");
      return;
   }
   thread::spawn(move || {
      let mut updated_plugins = Vec::new();
      if !update.plugins.is_empty() {
         set_status(Some(format!(
            "Updating {} plugin{}...",
            update.plugins.len(),
            if update.plugins.len() == 1 { "" } else { "s" }
         )));
         let mut failed = Vec::new();
         for name in &update.plugins {
            let expected_crc = update.plugin_crcs.get(name).copied();
            match update_plugin(name, expected_crc) {
               Ok(()) => {
                  log::info!("ota updated plugin {name}");
                  updated_plugins.push(name.clone());
               },
               Err(err) => {
                  log::error!("ota update {name} failed: {err}");
                  failed.push(format!("{name}: {err}"));
               },
            }
         }
         // Drop successfully updated plugins from FOUND.
         {
            let mut guard = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(pending) = guard.as_mut() {
               pending.plugins.retain(|name| !updated_plugins.contains(name));
               pending.plugin_crcs.retain(|name, _| !updated_plugins.contains(name));
               if pending.loader.is_none() && pending.plugins.is_empty() {
                  *guard = None;
               }
            }
         }
         if !failed.is_empty() && update.loader.is_none() {
            let msg = format!("Plugin update failed: {}", failed.join("; "));
            set_status(Some(msg.clone()));
            BUSY.store(false, Ordering::Release);
            let _ = crate::picker::toast(&msg);
            return;
         }
      }

      if update.loader.is_some() {
         run_loader_self_update();
         return;
      }

      // Plugin-only OTA update completed cleanly.
      set_status(None);
      BUSY.store(false, Ordering::Release);
      crate::settings::refresh();
      crate::patches::show_banner(
         "Plugins updated".to_owned(),
         Some(format!("Reloaded: {}", updated_plugins.join(", "))),
         Some("res://ic_mana_checkmark_large".to_owned()),
         Some(0xFF23_A55A),
         4500,
         crate::patches::BannerAction::None,
      );
   });
}

/// Runs the in-app loader self-update directly against the site's current
/// `libphoenix-*.so` (also exposed to the gated localhost debug shell).
pub fn self_update_from_site() {
   if BUSY.swap(true, Ordering::AcqRel) {
      log::info!("self-update: already running");
      return;
   }
   thread::spawn(run_loader_self_update);
}

fn update_plugin(name: &str, expected_crc: Option<u32>) -> Result<(), String> {
   let url = format!("{SITE}plugins/{name}.js");
   let bytes = crate::fetch::get(&url).map_err(|err| format!("fetch: {err}"))?;
   if let Some(expected) = expected_crc {
      let actual = crc32fast::hash(&bytes);
      if actual != expected {
         return Err(format!("CRC mismatch (got {actual:#x}, expected {expected:#x})"));
      }
   }
   let source = String::from_utf8(bytes).map_err(|err| format!("invalid UTF-8: {err}"))?;
   crate::save_source(name, source).map_err(|err| err.to_string())
}

fn run_loader_self_update() {
   set_status(Some("Downloading loader...".to_owned()));
   let _ = crate::picker::toast("Downloading Phoenix Unlock update...");

   let url = format!("{SITE}{LOADER_ASSET}");
   let bytes = match crate::fetch::get(&url) {
      Ok(bytes) => bytes,
      Err(err) => {
         fail_self_update(format!("Download failed: {err}"));
         return;
      },
   };
   if !bytes.starts_with(b"\x7fELF") || memmem::find(&bytes, b"phoenix-version:").is_none() {
      fail_self_update("Downloaded loader is invalid".to_owned());
      return;
   }

   let keys: SigningKey = match serde_json::from_str(SIGNING_KEY_JSON) {
      Ok(keys) => keys,
      Err(err) => {
         fail_self_update(format!("Signing key config invalid: {err}"));
         return;
      },
   };

   set_status(Some("Repacking & signing APK...".to_owned()));
   if let Err(err) = crate::picker::self_update(
      &bytes,
      &keys.new.pkcs8,
      &keys.new.spki,
      &keys.new.cert,
      &keys.lineage,
      SITE,
   ) {
      fail_self_update(format!("Self-update failed: {err}"));
   }
}

fn fail_self_update(message: String) {
   log::error!("self-update: {message}");
   set_status(Some(message.clone()));
   BUSY.store(false, Ordering::Release);
   let _ = crate::picker::toast(&message);
}

/// Callback invoked from `phx.Updater.onUpdateStatus(state, message)`.
pub fn on_native_status(state: &str, message: &str) {
   log::info!("self-update [{state}]: {message}");
   match state {
      "repacking" | "installing" => {
         set_status(Some(message.to_owned()));
      },
      "done" => {
         *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = None;
         set_status(None);
         BUSY.store(false, Ordering::Release);
         crate::patches::show_banner(
            "Phoenix Unlock updated".to_owned(),
            Some(message.to_owned()),
            Some("res://ic_mana_checkmark_large".to_owned()),
            Some(0xFF23_A55A),
            4000,
            crate::patches::BannerAction::None,
         );
      },
      "permission" | "fallback" | "cancelled" | "error" => {
         set_status(Some(message.to_owned()));
         BUSY.store(false, Ordering::Release);
         let _ = crate::picker::toast(message);
      },
      _ => {
         set_status(Some(message.to_owned()));
      },
   }
}
