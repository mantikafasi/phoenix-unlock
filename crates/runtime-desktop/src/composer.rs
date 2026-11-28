//! The composer's Nitro gift button, which `phoenix.ui.hide("composer.gift")`
//! leaves out, and `phoenix.ui.addComposerButton`'s buttons, drawn ahead of
//! the chat bar's own through a detour on the bar's render.

use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

/// `jne rel32`, and `nop; jmp rel32` to the same place on x86-64.
const SHOWN: [u8; 2] = [0x0F, 0x85];
const HIDDEN: [u8; 2] = [0x90, 0xE9];

struct Patch {
   site:   usize,
   shown:  [u8; 4],
   hidden: [u8; 4],
   len:    usize,
}

static GIFT: OnceLock<Patch> = OnceLock::new();
/// Plugins holding the gift button hidden.
static HIDERS: Mutex<usize> = Mutex::new(0);

/// Finds the gift button's branch from [`crate::prepare`], while the locator
/// indexes are built.
pub fn install(image: &Image<'static>) {
   if crate::X86_CODE {
      match desktop::composer_gift(image) {
         Ok(site) if image.read(site, 2) == Some(&SHOWN[..]) => {
            let _ = GIFT.set(Patch {
               site,
               shown: [SHOWN[0], SHOWN[1], 0, 0],
               hidden: [HIDDEN[0], HIDDEN[1], 0, 0],
               len: 2,
            });
         },
         Ok(site) => log::warn!("composer gift: {site:#x} is no jne"),
         Err(err) => log::warn!("composer gift: {err}, the gift button stays"),
      }
   } else {
      match phoenix_sigs::macos::composer_gift(image) {
         Ok(gift) => {
            let _ = GIFT.set(Patch {
               site:   gift.site,
               shown:  gift.shown.to_le_bytes(),
               hidden: gift.hidden.to_le_bytes(),
               len:    4,
            });
         },
         Err(err) => log::warn!("composer gift: {err}, the gift button stays"),
      }
   }
}

/// Hides the gift button until the matching `hide_gift(false)`. False when
/// this build's button was not found.
pub fn hide_gift(hide: bool) -> bool {
   let (Some(patch), Some(state)) = (GIFT.get(), phoenix_runtime::state()) else {
      return false;
   };
   let mut hiders = HIDERS.lock().unwrap_or_else(PoisonError::into_inner);
   let next = if hide {
      *hiders + 1
   } else {
      hiders.saturating_sub(1)
   };
   if (*hiders > 0) != (next > 0) {
      let bytes = if hide {
         &patch.hidden[..patch.len]
      } else {
         &patch.shown[..patch.len]
      };
      // SAFETY: all patch bytes lie in one aligned word, which the locator
      // checked, so a thread at the branch reads the old or the new opcode
      // whole.
      if let Err(err) = unsafe { state.image.write_code(patch.site, bytes) } {
         log::error!("composer gift: {err}");
         return false;
      }
   }
   *hiders = next;
   true
}

pub mod buttons {
   use std::{
      ffi::c_void,
      panic::{
         self,
         AssertUnwindSafe,
      },
      ptr,
      sync::{
         Mutex,
         OnceLock,
         PoisonError,
         mpsc,
      },
   };

   use phoenix_hook::image::Image;
   use phoenix_sigs::{
      index::Index,
      locate::composer,
   };

   use crate::{
      accessories,
      gpui::{
         BtnSize,
         BtnVariant,
         RawAnyElement,
         RcFnVtbl,
         W_FULL,
         build_raw_button,
         gpui_px,
         h_row,
         push_any,
         push_div,
         push_raw_button,
         raw_div,
         style_div,
         write_any,
      },
      js,
   };

   /// A plugin button as the render draws it. `on` draws it as the primary
   /// button, which is how a tint shows.
   pub struct Button {
      pub id:    u64,
      pub label: String,
      pub on:    bool,
   }

   static BUTTONS: Mutex<Vec<Button>> = Mutex::new(Vec::new());

   type RenderFn = unsafe extern "C" fn(usize, usize, usize, usize, usize) -> RawAnyElement;

   static ORIG: OnceLock<RenderFn> = OnceLock::new();

   /// Adds or replaces the button with `button.id` and redraws.
   pub fn set(button: Button) {
      let mut buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
      match buttons.iter_mut().find(|known| known.id == button.id) {
         Some(known) => *known = button,
         None => buttons.push(button),
      }
      drop(buttons);
      accessories::invalidate_discord_windows();
   }

