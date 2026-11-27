use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tweak {
   NoAutoPause,
   IdleActivity,
}

impl Tweak {
   const fn index(self) -> usize {
      match self {
         Self::NoAutoPause => 0,
         Self::IdleActivity => 1,
      }
   }
}

struct Patch {
   site:    usize,
   shipped: u8,
   patched: u8,
}

static PATCHES: [OnceLock<Patch>; 2] = [OnceLock::new(), OnceLock::new()];
static HOLDERS_PER_TWEAK: Mutex<[usize; 2]> = Mutex::new([0; 2]);

pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE {
      return;
   }
   let found = [
      (Tweak::NoAutoPause, desktop::spotify_auto_pause(image), 0xC3),
      (Tweak::IdleActivity, desktop::spotify_idle_activity(image), 0x39),
   ];
   for (tweak, site, patched) in found {
      match site.map(|site| (site, image.read(site, 1).map(|byte| byte[0]))) {
         Ok((site, Some(shipped))) => {
            let _ = PATCHES[tweak.index()].set(Patch {
               site,
               shipped,
               patched,
            });
         },
         Ok((site, None)) => log::warn!("spotify {tweak:?}: {site:#x} is unmapped"),
         Err(err) => log::warn!("spotify {tweak:?}: {err}, left as Discord ships it"),
      }
   }
}

pub fn ready(tweak: Tweak) -> bool {
   PATCHES[tweak.index()].get().is_some()
}

/// Reference-counted across plugins: the patch stays until the last holder
/// releases it. False when this build's site was not found or not writable.
pub fn hold(tweak: Tweak, on: bool) -> bool {
   let (Some(patch), Some(state)) = (PATCHES[tweak.index()].get(), phoenix_runtime::state())
   else {
      return false;
   };
   let mut holders = HOLDERS_PER_TWEAK.lock().unwrap_or_else(PoisonError::into_inner);
   let held = holders[tweak.index()];
   let next = if on {
      held + 1
   } else {
      held.saturating_sub(1)
   };
   if (held > 0) != (next > 0) {
      let byte = if on { patch.patched } else { patch.shipped };
      // SAFETY: the patch is one opcode byte and both opcodes decode to the
      // same length, so a thread at the site runs the old or the new
      // instruction whole.
      if let Err(err) = unsafe { state.image.write_code(patch.site, &[byte]) } {
         log::error!("spotify {tweak:?}: {err}");
         return false;
      }
   }
   holders[tweak.index()] = next;
   true
}
