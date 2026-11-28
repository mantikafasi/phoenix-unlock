use std::{
   ffi::c_void,
   ptr,
   sync::{
      OnceLock,
      PoisonError,
      RwLock,
      mpsc,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   index::Index,
   locate::surfaces::{
      Surface,
      button_id_field,
      composer_buttons,
      div_ids,
   },
};

use crate::gpui::{
   self,
   RawDiv,
   RcFnVtbl,
};

type DivId = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *const u8, usize) -> *mut RawDiv;
type ComposerChild = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, u8, *mut RawDiv) -> *mut RawDiv;
static DIV_ID: OnceLock<usize> = OnceLock::new();
static OWNED_ID: OnceLock<usize> = OnceLock::new();
static ORIGINAL: OnceLock<DivId> = OnceLock::new();
static COMPOSER: OnceLock<ComposerChild> = OnceLock::new();
static SLOTS: OnceLock<Vec<(Surface, usize, usize)>> = OnceLock::new();
static BUTTONS: RwLock<Vec<Button>> = RwLock::new(Vec::new());
static WORKER: OnceLock<mpsc::SyncSender<[u64; 2]>> = OnceLock::new();

static CLICK: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<[u64; 2]>(),
   align:         align_of::<u64>(),
   call_once:     pressed as *const c_void,
   call_mut:      pressed as *const c_void,
   call:          pressed as *const c_void,
   extra:         ptr::null(),
};

#[derive(Clone)]
pub struct Button {
   pub id:       u64,
   pub surface:  Surface,
   pub label:    String,
   pub priority: i32,
}

pub fn set_div_id(address: usize) {
   let _ = DIV_ID.set(address);
}

pub fn install(image: &Image<'static>, ix: &Index<'_>) {
   let (Some(address), Some(layout)) = (gpui::button_new_addr(), gpui::layout()) else {
      return;
   };
   let field = match button_id_field(ix, address, layout.button.size) {
      Ok(field) => field,
      Err(err) => {
         log::warn!("plugin ui button id: {err}");
         return;
      },
   };
   let _ = OWNED_ID.set(field);
   let Some(&target) = DIV_ID.get().filter(|_| gpui::supports_push_any()) else {
      return;
   };
   match composer_buttons(ix, target, layout.div.size) {
      Ok((site, _)) => {
         // SAFETY: the composer locator verifies the native optional Div child
         // call. Startup installs the detour before it can be called.
         match unsafe { image.hook_call(site, &[], with_composer as *const () as usize) } {
            Ok(original) => {
               // SAFETY: this is the optional Div child builder found above.
               let _ =
                  COMPOSER.set(unsafe { std::mem::transmute::<usize, ComposerChild>(original) });
            },
            Err(err) => log::warn!("plugin ui composer: {err}"),
         }
      },
      Err(err) => log::warn!("plugin ui composer: {err}"),
   }
   let slots = Surface::DIVS
      .into_iter()
      .flat_map(|surface| {
         div_ids(ix, target, surface)
            .inspect_err(|err| log::warn!("plugin ui {}: {err}", surface.name()))
            .unwrap_or_default()
            .into_iter()
            .map(move |addr| (surface, addr, surface.native_id().map_or(0, str::len)))
      })
      .collect::<Vec<_>>();
   if slots.is_empty() {
      return;
   }
   // SAFETY: modal discovery checked the native Div::id ABI and layout before
   // startup.
   let Some(original) = (unsafe {
      crate::hook_sret::<DivId>(
         image,
         target,
         with_id as *const () as usize,
         "plugin ui surfaces",
      )
   }) else {
      return;
   };
   let _ = ORIGINAL.set(original);
   let _ = SLOTS.set(slots);
}

pub fn ready(surface: Surface) -> bool {
   if surface == Surface::Composer && COMPOSER.get().is_some() {
      return true;
   }
   if surface == Surface::MessageToolbar
      && OWNED_ID.get().is_some()
      && crate::accessories::message_rows_hooked()
   {
      return true;
   }
   SLOTS
      .get()
      .is_some_and(|slots| slots.iter().any(|&(kind, ..)| kind == surface))
}

