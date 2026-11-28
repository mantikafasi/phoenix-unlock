//! Updates Discord itself to a build CI checked against the release the site
//! serves (`.github/workflows/discord.yml`), which lists it in
//! `web/discord.json` with each split's SHA-256.
//!
//! The splits this phone needs come from the tracker the file names, each
//! checked against its hash, and merge with the site's loader and the
//! installed APK's plugins into one APK, signed like the site's, under the
//! installed package and label, so it installs over this one and keeps the
//! login.

use std::{
   collections::{
      BTreeMap,
      HashSet,
   },
   ffi::CStr,
   fmt::Write as _,
   sync::{
      Mutex,
      MutexGuard,
      PoisonError,
   },
   thread,
   time::Instant,
};

use patcher::{
   LOADER,
   Library,
   PackageName,
   Payload,
   Runtime,
   manifest::Info,
   zip,
};
use phoenix_runtime::fetch;
use phoenix_sigs::PLUGIN_BUNDLE;
use sha2::{
   Digest,
   Sha256,
};

/// `web/discord.json` on main, where CI publishes the builds that passed, so a
/// ready build needs no site release. A Cloudflare worker serves it, sparing
/// phones raw.githubusercontent.com's rate limits.
const BUILDS: &str = "https://update.sunbird.tools/discord.json";
/// Under [`fetch::LIMIT`], which caps one response.
const CHUNK: usize = 4 << 20;
/// Mobile networks stall for longer than the read timeout mid-split, and a
/// range costs little to fetch again.
const RETRIES: u32 = 3;
const RETRY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// This phone's ABI split and the directory its libraries sit in.
const ABI: (&str, &str) = if cfg!(target_arch = "x86_64") {
   ("config.x86_64", "x86_64")
} else if cfg!(target_arch = "arm") {
   ("config.armeabi_v7a", "armeabi-v7a")
} else {
   ("config.arm64_v8a", "arm64-v8a")
};
/// Android's density buckets, by split name.
const DENSITIES: [(&str, u32); 7] = [
   ("ldpi", 120),
   ("mdpi", 160),
   ("tvdpi", 213),
   ("hdpi", 240),
   ("xhdpi", 320),
   ("xxhdpi", 480),
   ("xxxhdpi", 640),
];

/// `web/discord.json`: the Android build CI checked, if any, beside the
/// desktop builds kept by hand.
#[derive(serde::Deserialize)]
struct Builds {
   android: Option<Build>,
}

/// The Android build `web/discord.json` lists.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Build {
   pub version_code: u32,
   pub version_name: String,
   url:              String,
   splits:           BTreeMap<String, Split>,
}

#[derive(Clone, serde::Deserialize)]
struct Split {
   sha256: String,
   size:   usize,
}

/// A running update: its steps in order, the one under way, and the bytes
/// downloaded of all its splits.
#[derive(Clone)]
pub struct Progress {
   pub steps:   Vec<Step>,
   pub at:      usize,
   pub done:    usize,
   pub total:   usize,
   /// When the downloads began, for their speed.
   pub started: Option<Instant>,
}

#[derive(Clone)]
pub struct Step {
   /// What it does, like "Download native libraries".
   pub label: String,
   /// The bytes a download fetches.
   pub size:  Option<usize>,
}

impl Progress {
   /// The bytes step `index` has fetched.
   #[must_use]
   pub fn fetched(&self, index: usize) -> usize {
      let before = self.steps[..index.min(self.steps.len())]
         .iter()
         .filter_map(|step| step.size)
         .sum::<usize>();
      let size = self
         .steps
         .get(index)
         .and_then(|step| step.size)
         .unwrap_or(0);
      self.done.saturating_sub(before).min(size)
   }

   /// How much of the download is done, from 0 to 1.
   #[must_use]
   pub fn fraction(&self) -> f64 {
      #[expect(
         clippy::cast_precision_loss,
         reason = "a bar's width needs no byte precision"
      )]
      let fraction = if self.total == 0 {
         0.0
      } else {
         self.done as f64 / self.total as f64
      };
      fraction.clamp(0.0, 1.0)
   }

   #[must_use]
   pub fn percent(&self) -> u32 {
      #[expect(
         clippy::cast_possible_truncation,
         clippy::cast_sign_loss,
         reason = "a percentage between 0 and 100"
      )]
      let percent = (self.fraction() * 100.0).round() as u32;
      percent
   }

   /// Bytes a second so far, and the seconds left at that pace.
   #[must_use]
   pub fn pace(&self) -> Option<(f64, u64)> {
      let seconds = self.started?.elapsed().as_secs_f64();
      if seconds < 1.0 || self.done == 0 || self.done >= self.total {
         return None;
      }
      #[expect(
         clippy::cast_precision_loss,
         reason = "a download's speed needs no byte precision"
      )]
      let speed = self.done as f64 / seconds;
      #[expect(
         clippy::cast_precision_loss,
         clippy::cast_possible_truncation,
         clippy::cast_sign_loss,
         reason = "whole seconds left, never negative"
      )]
      let left = ((self.total - self.done) as f64 / speed).ceil() as u64;
      Some((speed, left))
   }
}

