//! Tells the user when the site ships a newer Phoenix Unlock: a newer loader,
//! or plugin bundles that differ from the installed ones. The site publishes
//! both in `version.json` (`web/tools/bundle-plugins.mjs`).

use std::{
   collections::BTreeMap,
   sync::OnceLock,
   thread,
};

/// This build's version, `<UTC commit date>-<short hash>` (`build.rs`).
pub const VERSION: &str = env!("PHOENIX_VERSION");
pub const SITE: &str = "https://mantikafasi.github.io/phoenix-unlock/";

/// Where `bundle-plugins.mjs` reads the version from the loader the site ships.
#[used]
static MARKER: &str = concat!("phoenix-version:", env!("PHOENIX_VERSION"), ";");

#[derive(serde::Deserialize)]
struct Site {
   loader:  String,
   plugins: BTreeMap<String, u32>,
}

/// What the site has that this install lacks.
pub struct Update {
   /// The site's version, when it is newer than [`VERSION`].
   pub loader:  Option<String>,
   /// Installed plugins whose code differs from the site's.
   pub plugins: Vec<String>,
}

static FOUND: OnceLock<Update> = OnceLock::new();

/// The update the check at start found, if any.
pub fn found() -> Option<&'static Update> {
   FOUND.get()
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
      let update = Update {
         loader:  (site.loader.as_str() > VERSION).then_some(site.loader),
         plugins: crate::plugins()
            .into_iter()
            .filter(|plugin| {
               site.plugins.get(&plugin.name).is_some_and(|&crc| {
                  crate::source(&plugin.name)
                     .is_some_and(|source| crc32fast::hash(source.as_bytes()) != crc)
               })
            })
            .map(|plugin| plugin.name)
            .collect(),
      };
      if update.loader.is_none() && update.plugins.is_empty() {
         log::info!("up to date ({VERSION})");
         return;
      }
      log::info!(
         "update available: loader {:?}, plugins {:?}",
         update.loader,
         update.plugins
      );
      let body = match (&update.loader, update.plugins.is_empty()) {
         (Some(version), true) => format!("Build {version} — tap to download"),
         (Some(version), false) => {
            format!(
               "Build {version} & {} plugin update{} — tap to open",
               update.plugins.len(),
               if update.plugins.len() == 1 { "" } else { "s" }
            )
         },
         (None, false) => format!("Updated plugins: {} — tap to open", update.plugins.join(", ")),
         (None, true) => String::new(),
      };
      if FOUND.set(update).is_ok() {
         crate::settings::refresh();
         crate::patches::show_banner(
            "Phoenix Unlock update available".to_owned(),
            Some(body),
            Some("res://ic_mana_puzzle_piece_retry".to_owned()),
            Some(0xFF23_A55A),
            8000,
            crate::patches::BannerAction::OpenUrl(SITE.to_owned()),
         );
      }
   });
}
