use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
};

use phoenix_sigs::{
   index::Index,
   locate::spotify::{
      self,
      CodePatch,
   },
};

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

static PATCHES: [OnceLock<CodePatch>; 2] = [OnceLock::new(), OnceLock::new()];
static HOLDERS_PER_TWEAK: Mutex<[usize; 2]> = Mutex::new([0; 2]);

pub fn install(ix: &Index<'_>) {
   let found = [
      (Tweak::NoAutoPause, spotify::auto_pause(ix)),
      (Tweak::IdleActivity, spotify::idle_activity(ix)),
   ];
   for (tweak, patch) in found {
      match patch {
         Ok(patch) => {
            let _ = PATCHES[tweak.index()].set(patch);
         },
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
      let bytes = if on {
         &patch.patched[..patch.len]
      } else {
         &patch.shipped[..patch.len]
      };
      // SAFETY: the locator patches one x86 opcode byte or one aligned arm64
      // instruction, so a thread at the site runs the old or the new
      // instruction whole.
      if let Err(err) = unsafe { state.image.write_code(patch.site, bytes) } {
         log::error!("spotify {tweak:?}: {err}");
         return false;
      }
   }
   holders[tweak.index()] = next;
   true
}
