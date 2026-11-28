//! The first-launch modal: how the launcher starts Sunbird, saved to
//! `launcher.conf` however it is closed. A modal hook that installed but never
//! draws is a silent failure only this notices, so a modal that stays undrawn
//! hands over to [`crate::recovery`].

use std::{
   sync::{
      Arc,
      atomic::{
         AtomicBool,
         AtomicU64,
         Ordering,
      },
   },
   thread,
   time::{
      Duration,
      Instant,
   },
};

use crate::{
   config::{
      self,
      LauncherConfig,
   },
   modal::{
      self,
      ModalButton,
      ModalSection,
      PageModalSpec,
   },
};

/// How long Discord's window may take to draw its first frame after start.
const WINDOW_WAIT: Duration = Duration::from_secs(12);
/// How long an opened modal may take to reach a frame.
const RENDER_WAIT: Duration = Duration::from_secs(5);

static PAGE: AtomicU64 = AtomicU64::new(0);
static ANSWERED: AtomicBool = AtomicBool::new(false);

/// Shows the modal once Discord draws, on a thread of its own.
pub fn start() {
   let _ = thread::Builder::new()
      .name("sunbird-onboarding".to_owned())
      .spawn(run);
}

fn run() {
   // Without the overlay hook only the macOS panel fallback can draw it, and
   // nothing would ever mark the window seen.
   if !modal::hooked() && !cfg!(target_os = "macos") {
      return;
   }
   if modal::hooked() {
      let deadline = Instant::now() + WINDOW_WAIT;
      while !modal::window_seen() && Instant::now() < deadline {
         thread::sleep(Duration::from_millis(100));
      }
   }
   open();
   thread::sleep(RENDER_WAIT);
   if modal::rendered() || ANSWERED.load(Ordering::Acquire) {
      return;
   }
   modal::close_page(PAGE.load(Ordering::Acquire));
   crate::health::critical("modal", Err("hook installed but did not render".to_owned()));
   crate::recovery::start();
}

fn open() {
   let id = modal::open_page_closing(spec(), Some(Arc::new(finish)));
   PAGE.store(id, Ordering::Release);
}

fn spec() -> PageModalSpec {
   let config = config::load();
   let switch =
      |id: &str, label: &str, description: &str, checked, set: fn(&mut LauncherConfig, bool)| {
         ModalSection::Switch {
            id: id.to_owned(),
            label: label.to_owned(),
            description: Some(description.to_owned()),
            checked,
            on_toggle: Arc::new(move |on| toggle(|config| set(config, on))),
         }
      };
   PageModalSpec {
      title:    "Welcome to Sunbird".to_owned(),
      sections: vec![
         ModalSection::Text {
            title: None,
            text:  "Sunbird is running inside Discord. Choose how it starts; you can change this \
                    any time in User Settings -> Sunbird -> General."
               .to_owned(),
         },
         switch(
            "sunbird-onboard-auto-start",
            "Start Sunbird on login",
            "Open Discord with Sunbird when you sign in to your computer.",
            config.auto_start,
            |config, on| config.auto_start = on,
         ),
         switch(
            "sunbird-onboard-auto-update",
            "Auto-update Sunbird on launch",
            "Download the newest Sunbird loader each time the launcher starts Discord.",
            config.auto_update,
            |config, on| config.auto_update = on,
         ),
         ModalSection::Buttons {
            title:   None,
            buttons: vec![ModalButton {
               label:    "Save & Continue".to_owned(),
               danger:   false,
               primary:  true,
               on_press: Arc::new(|| {
                  modal::close_page(PAGE.load(Ordering::Acquire));
                  finish();
               }),
            }],
         },
      ],
   }
}

fn toggle(change: impl FnOnce(&mut LauncherConfig)) {
   config::update(change);
   if !modal::replace_page(PAGE.load(Ordering::Acquire), spec()) {
      // An `AppKit` panel can't be rebuilt in place, so it is reopened with
      // the new state; closing the old one saves it, which is harmless.
      modal::close_page(PAGE.load(Ordering::Acquire));
      open();
   }
}

/// Saves the toggles with `onboarded` set, so the modal never returns.
fn finish() {
   ANSWERED.store(true, Ordering::Release);
   config::save_or_report(config::update(|config| config.onboarded = true));
}
