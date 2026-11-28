//! Warns when Discord is not the build `web/discord.json` lists for this OS,
//! older or newer, since the locators were checked on that build.

use std::{
   cmp::Ordering,
   collections::HashMap,
};

use phoenix_runtime::update::ASSETS;

use crate::notices::{
   self,
   Notice,
};

/// Discord's warning yellow.
const WARNING: u32 = 0xFFF0_B232;

/// The key `web/discord.json` lists this OS's supported build under.
const OS: &str = if cfg!(windows) {
   "windows"
} else if cfg!(target_os = "linux") {
   "linux"
} else {
   "macos"
};

#[derive(serde::Deserialize)]
struct Builds {
   #[serde(default)]
   desktop: HashMap<String, String>,
}

/// Compares the running Discord with the supported build and shows a notice
/// when they differ. Blocks on the network, so it runs off Discord's threads.
pub fn check() {
   let Some(installed) = installed() else {
      log::info!("discord version: cannot read this Discord's version");
      return;
   };
   let supported = match supported() {
      Ok(Some(supported)) => supported,
      Ok(None) => return,
      Err(err) => {
         log::info!("discord version check: {err}");
         return;
      },
   };
   let (title, advice) = match compare(&installed, &supported) {
      Some(Ordering::Equal) => return,
      Some(Ordering::Less) => ("Discord is older than Sunbird supports", "Update Discord."),
      Some(Ordering::Greater) => {
         (
            "Discord is newer than Sunbird supports",
            "Some features may not work until Sunbird updates.",
         )
      },
      None => {
         (
            "Discord is not a build Sunbird supports",
            "Some features may not work.",
         )
      },
   };
   log::warn!("discord version: running {installed}, supported {supported}");
   let body = format!("Running {installed}, Sunbird supports {supported}. {advice}");
   // Stays until closed: a five second banner at startup goes unread.
   let notice = Notice {
      source:    "Sunbird".to_owned(),
      title:     title.to_owned(),
      body:      Some(body.clone()),
      accent:    Some(WARNING),
      duration:  None,
      pressable: false,
   };
   if notices::show(notice).is_none() {
      phoenix_ui_desktop::notify(title, Some(&body));
   }
}

fn supported() -> Result<Option<String>, String> {
   let body = phoenix_runtime::fetch::get(&format!("{ASSETS}discord.json"))
      .map_err(|err| err.to_string())?;
   let mut builds =
      serde_json::from_slice::<Builds>(&body).map_err(|err| format!("discord.json: {err}"))?;
   Ok(builds.desktop.remove(OS))
}

/// Orders dotted versions part by part, `None` when either is not one.
fn compare(installed: &str, supported: &str) -> Option<Ordering> {
   let parts = |version: &str| {
      version
         .split('.')
         .map(|part| part.parse::<u32>().ok())
         .collect::<Option<Vec<_>>>()
   };
   Some(parts(installed)?.cmp(&parts(supported)?))
}

/// The `AppImage`'s version, from the desktop entry its runtime mounts at
/// `APPDIR`.
#[cfg(target_os = "linux")]
fn installed() -> Option<String> {
   let dir = std::env::var_os("APPDIR")?;
   std::fs::read_dir(dir)
      .ok()?
      .flatten()
      .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "desktop"))
      .find_map(|entry| {
         std::fs::read_to_string(entry.path())
            .ok()?
            .lines()
            .find_map(|line| {
               line
                  .strip_prefix("X-AppImage-Version=")
                  .map(|version| version.trim().to_owned())
            })
      })
}

/// The exe's file version, from its version resource.
#[cfg(windows)]
fn installed() -> Option<String> {
   use std::{
      ffi::c_void,
      os::windows::ffi::OsStrExt,
      ptr,
   };

   #[link(name = "version")]
   unsafe extern "system" {
      fn GetFileVersionInfoSizeW(name: *const u16, handle: *mut u32) -> u32;
      fn GetFileVersionInfoW(name: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
      fn VerQueryValueW(
         block: *const c_void,
         sub_block: *const u16,
         buffer: *mut *mut c_void,
         len: *mut u32,
      ) -> i32;
   }

   let exe = std::env::current_exe().ok()?;
   let name = exe
      .as_os_str()
      .encode_wide()
      .chain([0])
      .collect::<Vec<u16>>();
   let root = [u16::from(b'\\'), 0];
   // SAFETY: `name` is a NUL-terminated path, and a null handle is allowed.
   let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), ptr::null_mut()) };
   if size == 0 {
      return None;
   }
   let mut data = vec![0u8; size as usize];
   // SAFETY: `data` holds the `size` bytes the call may write.
   if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
      return None;
   }
   let mut fixed = ptr::null_mut::<c_void>();
   let mut len = 0u32;
   // SAFETY: `data` is the block filled above, `root` a NUL-terminated `\`,
   // and the out pointers are live locals.
   if unsafe {
      VerQueryValueW(
         data.as_ptr().cast(),
         root.as_ptr(),
         &raw mut fixed,
         &raw mut len,
      )
   } == 0
      || fixed.is_null()
      || len < 16
   {
      return None;
   }
   // SAFETY: `VS_FIXEDFILEINFO` starts with signature, struct version, then
   // the file version's high and low words, all inside the `len` bytes that
   // `VerQueryValueW` pointed into `data`.
   let [signature, _, high, low] =
      std::array::from_fn(|at| unsafe { fixed.cast::<u32>().add(at).read_unaligned() });
   (signature == 0xFEEF_04BD).then(|| {
      format!(
         "{}.{}.{}.{}",
         high >> 16,
         high & 0xFFFF,
         low >> 16,
         low & 0xFFFF
      )
   })
}

/// macOS lists no supported build yet.
#[cfg(target_os = "macos")]
const fn installed() -> Option<String> {
   None
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn orders_versions() {
      assert_eq!(compare("0.1.0.2229", "0.1.0.2334"), Some(Ordering::Less));
      assert_eq!(compare("0.1.0.2334", "0.1.0.2334"), Some(Ordering::Equal));
      assert_eq!(compare("0.1.1.0", "0.1.0.2334"), Some(Ordering::Greater));
      assert_eq!(compare("0.1.0-dev", "0.1.0.2334"), None);
   }
}
