//! Tells the user when the site ships a newer Phoenix Unlock: a newer loader,
//! or plugin bundles that differ from the installed ones. The site publishes
//! both in `version.json` (`web/tools/bundle-plugins.ts`).
//!
//! Plugin updates download each changed `plugins/<name>.js`, verify its CRC-32
//! against `version.json` and hot-reload it. A loader update is the platform's
//! ([`crate::Platform::update_loader`]).

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs,
   path::Path,
   sync::{
      Mutex,
      MutexGuard,
      PoisonError,
   },
   thread,
};

use crate::{
   Notice,
   permissions::{
      Permission,
      Permissions,
   },
};
/// This build's version, `<UTC commit date>-<short hash>` (`build.rs`).
pub const VERSION: &str = env!("PHOENIX_VERSION");
pub const SITE: &str = "https://sunbird.tools/";
pub const ASSETS: &str = "https://assets.sunbird.tools/";

/// Where `bundle-plugins.ts` reads the version from the loader the site ships.
#[used]
static MARKER: &str = concat!("phoenix-version:", env!("PHOENIX_VERSION"), ";");

#[derive(serde::Deserialize)]
struct Site {
   loader:        String,
   #[serde(default)]
   loader_crcs:   BTreeMap<String, u32>,
   #[serde(default)]
   launcher_crcs: BTreeMap<String, u32>,
   plugins:       BTreeMap<String, u32>,
}

/// What the site has that this install lacks.
#[derive(Clone)]
pub struct Update {
   /// The site's version, when it is newer than [`VERSION`].
   pub loader:        Option<String>,
   /// Expected CRC-32 for each loader binary listed in `version.json`.
   pub loader_crcs:   BTreeMap<String, u32>,
   /// Expected CRC-32 for each desktop launcher listed in `version.json`.
   pub launcher_crcs: BTreeMap<String, u32>,
   /// Installed plugins whose code differs from the site's.
   pub plugins:       Vec<String>,
   /// Expected CRC-32 for each plugin in [`Self::plugins`].
   pub plugin_crcs:   BTreeMap<String, u32>,
}

/// A plugin the site offers, as `plugins/index.json` lists it.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Listed {
   pub name:        String,
   pub description: String,
   #[serde(default)]
   pub warning:     Option<String>,
}

/// The site's plugin list, fetched when the install page first opens.
#[derive(Clone)]
pub enum Catalog {
   Unfetched,
   Loading,
   Loaded(Vec<Listed>),
   Failed(String),
}

static CATALOG: Mutex<Catalog> = Mutex::new(Catalog::Unfetched);
static FOUND: Mutex<Option<Update>> = Mutex::new(None);
/// The launchers' CRC-32s from the last `version.json`, kept apart from
/// [`FOUND`] since a launcher refresh alone is no update to tell anyone about.
static LAUNCHERS: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());
static PROGRESS: Mutex<Progress> = Mutex::new(Progress::Idle);

/// What the one update slot is doing. Only a [`Slot`] leaves `Idle`.
enum Progress {
   Idle,
   Running(String),
   Finished(String),
}

/// The claim on the update slot. Dropping it releases the slot, with the line
/// [`Slot::finish`] left or none.
#[must_use]
pub struct Slot {
   result: Option<String>,
}

impl Slot {
   /// Shows `step` while the update runs.
   pub fn step(&self, step: impl Into<String>) {
      *progress() = Progress::Running(step.into());
      crate::refresh_settings();
   }

   /// Releases the slot with `result` shown.
   pub fn finish(mut self, result: Option<String>) {
      self.result = result;
   }
}

impl Drop for Slot {
   fn drop(&mut self) {
      *progress() = self
         .result
         .take()
         .map_or(Progress::Idle, Progress::Finished);
      crate::refresh_settings();
   }
}

fn progress() -> MutexGuard<'static, Progress> {
   PROGRESS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Claims the update slot showing `step`. `None` when an update already runs.