#[cfg(test)]
pub fn test_slot() {
   let _ = SLOTS.set(vec![
      (Surface::Composer, 0, 0),
      (Surface::MessageToolbar, 0, 0),
   ]);
}

pub fn set(button: Button) {
   let mut buttons = BUTTONS.write().unwrap_or_else(PoisonError::into_inner);
   if let Some(existing) = buttons.iter_mut().find(|existing| existing.id == button.id) {
      *existing = button;
   } else {
      buttons.push(button);
   }
   buttons.sort_by_key(|button| (button.priority, button.id));
   drop(buttons);
   crate::modal::force_redraw();
}

pub fn get(id: u64) -> Option<Button> {
   BUTTONS
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .find(|button| button.id == id)
      .cloned()
}

pub fn remove(id: u64) {
   BUTTONS
      .write()
      .unwrap_or_else(PoisonError::into_inner)
      .retain(|button| button.id != id);
   crate::modal::force_redraw();
}

unsafe extern "C" fn with_id(
   out: *mut RawDiv,
   div: *mut RawDiv,
   id: *const u8,
   len: usize,
) -> *mut RawDiv {
   let original = *ORIGINAL.wait();
   // SAFETY: the detour forwards the original Div::id arguments.
   let result = unsafe { original(out, div, id, len) };
   let surface = SLOTS.get().and_then(|slots| {
      slots.iter().find_map(|&(surface, address, size)| {
         (id.addr() == address && len == size).then_some(surface)
      })
   });
   if let Some(surface) = surface {
      // SAFETY: result is the initialized native Div returned by Div::id.
      unsafe { append_buttons(surface, result) };
   }
   result
}

unsafe extern "C" fn with_composer(
   out: *mut RawDiv,
   parent: *mut RawDiv,
   show_emoji: u8,
   emoji: *mut RawDiv,
) -> *mut RawDiv {
   // SAFETY: the locator verifies parent is the live Div passed to the builder.
   unsafe { append_buttons(Surface::Composer, parent) };
   // SAFETY: forwards the builder's original inputs and output storage.
   unsafe { (COMPOSER.wait())(out, parent, show_emoji, emoji) }
}

unsafe fn append_buttons(surface: Surface, parent: *mut RawDiv) {
   if parent.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      if let Some(row) = row(surface, 0) {
         // SAFETY: caller provides the initialized Div we may append to.
         unsafe { gpui::append_div(parent.cast(), row) };
      }
   });
}

pub fn row(surface: Surface, message: u64) -> Option<RawDiv> {
   let owned_id = *OWNED_ID.get()?;
   let buttons = BUTTONS
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .filter(|button| button.surface == surface)
      .cloned()
      .collect::<Vec<_>>();
   if buttons.is_empty() {
      return None;
   }
   let mut row = gpui::h_row(4.0, false)?;
   for button in buttons {
      let key = format!("sunbird-{}-{}-{message}", surface.name(), button.id);
      if let Some(native) = gpui::build_surface_button(
         &key,
         &button.label,
         &raw const CLICK,
         [button.id, message],
         owned_id,
      ) {
         row = gpui::push_raw_button(row, native);
      }
   }
   Some(row)
}

unsafe extern "C" fn pressed(
   data: *const [u64; 2],
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if !data.is_null() {
      // SAFETY: the Rc callback captures the button id and the message id.
      let action = unsafe { data.read() };
      let _ = worker().try_send(action);
   }
}

fn worker() -> &'static mpsc::SyncSender<[u64; 2]> {
   WORKER.get_or_init(|| {
      let (tx, rx) = mpsc::sync_channel::<[u64; 2]>(64);
      let _ = std::thread::Builder::new()
         .name("sunbird-surface-button".into())
         .spawn(move || {
            while let Ok([id, message]) = rx.recv() {
               if let Some(engine) = phoenix_runtime::engine() {
                  crate::js::surfaces::press(&engine, id, message);
               }
            }
         });
      tx
   })
}