   pub fn remove(ids: &[u64]) {
      let mut buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
      let before = buttons.len();
      buttons.retain(|button| !ids.contains(&button.id));
      if buttons.len() != before {
         drop(buttons);
         accessories::invalidate_discord_windows();
      }
   }

   /// Hooks the chat bar's button row from [`crate::prepare`].
   pub fn install(image: &Image<'static>, ix: &Index<'_>) {
      let func = match composer::composer_buttons(ix) {
         Ok(func) => func,
         Err(err) => {
            log::warn!("composer buttons: {err}, plugin buttons stay off");
            return;
         },
      };
      // SAFETY: runs from `prepare` before Discord's threads, and `detour` has
      // the render's signature on every desktop, five arguments and an
      // `AnyElement` returned through the return slot.
      let Some(orig) = (unsafe {
         crate::hook::<RenderFn>(
            image,
            func,
            detour as *const () as usize,
            "composer buttons: hook failed",
         )
      }) else {
         return;
      };
      let _ = ORIG.set(orig);
      log::info!("composer buttons: hooked the button row @ {func:#x}");
   }

   unsafe extern "C" fn detour(
      a0: usize,
      a1: usize,
      a2: usize,
      a3: usize,
      a4: usize,
   ) -> RawAnyElement {
      let orig = *ORIG.get().expect("the detour only runs once ORIG is set");
      // SAFETY: forwards every argument to the render.
      let mut out = unsafe { orig(a0, a1, a2, a3, a4) };
      let _ = panic::catch_unwind(AssertUnwindSafe(|| decorate(&raw mut out)));
      out
   }

   /// Puts the plugin buttons in a row ahead of the element `sret` holds.
   fn decorate(sret: *mut RawAnyElement) {
      let buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
      if buttons.is_empty() {
         return;
      }
      let Some(mut row) = h_row(4.0, false) else {
         return;
      };
      for button in buttons.iter() {
         let variant = if button.on {
            BtnVariant::Primary
         } else {
            BtnVariant::Secondary
         };
         let id = format!("sunbird-composer-{}", button.id);
         if let Some(built) = build_raw_button(
            &id,
            &button.label,
            variant,
            BtnSize::Small,
            &raw const PRESS_VTBL,
            button.id,
         ) {
            row = push_raw_button(row, built);
         }
      }
      drop(buttons);
      let Some(mut bar) = raw_div() else {
         return;
      };
      // The bar fills its parent, so it shrinks to the width the buttons leave.
      style_div(&mut row, |style| style.length(style.at.size[0], W_FULL));
      style_div(&mut bar, |style| {
         style.length(style.at.size[0], W_FULL);
         style.length(style.at.min_size[0], gpui_px(0.0));
         style.float(style.at.flex_shrink, 1.0);
      });
      // SAFETY: moves the rendered element out of `sret`, which gets it back or
      // the row holding it.
      let mut orig = unsafe { ptr::read(sret) };
      if !push_any(&mut bar, &mut orig) {
         // SAFETY: puts back the element read above, which nothing took.
         unsafe { ptr::write(sret, orig) };
         return;
      }
      write_any(push_div(row, bar), sret);
   }

   static PRESS_VTBL: RcFnVtbl = RcFnVtbl {
      drop_in_place: ptr::null(),
      size:          size_of::<u64>(),
      align:         align_of::<u64>(),
      call_once:     press as *const c_void,
      call_mut:      press as *const c_void,
      call:          press as *const c_void,
      extra:         ptr::null(),
   };

   unsafe extern "C" fn press(
      id: *const u64,
      _event: *const c_void,
      _window: *mut c_void,
      _app: *mut c_void,
   ) {
      if id.is_null() {
         return;
      }
      // SAFETY: GPUI passes a pointer to the `u64` payload of the `Rc` that
      // `build_raw_button` allocated.
      let id = unsafe { *id };
      let _ = worker().send(id);
   }

   fn worker() -> &'static mpsc::Sender<u64> {
      static WORKER: OnceLock<mpsc::Sender<u64>> = OnceLock::new();
      WORKER.get_or_init(|| {
         let (tx, rx) = mpsc::channel::<u64>();
         let _ = std::thread::Builder::new()
            .name("sunbird-composer".to_owned())
            .spawn(move || {
               while let Ok(id) = rx.recv() {
                  if let Some(engine) = phoenix_runtime::engine() {
                     js::ui::press_composer_button(&engine, id);
                  }
               }
            });
         tx
      })
   }
}