pub fn begin(step: impl Into<String>) -> Option<Slot> {
   let mut progress = progress();
   if matches!(*progress, Progress::Running(_)) {
      return None;
   }
   *progress = Progress::Running(step.into());
   drop(progress);
   crate::refresh_settings();
   Some(Slot { result: None })
}

/// The update the last check found, if any.
pub fn found() -> Option<Update> {
   FOUND.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The step the running update shows, or the line the last one left.
#[must_use]
pub fn status() -> Option<String> {
   match &*progress() {
      Progress::Idle => None,
      Progress::Running(line) | Progress::Finished(line) => Some(line.clone()),
   }
}

/// Whether an update holds the slot.
#[must_use]
pub fn busy() -> bool {
   matches!(*progress(), Progress::Running(_))
}

pub fn catalog() -> Catalog {
   CATALOG
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Fetches the site's plugin list in the background, unless it is loaded or
/// on its way. A failed fetch is retried on the next call.
pub fn fetch_catalog() {
   {
      let mut catalog = CATALOG.lock().unwrap_or_else(PoisonError::into_inner);
      if matches!(*catalog, Catalog::Loading | Catalog::Loaded(_)) {
         return;
      }
      *catalog = Catalog::Loading;
   }
   thread::spawn(|| {
      let fetched = crate::fetch::get(&format!("{ASSETS}plugins/index.json"))
         .map_err(|err| err.to_string())
         .and_then(|body| {
            serde_json::from_slice::<Vec<Listed>>(&body).map_err(|err| err.to_string())
         });
      *CATALOG.lock().unwrap_or_else(PoisonError::into_inner) = match fetched {
         Ok(listed) => Catalog::Loaded(listed),
         Err(err) => {
            log::info!("plugin list: {err}");
            Catalog::Failed(err)
         },
      };
      crate::refresh_settings();
   });
}

/// Installs a plugin the site lists. Site plugins can call API an older loader
/// lacks, so they wait until the loader is current.
pub fn install_listed(name: &str) -> Result<crate::installer::Installed, crate::Error> {
   if let Some(version) = found().and_then(|update| update.loader) {
      return Err(crate::Error::LoaderOutdated { version });
   }
   crate::install(&format!("{ASSETS}plugins/{name}.js"))
}

/// Whether a plugin that needs loader `min_sunbird` is held back on this one.
/// Versions are `<UTC date>-<hash>`, which sort by date first as strings.
#[must_use]
pub fn outdated(min_sunbird: &str) -> bool {
   min_sunbird > VERSION
}

/// Asks the site what this install lacks, and remembers it for [`found`].
/// `None` means up to date.
fn query() -> Result<Option<Update>, String> {
   let body = crate::fetch::get(&format!("{ASSETS}version.json")).map_err(|err| err.to_string())?;
   let site =
      serde_json::from_slice::<Site>(&body).map_err(|err| format!("version.json: {err}"))?;
   let loader = (site.loader.as_str() > VERSION).then_some(site.loader);
   let updates_loader = crate::platform().is_some_and(<dyn crate::Platform>::updates_loader);
   // Plugins come along with an in-app loader update, so one tap brings both. Elsewhere the
   // site's plugins would run on this older loader, so they wait for the new one.
   let plugins_wait = loader.is_some() && !updates_loader;
   let mut plugins = Vec::new();
   let mut plugin_crcs = BTreeMap::new();
   for plugin in crate::plugins() {
      if !plugins_wait
         && let Some(&crc) = site.plugins.get(&plugin.name)
         && crate::source(&plugin.name)
            .is_some_and(|source| crc32fast::hash(source.as_bytes()) != crc)
      {
         plugin_crcs.insert(plugin.name.clone(), crc);
         plugins.push(plugin.name);
      }
   }
   LAUNCHERS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone_from(&site.launcher_crcs);
   let update = (loader.is_some() || !plugins.is_empty()).then_some(Update {
      loader,
      loader_crcs: site.loader_crcs,
      launcher_crcs: site.launcher_crcs,
      plugins,
      plugin_crcs,
   });
   FOUND
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone_from(&update);
   crate::refresh_settings();
   Ok(update)
}

/// Asks the site on this thread, for callers that act on the answer right
/// away. `Ok(None)` means up to date.
pub fn check_blocking() -> Result<Option<Update>, String> {
   query()
}

/// The desktop launchers' CRC-32s from the last check, empty before one.
#[must_use]
pub fn launcher_crcs() -> BTreeMap<String, u32> {
   LAUNCHERS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Downloads `plugins/<asset>` and checks that it starts with `magic` and
/// has CRC-32 `expected_crc`, for files `version.json` lists without a
/// version marker (the desktop launchers).
pub fn download_verified(asset: &str, magic: &[u8], expected_crc: u32) -> Result<Vec<u8>, String> {
   let bytes = crate::fetch::get(&format!("{ASSETS}plugins/{asset}"))
      .map_err(|err| format!("download {asset}: {err}"))?;
   verify_asset(asset, magic, expected_crc, &bytes)?;
   Ok(bytes)
}

fn verify_asset(asset: &str, magic: &[u8], expected: u32, bytes: &[u8]) -> Result<(), String> {
   if !bytes.starts_with(magic) {
      return Err(format!("the downloaded {asset} is not what the site lists"));
   }
   let actual = crc32fast::hash(bytes);
   if actual != expected {
      return Err(format!(
         "CRC mismatch for {asset} (got {actual:#x}, expected {expected:#x})"
      ));
   }
   Ok(())
}

/// Downloads loader `asset` for `version` and checks that it starts with
/// `magic` (the platform's executable header) and passes [`verify_loader`].
pub fn download_loader(
   asset: &str,
   magic: &[u8],
   version: &str,
   update: &Update,
) -> Result<Vec<u8>, String> {
   let bytes = crate::fetch::get(&format!("{ASSETS}plugins/{asset}"))
      .map_err(|err| format!("download {asset}: {err}"))?;
   if !bytes.starts_with(magic) {
      return Err(format!("the downloaded {asset} is not a loader binary"));
   }
   verify_loader(asset, version, &bytes, update)?;
   Ok(bytes)
}

/// Asks the site in the background. Failures (offline) are only logged.
pub fn check() {
   thread::spawn(check_and_notify);
}

/// [`check`] on this thread, for callers with more to do once it answers.
pub fn check_and_notify() {
   let update = match query() {
      Ok(Some(update)) => update,
      Ok(None) => {
         log::info!("up to date ({VERSION})");
         return;
      },
      Err(err) => {
         log::info!("update check: {err}");
         return;
      },
   };
   let restored = restore_permissions(&update);
   if !restored.is_empty() {
      log::info!("restored permissions of {restored:?}");
      forget_updated(&restored);
      if let Some(platform) = crate::platform() {
         platform.notify(Notice::PluginsUpdated(&restored));
      }
   }
   let Some(update) = found() else {
      return;
   };
   log::info!(
      "update available: loader {:?}, plugins {:?}",
      update.loader,
      update.plugins
   );
   if let Some(platform) = crate::platform() {
      platform.notify(Notice::UpdateAvailable(&update));
   }
}

/// Checks the site on demand for a user click, updating status and showing a
/// toast on completion.
pub fn check_now() {
   let Some(slot) = begin("Checking for updates...") else {
      log::info!("update check: already running");
      crate::toast("Update check already in progress...");
      return;
   };
   thread::spawn(move || {
      match query() {
         Ok(Some(update)) => {
            log::info!(
               "update available: loader {:?}, plugins {:?}",
               update.loader,
               update.plugins
            );
            drop(slot);
            if let Some(platform) = crate::platform() {
               platform.notify(Notice::UpdateAvailable(&update));
            }
         },
         Ok(None) => {
            log::info!("up to date ({VERSION})");
            slot.finish(Some(format!("Sunbird {VERSION} is up to date.")));
            crate::toast("Sunbird is up to date");
         },
         Err(err) => {
            log::info!("update check: {err}");
            let msg = format!("Update check failed: {err}");
            slot.finish(Some(msg.clone()));
            crate::toast(&msg);
         },
      }
   });
}

/// Checks the site again and applies what this install lacks.
///
/// Updates changed plugins over-the-air first (hot-reloading them live), then
/// runs the in-app APK self-updater if a newer loader is available.
pub fn apply() {
   let Some(slot) = begin("Checking for updates...") else {
      log::info!("update apply: already running");
      crate::toast("Update already in progress...");
      return;
   };
   thread::spawn(move || {
      // The last check ran when Discord started, and the site may have shipped
      // since, so what it found can be out of date by the time this runs.
      let update = query().unwrap_or_else(|err| {
         log::info!("update apply: {err}, using the last check");
         found()
      });
      let Some(update) = update else {
         log::info!("update apply: up to date ({VERSION})");
         drop(slot);
         crate::toast("Sunbird is up to date");
         return;
      };
      // The site's plugins are built against its loader and can call API this
      // one lacks, so with a loader update they wait in `plugins.next/` for it.
      if let Some(version) = &update.loader
         && let Some(platform) = crate::platform()
      {
         if platform.updates_loader() {
            platform.update_loader(slot, version, &update);
            return;
         }
         let msg = format!(
            "Sunbird {version} is available, {}.",
            platform.loader_update_hint()
         );
         slot.finish(Some(msg.clone()));
         crate::toast(&msg);
         return;
      }
      let mut updated_plugins = Vec::new();
      let mut failed = Vec::new();
      if !update.plugins.is_empty() {
         slot.step(format!(
            "Updating {} plugin{}...",
            update.plugins.len(),
            if update.plugins.len() == 1 { "" } else { "s" }
         ));
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
         forget_updated(&updated_plugins);
      }

      // A failed plugin used to go unreported whenever the loader update below
      // still had to run, since only `update.loader.is_none()` printed it.
      // The slot is released either way, and the failure keeps its line.
      if failed.is_empty() {
         drop(slot);
      } else {
         let msg = format!("Plugin update failed: {}", failed.join("; "));
         log::error!("{msg}");
         slot.finish(Some(msg.clone()));
         crate::toast(&msg);
      }
      if !updated_plugins.is_empty()
         && let Some(platform) = crate::platform()
      {
         platform.notify(Notice::PluginsUpdated(&updated_plugins));
      }
   });
}

/// Drops `updated` from what [`found`] still offers.
fn forget_updated(updated: &[String]) {
   let mut guard = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(pending) = guard.as_mut() {
      pending.plugins.retain(|name| !updated.contains(name));
      pending
         .plugin_crcs
         .retain(|name, _| !updated.contains(name));
      if pending.loader.is_none() && pending.plugins.is_empty() {
         *guard = None;
      }
   }
}

/// Copies from before the `@permissions` line load with no permissions, so an
/// update that declares some installs at launch instead of waiting for a tap.
fn restore_permissions(update: &Update) -> Vec<String> {
   if update.loader.is_some() {
      return Vec::new();
   }
   let declares = |source: &str| {
      source
         .parse::<Permissions>()
         .is_ok_and(|permissions| !permissions.declared().is_empty())
   };
   let mut restored = Vec::new();
   for name in &update.plugins {
      if crate::source(name).is_none_or(|source| declares(&source)) {
         continue;
      }
      let installed =
         download_plugin(name, update.plugin_crcs.get(name).copied()).and_then(|source| {
            if !declares(&source) {
               return Ok(false);
            }
            install_update(name, source).map(|()| true)
         });
      match installed {
         Ok(true) => restored.push(name.clone()),
         Ok(false) => {},
         Err(err) => log::error!("restoring {name}'s permissions failed: {err}"),
      }
   }
   restored
}

fn update_plugin(name: &str, expected_crc: Option<u32>) -> Result<(), String> {
   install_update(name, download_plugin(name, expected_crc)?)
}

fn install_update(name: &str, source: String) -> Result<(), String> {
   if let Some(min) = crate::Header::from(source.as_str()).min_sunbird
      && outdated(&min)
   {
      return Err(format!("needs Sunbird {min}, update Sunbird first"));
   }
   same_reach(name, &source)?;
   crate::save_source(name, source).map_err(|err| err.to_string())
}

/// Refuses an update that declares more than the running copy and the
/// bundled one, since the user agreed only to what those reach. A running
/// copy that declares nothing was installed before the `@permissions` line,
/// so its update takes what it declares.
fn same_reach(name: &str, source: &str) -> Result<(), String> {
   let Some(running) = crate::source(name) else {
      return Ok(());
   };
   let newer = source
      .parse::<Permissions>()
      .map_err(|err| err.to_string())?;
   let parse = |copy: &str| {
      copy
         .parse::<Permissions>()
         .unwrap_or_else(|_| Permissions::Declared(BTreeSet::new()))
   };
   let current = parse(&running);
   if current.declared().is_empty() {
      return Ok(());
   }
   let mut added = current.widened(&newer);
   if let Some(bundled) = crate::bundled_source(name) {
      let past_bundled = parse(&bundled).widened(&newer);
      added.retain(|permission| past_bundled.contains(permission));
   }
   if added.is_empty() {
      return Ok(());
   }
   Err(format!(
      "the update asks for {}, remove it and install it again from the plugin list to allow that",
      added
         .iter()
         .map(Permission::to_string)
         .collect::<Vec<_>>()
         .join(" ")
   ))
}

/// Verifies a downloaded loader binary against its exact embedded version
/// marker and the CRC-32 published for `asset` in `version.json`.
pub fn verify_loader(
   asset: &str,
   version: &str,
   bytes: &[u8],
   update: &Update,
) -> Result<(), String> {
   let marker = format!("phoenix-version:{version};");
   if !bytes
      .windows(marker.len())
      .any(|window| window == marker.as_bytes())
   {
      return Err(format!("missing version marker for {version}"));
   }
   let expected = update
      .loader_crcs
      .get(asset)
      .copied()
      .ok_or_else(|| format!("missing CRC-32 for {asset} in version.json"))?;
   let actual = crc32fast::hash(bytes);
   if actual != expected {
      return Err(format!(
         "CRC mismatch for {asset} (got {actual:#x}, expected {expected:#x})"
      ));
   }
   Ok(())
}

/// The site's `plugins/<name>.js`, checked against its CRC-32.
pub fn download_plugin(name: &str, expected_crc: Option<u32>) -> Result<String, String> {
   let url = format!("{ASSETS}plugins/{name}.js");
   let bytes = crate::fetch::get(&url).map_err(|err| format!("fetch: {err}"))?;
   if let Some(expected) = expected_crc {
      let actual = crc32fast::hash(&bytes);
      if actual != expected {
         return Err(format!(
            "CRC mismatch (got {actual:#x}, expected {expected:#x})"
         ));
      }
   }
   String::from_utf8(bytes).map_err(|err| format!("invalid UTF-8: {err}"))
}

/// Where plugins for a loader not installed yet wait, with that loader's
/// version in [`STAGED_FOR`].
const STAGED: &str = "plugins.next";
const STAGED_FOR: &str = "loader";

/// Downloads `update`'s plugins into [`STAGED`] for loader `version`, and
/// returns the ones that failed. A failed one stays as it is, and the next
/// check offers it again.
#[must_use]
pub fn stage_plugins(version: &str, update: &Update) -> Vec<String> {
   let Some(state) = crate::state() else {
      return vec!["runtime not started".to_owned()];
   };
   let dir = state.home.join(STAGED);
   let _ = fs::remove_dir_all(&dir);
   if let Err(err) = fs::create_dir_all(&dir) {
      log::error!("staging plugins: {}: {err}", dir.display());
      return vec![format!("create {}: {err}", dir.display())];
   }
   let mut failed = Vec::new();
   for name in &update.plugins {
      let staged =
         download_plugin(name, update.plugin_crcs.get(name).copied()).and_then(|source| {
            same_reach(name, &source)?;
            fs::write(dir.join(format!("{name}.js")), source).map_err(|err| err.to_string())
         });
      match staged {
         Ok(()) => log::info!("staged plugin {name} for {version}"),
         Err(err) => {
            log::error!("staging {name} failed: {err}");
            failed.push(format!("{name}: {err}"));
         },
      }
   }
   if let Err(err) = fs::write(dir.join(STAGED_FOR), version) {
      log::error!("staging plugins: {err}");
      failed.push(format!("write stage marker: {err}"));
   }
   failed
}

/// Moves the plugins staged for this loader, or an older one, into `plugins`.
/// Plugins staged for a newer loader, whose install was not finished, wait.
pub fn adopt_staged(home: &Path, plugins: &Path) {
   let dir = home.join(STAGED);
   let Ok(version) = fs::read_to_string(dir.join(STAGED_FOR)) else {
      return;
   };
   if version.trim() > VERSION {
      return;
   }
   if let Err(err) = fs::create_dir_all(plugins) {
      log::error!("staged plugins: {}: {err}", plugins.display());
      return;
   }
   for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
      let path = entry.path();
      if path.extension().is_some_and(|extension| extension == "js")
         && let Some(name) = path.file_name()
         && let Err(err) = fs::rename(&path, plugins.join(name))
      {
         log::error!("staged plugin {}: {err}", path.display());
      }
   }
   let _ = fs::remove_dir_all(&dir);
   log::info!("moved in the plugins staged for {}", version.trim());
}

#[cfg(test)]
mod tests {
   use super::*;

   fn update(loader_crcs: &[(&str, u32)]) -> Update {
      Update {
         loader:        None,
         loader_crcs:   loader_crcs
            .iter()
            .map(|&(name, crc)| (name.to_owned(), crc))
            .collect(),
         launcher_crcs: BTreeMap::new(),
         plugins:       Vec::new(),
         plugin_crcs:   BTreeMap::new(),
      }
   }

   #[test]
   fn reads_launcher_crcs() {
      let site = serde_json::from_str::<Site>(
         r#"{"loader": "1", "launcher_crcs": {"sunbird-linux.sh": 42}, "plugins": {}}"#,
      )
      .unwrap();
      assert_eq!(site.launcher_crcs.get("sunbird-linux.sh"), Some(&42));
      let old = serde_json::from_str::<Site>(r#"{"loader": "1", "plugins": {}}"#).unwrap();
      assert!(old.launcher_crcs.is_empty() && old.loader_crcs.is_empty());
   }

   #[test]
   fn verifies_assets() {
      let script = b"#!/bin/sh\necho hi\n";
      let crc = crc32fast::hash(script);
      assert!(verify_asset("a.sh", b"#!/bin/sh", crc, script).is_ok());
      assert!(verify_asset("a.sh", b"#!/bin/sh", crc ^ 1, script).is_err());
      assert!(verify_asset("a.sh", b"MZ", crc, script).is_err());
   }

   #[test]
   fn verifies_loaders() {
      let dll = b"MZ...phoenix-version:2026.1-abc;...";
      let crc = crc32fast::hash(dll);
      let listed = update(&[("phoenix-windows-x86_64.dll", crc)]);
      assert!(verify_loader("phoenix-windows-x86_64.dll", "2026.1-abc", dll, &listed).is_ok());
      assert!(verify_loader("phoenix-windows-x86_64.dll", "2026.2-def", dll, &listed).is_err());
      assert!(verify_loader("libphoenix-linux-x86_64.so", "2026.1-abc", dll, &listed).is_err());
      let wrong = update(&[("phoenix-windows-x86_64.dll", crc ^ 1)]);
      assert!(verify_loader("phoenix-windows-x86_64.dll", "2026.1-abc", dll, &wrong).is_err());
   }
}
