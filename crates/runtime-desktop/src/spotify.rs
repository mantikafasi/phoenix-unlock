use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   Found,
   desktop,
   macos,
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

/// One x86 opcode byte or one arm64 instruction word.
struct Patch {
   site:    usize,
   shipped: [u8; 4],
   patched: [u8; 4],
   len:     usize,
}

impl Patch {
   fn byte(image: &Image<'_>, site: Found<usize>, patched: u8) -> Found<Self> {
      let site = site?;
      let shipped = image
         .read(site, 1)
         .ok_or_else(|| format!("{site:#x} is unmapped"))?[0];
      Ok(Self {
         site,
         shipped: [shipped, 0, 0, 0],
         patched: [patched, 0, 0, 0],
         len: 1,
      })
   }

   fn word(image: &Image<'_>, site: usize, patched: u32) -> Found<Self> {
      let shipped = image
         .read_u32(site)
         .filter(|_| site.is_multiple_of(4))
         .ok_or_else(|| format!("{site:#x} is no mapped instruction"))?;
      Ok(Self {
         site,
         shipped: shipped.to_le_bytes(),
         patched: patched.to_le_bytes(),
         len: 4,
      })
   }
}

static PATCHES: [OnceLock<Patch>; 2] = [OnceLock::new(), OnceLock::new()];
static HOLDERS_PER_TWEAK: Mutex<[usize; 2]> = Mutex::new([0; 2]);

pub fn install(image: &Image<'static>) {
   let found = if crate::X86_CODE {
      [
         (
            Tweak::NoAutoPause,
            Patch::byte(image, desktop::spotify_auto_pause(image), 0xC3),
         ),
         (
            Tweak::IdleActivity,
            Patch::byte(image, desktop::spotify_idle_activity(image), 0x39),
         ),
      ]
   } else {
      [
         (
            Tweak::NoAutoPause,
            macos::spotify_auto_pause(image).and_then(|site| Patch::word(image, site, macos::RET)),
         ),
         (
            Tweak::IdleActivity,
            macos::spotify_idle_activity(image)
               .and_then(|flag| Patch::word(image, flag.site, flag.patched)),
         ),
      ]
   };
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
      let bytes = if on { &patch.patched } else { &patch.shipped };
      // SAFETY: an x86 patch is one opcode byte whose two opcodes decode to the
      // same length, and an arm64 patch is one aligned instruction word, so a
      // thread at the site runs the old or the new instruction whole.
      if let Err(err) = unsafe { state.image.write_code(patch.site, &bytes[..patch.len]) } {
         log::error!("spotify {tweak:?}: {err}");
         return false;
      }
   }
   holders[tweak.index()] = next;
   true
}
