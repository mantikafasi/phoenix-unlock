//! The channel switch `ForwardDialog` makes after forwarding to one
//! destination, skipped while a plugin holds `phoenix.ui.disableForwardFollow`.

use std::sync::{
   OnceLock,
   atomic::{
      AtomicBool,
      Ordering,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   index::Index,
   locate::forward,
};

/// `Workspace`'s switch, which takes four words and returns nothing.
type Switch = unsafe extern "C" fn(usize, usize, usize, usize);

static OFF: AtomicBool = AtomicBool::new(false);
static ORIGINAL: OnceLock<Switch> = OnceLock::new();

/// Hooks the switch from [`crate::prepare`], before Discord's threads run.
pub fn install(image: &Image<'static>, ix: &Index<'_>) {
   let switch = match forward::forward_follow(ix) {
      Ok(switch) => switch,
      Err(err) => {
         log::warn!("forward follow: {err}, forwarding still switches channels");
         return;
      },
   };
   // SAFETY: runs before Discord's threads, and `skip` has the switch's ABI.
   if let Some(original) =
      unsafe { crate::hook::<Switch>(image, switch, skip as *const () as usize, "forward follow") }
   {
      let _ = ORIGINAL.set(original);
      log::info!("forward follow: hooked @ {switch:#x}");
   }
}

/// Whether the switch is hooked, so `disableForwardFollow` can work.
pub fn ready() -> bool {
   ORIGINAL.get().is_some()
}

/// Skips the switch while `off`.
pub fn turn_off(off: bool) {
   OFF.store(off, Ordering::Release);
}

unsafe extern "C" fn skip(arg0: usize, arg1: usize, arg2: usize, arg3: usize) {
   if OFF.load(Ordering::Acquire) {
      return;
   }
   if let Some(original) = ORIGINAL.get() {
      // SAFETY: the trampoline runs the switch with the arguments Discord
      // passed.
      unsafe { original(arg0, arg1, arg2, arg3) };
   }
}