#[derive(Clone)]
pub enum State {
   /// Unchecked, or no newer build.
   Current,
   Available(Build),
   Running(Build, Progress),
   Failed {
      build:    Build,
      error:    String,
      /// How far it got, which the page marks the failed step in.
      progress: Option<Progress>,
   },
   /// Android's installer has it.
   Handed(Build),
}

static STATE: Mutex<State> = Mutex::new(State::Current);
/// The installed version's name, from the check.
static INSTALLED: Mutex<String> = Mutex::new(String::new());
/// Set by the banner, so the update page opens once Settings shows.
static OPEN_PAGE: Mutex<Option<Instant>> = Mutex::new(None);

fn state_mut() -> MutexGuard<'static, State> {
   STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set(state: State) {
   *state_mut() = state;
   phoenix_runtime::refresh_settings();
}

#[must_use]
pub fn state() -> State {
   state_mut().clone()
}

/// The installed Discord's version name, once checked.
#[must_use]
pub fn installed_name() -> String {
   INSTALLED
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// The bytes this phone downloads for `build`.
#[must_use]
pub fn download_size(build: &Build) -> usize {
   wanted(build).map_or(0, |names| {
      names.iter().map(|name| build.splits[name].size).sum()
   })
}

/// Whether Settings should open the update page, once.
pub fn take_open_page() -> bool {
   /// Settings shows well within this after the banner's tap.
   const FRESH: std::time::Duration = std::time::Duration::from_secs(15);
   OPEN_PAGE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
      .is_some_and(|tapped| tapped.elapsed() < FRESH)
}

/// Opens Settings, which then shows the update page.
pub fn open_page() {
   *OPEN_PAGE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
   if let Err(err) = crate::install::open_in_app("discord://app/settings") {
      *OPEN_PAGE.lock().unwrap_or_else(PoisonError::into_inner) = None;
      log::warn!("discord update: opening Settings: {err}");
   }
}

/// Looks for a newer checked build, and offers it once no other banner shows.
pub fn check() {
   match newer() {
      Ok(Some(build)) => {
         log::info!("discord update: {} is ready", build.version_code);
         let name = build.version_name.clone();
         set(State::Available(build));
         // A banner replaces the one showing, so this waits its turn.
         for _ in 0..60 {
            if !crate::patches::banners::busy() {
               break;
            }
            thread::sleep(std::time::Duration::from_millis(500));
         }
         crate::patches::banners::show_banner(
            "Discord update available".to_owned(),
            Some(format!("{name}, checked for Sunbird. Tap to update")),
            Some("res://ic_mana_puzzle_piece_retry".to_owned()),
            Some(crate::ACCENT),
            8000,
            crate::patches::banners::BannerAction::OpenDiscordUpdate,
         );
      },
      Ok(None) => {},
      Err(err) => log::info!("discord update check: {err}"),
   }
}

fn newer() -> Result<Option<Build>, String> {
   /// Discord's largest split is a few hundred MB.
   const LARGEST: usize = 1 << 30;
   // The site ships no x86_64 loader, so such a phone could not finish.
   if cfg!(target_arch = "x86_64") {
      return Ok(None);
   }
   let body = fetch::get(BUILDS).map_err(|err| err.to_string())?;
   let builds =
      serde_json::from_slice::<Builds>(&body).map_err(|err| format!("discord.json: {err}"))?;
   let Some(build) = builds.android else {
      return Ok(None);
   };
   if !build.url.starts_with("https://") {
      return Err(format!("discord.json: {} is not https", build.url));
   }
   if let Some((name, _)) = build.splits.iter().find(|(_, split)| split.size > LARGEST) {
      return Err(format!("discord.json: {name} is too large"));
   }
   let installed = installed()?;
   *INSTALLED.lock().unwrap_or_else(PoisonError::into_inner) = installed.version_name;
   Ok((build.version_code > installed.version_code).then_some(build))
}

/// What the running APK's manifest says.
fn installed() -> Result<Info, String> {
   let base = crate::host::base_apk().map_err(|err| err.to_string())?;
   let map = crate::host::Map::open(&base).map_err(|err| format!("{base}: {err}"))?;
   patcher::manifest_info(map.bytes()).map_err(|err| err.to_string())
}

/// Starts the update the page offers, unless one runs. One already handed to
/// the installer opens it again.
pub fn start() {
   let build = {
      let mut state = state_mut();
      let build = match &*state {
         State::Handed(build) => {
            if let Ok(path) = crate::loader::update_path(crate::loader::DISCORD_UPDATE_APK)
               && path.exists()
            {
               let build = build.clone();
               drop(state);
               if let Err(err) = crate::install::open(&path) {
                  set(State::Failed {
                     build,
                     error: err,
                     progress: None,
                  });
               }
               return;
            }
            build.clone()
         },
         State::Available(build) | State::Failed { build, .. } => build.clone(),
         State::Current | State::Running(..) => return,
      };
      *state = State::Running(build.clone(), Progress {
         steps:   vec![Step {
            label: "Get ready".to_owned(),
            size:  None,
         }],
         at:      0,
         done:    0,
         total:   0,
         started: None,
      });
      build
   };
   phoenix_runtime::refresh_settings();
   thread::spawn(move || {
      // A retry takes a build CI listed since, if any.
      let build = newer().ok().flatten().unwrap_or(build);
      match update(&build) {
         Ok(()) => set(State::Handed(build)),
         Err(error) => {
            log::error!("discord update: {error}");
            let progress = match state() {
               State::Running(_, progress) => Some(progress),
               _ => None,
            };
            set(State::Failed {
               build,
               error,
               progress,
            });
         },
      }
   });
}

/// What a split holds, for the step list.
fn label(name: &str) -> String {
   match name.strip_prefix("config.") {
      None => "Discord".to_owned(),
      Some(config) if config == ABI.0.trim_start_matches("config.") => {
         "Native libraries".to_owned()
      },
      Some(config) if DENSITIES.iter().any(|(density, _)| *density == config) => {
         "Images for your screen".to_owned()
      },
      Some(language) => format!("Language pack ({language})"),
   }
}

fn update(build: &Build) -> Result<(), String> {
   let installed = installed()?;
   let names = wanted(build)?;
   let mut progress = Progress {
      steps:   names
         .iter()
         .map(|name| {
            Step {
               label: if name == "base" {
                  "Download Discord".to_owned()
               } else {
                  format!("Download {}", label(name).to_lowercase())
               },
               size:  Some(build.splits[name].size),
            }
         })
         .chain(
            [
               "Download the Sunbird loader",
               "Patch and sign",
               "Open the installer",
            ]
            .map(|label| {
               Step {
                  label: label.to_owned(),
                  size:  None,
               }
            }),
         )
         .collect(),
      at:      0,
      done:    0,
      total:   names.iter().map(|name| build.splits[name].size).sum(),
      started: Some(Instant::now()),
   };
   let mut apks = Vec::with_capacity(names.len());
   for (at, name) in names.iter().enumerate() {
      progress.at = at;
      set(State::Running(build.clone(), progress.clone()));
      apks.push(download(build, name, &mut progress)?);
   }

   progress.at = names.len();
   set(State::Running(build.clone(), progress.clone()));
   let loader = crate::loader::download()?;
   // Installed plugins live in the app's files and outlast the update, so only
   // the bundled ones ride along.
   let plugins = phoenix_runtime::plugins()
      .into_iter()
      .filter_map(|plugin| {
         phoenix_runtime::bundled_source(&plugin.name).map(|source| (plugin.name, source))
      })
      .collect();
   // `wanted` puts the ABI split second.
   let mut libraries = carried(&apks[1])?;
   libraries.push(loader);
   let prepared = Payload {
      runtime: Some(Runtime { libraries, plugins }),
      frida:   None,
   }
   .prepare()
   .map_err(|err| err.to_string())?;
   let package = installed
      .package
      .parse::<PackageName>()
      .map_err(|err| err.to_string())?;

   progress.at += 1;
   set(State::Running(build.clone(), progress.clone()));
   let apk = patcher::merge(
      &mut apks,
      &prepared,
      &package,
      installed.label.as_deref(),
      &mut |line| log::info!("discord update: {line}"),
   )
   .map_err(|err| err.to_string())?;
   let path = crate::loader::update_path(crate::loader::DISCORD_UPDATE_APK)?;
   crate::loader::write_apk(&apk, &path)?;
   drop(apk);
   drop(apks);

   progress.at += 1;
   set(State::Running(build.clone(), progress));
   crate::install::open(&path)
}

/// The libraries the installed APK carries beside the loader that Discord's
/// own ABI split lacks, like `ShadowHook`'s probe, so the update keeps them.
/// The loader and the plugin bundle are made anew.
fn carried(abi: &[u8]) -> Result<Vec<Library>, String> {
   let base = crate::host::base_apk().map_err(|err| err.to_string())?;
   // An install from separate splits keeps its libraries in the ABI split
   // beside the base.
   let split = std::path::Path::new(&base)
      .with_file_name(format!("split_{}.apk", ABI.0))
      .to_str()
      .map(str::to_owned)
      .filter(|split| std::path::Path::new(split).exists());
   let dir = format!("lib/{}/", ABI.1);
   let shipped = zip::read(abi)
      .map_err(|err| err.to_string())?
      .into_iter()
      .map(|record| record.name)
      .collect::<HashSet<String>>();
   let mut libraries = Vec::new();
   for path in std::iter::once(base).chain(split) {
      let map = crate::host::Map::open(&path).map_err(|err| format!("{path}: {err}"))?;
      let installed = map.bytes();
      for record in zip::read(installed).map_err(|err| err.to_string())? {
         let Some(name) = record.name.strip_prefix(&dir) else {
            continue;
         };
         if name == LOADER || name == PLUGIN_BUNDLE || shipped.contains(&record.name) {
            continue;
         }
         let data = record
            .entry(installed)
            .data()
            .map_err(|err| err.to_string())?
            .into_owned();
         libraries.push(Library::new(name, data).map_err(|err| format!("{name}: {err}"))?);
      }
   }
   Ok(libraries)
}

/// The base, this phone's ABI and nearest density, and its language plus
/// English, as far as the build has them.
fn wanted(build: &Build) -> Result<Vec<String>, String> {
   let mut names = Vec::new();
   for need in ["base", ABI.0] {
      if !build.splits.contains_key(need) {
         return Err(format!(
            "Discord {} has no {need} split",
            build.version_code
         ));
      }
      names.push(need.to_owned());
   }
   let dpi = property(c"ro.sf.lcd_density")
      .and_then(|value| value.parse::<u32>().ok())
      .unwrap_or(480);
   if let Some((density, _)) = DENSITIES
      .iter()
      .filter(|(name, _)| build.splits.contains_key(&format!("config.{name}")))
      .min_by_key(|(_, bucket)| (bucket.abs_diff(dpi), u32::MAX - bucket))
   {
      names.push(format!("config.{density}"));
   }
   let locale = property(c"persist.sys.locale").or_else(|| property(c"ro.product.locale"));
   let language = locale
      .as_deref()
      .and_then(|locale| locale.split(['-', '_']).next())
      .unwrap_or("en");
   // Android names some language splits by their legacy codes.
   let legacy = match language {
      "he" => "iw",
      "id" => "in",
      "yi" => "ji",
      other => other,
   };
   for language in [language, legacy, "en"] {
      let name = format!("config.{language}");
      if build.splits.contains_key(&name) && !names.contains(&name) {
         names.push(name);
      }
   }
   Ok(names)
}

/// A system property, or `None` when unset.
fn property(name: &CStr) -> Option<String> {
   let mut value = [0u8; libc::PROP_VALUE_MAX as usize];
   // SAFETY: `value` holds PROP_VALUE_MAX bytes, the most the call writes,
   // NUL included.
   let len = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };
   if len <= 0 {
      return None;
   }
   CStr::from_bytes_until_nul(&value)
      .ok()?
      .to_str()
      .ok()
      .map(str::to_owned)
}

