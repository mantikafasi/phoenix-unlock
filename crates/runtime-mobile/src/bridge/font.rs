//! Final text-face projection for both guest UI and plugin-generated updates.
use std::sync::{
   LazyLock,
   Mutex,
   PoisonError,
};

use phoenix_ui_mobile::{
   decode,
   encode,
   system_font::SystemFont,
};

use super::{
   Action,
   UI_LOCK,
   send_ops,
};

static FONTS: LazyLock<Mutex<SystemFont>> = LazyLock::new(Mutex::default);

pub(super) fn project(input: &[u8], action: Action) -> Action {
   let bytes = match &action {
      Action::Pass => input,
      Action::Replace(bytes) => bytes,
      Action::Drop => return action,
   };
   if let Ok(mut ops) = decode(bytes)
      && FONTS
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .observe(&mut ops)
   {
      return Action::Replace(encode(&ops));
   }
   action
}

pub fn set_owner(plugin: &str, enabled: bool) {
   // Serialize the refresh against row deletion and recycling, including the
   // actual host write. Reprojecting this batch would lose the original face.
   let _guard = UI_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
   let ops = FONTS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .set_owner(plugin, enabled);
   if !ops.is_empty() {
      send_ops(&encode(&ops));
   }
}