/// Downloads split `name` in ranges, each within one response's limit, and
/// checks it against the build's hash.
fn download(build: &Build, name: &str, progress: &mut Progress) -> Result<Vec<u8>, String> {
   let split = &build.splits[name];
   let url = format!("{}{name}", build.url);
   let mut bytes = Vec::with_capacity(split.size);
   let mut hash = Sha256::new();
   while bytes.len() < split.size {
      let end = (bytes.len() + CHUNK).min(split.size) - 1;
      let range = format!("bytes={}-{end}", bytes.len());
      let mut tries = 1..=RETRIES;
      let chunk = loop {
         match fetch::get_with(&url, vec![("Range".to_owned(), range.clone())]) {
            Err(fetch::FetchError::Platform { message, .. }) if tries.next().is_some() => {
               log::warn!(
                  "discord update: {name} at {}: {message}, retrying",
                  bytes.len()
               );
               thread::sleep(RETRY_WAIT);
            },
            result => break result,
         }
      }
      .map_err(|err| {
         match err {
            fetch::FetchError::Status { status, .. } => {
               format!(
                  "{} did not download: Discord's server answered {status}",
                  label(name)
               )
            },
            other => format!("{} did not download: {other}", label(name)),
         }
      })?;
      if chunk.is_empty() || bytes.len() + chunk.len() > split.size {
         return Err(format!("{name}: the server sent {} bytes", chunk.len()));
      }
      hash.update(&chunk);
      bytes.extend_from_slice(&chunk);
      progress.done += chunk.len();
      set(State::Running(build.clone(), progress.clone()));
   }
   let found = hash
      .finalize()
      .iter()
      .fold(String::with_capacity(64), |mut hex, byte| {
         let _ = write!(hex, "{byte:02x}");
         hex
      });
   if found != split.sha256 {
      return Err(format!(
         "{} differs from the build Sunbird checked",
         label(name)
      ));
   }
   Ok(bytes)
}
