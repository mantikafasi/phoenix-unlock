//! Desktop Phoenix's User Settings: Discord's own **Phoenix** page shown, and a
//! native **Sunbird** entry under the sidebar that opens Sunbird's GPUI page.
//!
//! All patches are build-independent anchor lookups run from [`crate::prepare`]
//! before Discord's threads:
//! 1. `is_page_visible`'s range guard (`pages 9..=24`) is lowered to `9..=23`,
//!    so page 24 (`Other -> Phoenix`) falls through to `return true` and shows
//!    with Discord's own label and content.
//! 2. The sidebar's entry push gets a native **Sunbird** row, built by Log
//!    Out's own constructor and pushed right before Log Out (so under Phoenix),
//!    and `host::section` (the one loader of `account-loading`) draws
//!    [`gpui::render_sunbird_page`] instead of the active page while Sunbird is
//!    open.
//! 3. Discord's page-changed handler, which the page setters a sidebar row runs
//!    call after storing the page (even when it stays the same), closes
//!    Sunbird, so any sidebar row leaves it.
//!
//! The active page byte is never written: no page id is free (every value in
//! `0..30` is a real page, and over a thousand sites read the byte). While
//! Sunbird is open, the sidebar's selected-page map answers no page during the
//! sidebar's render instead, so the last page's row deselects.
//!
//! A build where the sidebar's rows or the handler don't decode, and arm64,
//! fall back to taking page 24 over, relabeled `Sunbird` in place.

use std::{
   ffi::c_void,
   ptr,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         AtomicUsize,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   Found,
   index::Index,
   locate::settings as locate,
};

use crate::gpui::{
   self,
   RawAnyElement,
   RcFnVtbl,
   SharedString,
};

/// The last flag-gated settings page, `Other -> Phoenix` (`24`, from `sub #9;
/// cmp #0xf`), which the fallback takes over.
const PHOENIX_PAGE: u8 = 24;

/// The largest sidebar entry and click closure Sunbird's row is built in.
const MAX_ENTRY: usize = 0x200;
const MAX_CLOSURE: usize = 0x40;

/// `host::section` and the sidebar render share this shape.
type RenderFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement;

/// `fn(&mut HostState, &mut Context)`. Its result, if any, passes through.
type PageChangedFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize;

/// The sidebar's selected-page map, `fn(page) -> page`.
type SelectedFn = unsafe extern "C" fn(u8) -> u8;

/// Past every page id, which the sidebar only compares with its rows' pages.
const NO_PAGE: u8 = u8::MAX;

/// The sidebar's `fn(&mut Vec<Entry>, &mut Entry)`.
type PushFn = unsafe extern "C" fn(*mut c_void, *mut u8);

/// Log Out's row constructor, see [`locate::SidebarRows::row`].
type RowFn = unsafe extern "C" fn(
   out: *mut u8,
   id: *const u8,
   id_len: usize,
   label: *const SharedString,
   icon: u16,
   selected: bool,
   danger: bool,
   closure: *const u8,
) -> *mut u8;

/// What Sunbird's row is built from.
struct Rows {
   row:          RowFn,
   click_vtable: usize,
   click_offset: usize,
   icon:         u16,
   /// Sunbird's click with the layout of Log Out's closure, so Discord frees
   /// the box Log Out's constructor makes.
   click:        RcFnVtbl,
}

/// How Sunbird reaches its page on this build.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
   /// Its own sidebar entry, with page 24 left to Discord.
   Entry,
   /// Page 24 relabeled and drawn by Sunbird.
   TakeOver,
}

static MODE: OnceLock<Mode> = OnceLock::new();
/// Whether the Sunbird entry was clicked since the last page change.
static OPEN: AtomicBool = AtomicBool::new(false);
static ORIG_SECTION: OnceLock<RenderFn> = OnceLock::new();
static ORIG_PUSH: OnceLock<PushFn> = OnceLock::new();
static ORIG_SIDEBAR: OnceLock<RenderFn> = OnceLock::new();
static ORIG_SELECTED: OnceLock<SelectedFn> = OnceLock::new();
/// Set while the sidebar renders with Sunbird open.
static UNSELECT: AtomicBool = AtomicBool::new(false);
static ROWS: OnceLock<Rows> = OnceLock::new();
static ORIG_PAGE_CHANGED: OnceLock<PageChangedFn> = OnceLock::new();
static PAGE_OFFSET: AtomicUsize = AtomicUsize::new(0);
/// The page the last section render showed, so the arm64 fallback opens the
/// settings window once per visit rather than once per frame.
static LAST_PAGE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Whether [`install`] hooked the section render that draws Sunbird's page.
pub fn hooked() -> bool {
   ORIG_SECTION.get().is_some()
}

/// Shows page 24, hooks `host::section`, and adds the Sunbird entry, or takes
/// page 24 over where the entry can't be added.
pub fn install(image: &Image<'static>, ix: &Index<'_>) {
   if !crate::X86_CODE {
      #[cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")))]
      install_arm64(image, ix);
      return;
   }
   patch_page_visibility(image, ix);
   if !gpui::locate(image, ix) {
      return;
   }
   let Some(page_offset) = hook_section(image, locate::settings_section(ix)) else {
      return;
   };
   if install_entry(image, ix, page_offset) {
      let _ = MODE.set(Mode::Entry);
      log::info!("desktop settings: Sunbird has its own entry, page {PHOENIX_PAGE} stays Phoenix");
   } else {
      relabel(image, ix);
   }
}

/// macOS Phoenix, arm64: page 24 taken over, drawn with
/// Discord's own builders ([`gpui::locate`]) or opening Sunbird's settings
/// window when they don't decode. Nothing is written unless the visibility
/// guards and `host::section` both decode.
#[cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")))]
fn install_arm64(image: &Image<'static>, ix: &Index<'_>) {
   use phoenix_sigs::macos;

   // Locate GPUI symbols and Discord's allocator up front so early exits below
   // cannot silently disable icons, modals, or other native GPUI features.
   let native = gpui::locate(image, ix);

   let (limits, section) = match (
      locate::settings_visibility(ix),
      locate::settings_section(ix),
   ) {
      (Ok(limits), Ok(section)) => (limits, section),
      (limits, section) => {
         let err = [limits.err(), section.err()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
         log::warn!("desktop settings: {err}, leaving the sidebar");
         return;
      },
   };
   let Some(guards) = limits
      .iter()
      .map(|guard| {
         image
            .read_u32(guard.cmp)
            .map(|cmp| (guard.cmp, macos::lowered_guard(cmp)))
      })
      .collect::<Option<Vec<_>>>()
   else {
      log::warn!("desktop settings: visibility guard unmapped");
      return;
   };
   if hook_section(image, Ok(section)).is_none() {
      return;
   }
   // SAFETY: one-instruction patches from `prepare`, before Discord's threads
   // run.
   let shown = unsafe {
      guards
         .iter()
         .try_for_each(|&(limit, lowered)| image.write_code(limit, &lowered.to_le_bytes()))
   };
   if let Err(err) = shown {
      log::warn!("desktop settings: {err}");
      return;
   }
   relabel(image, ix);
   log::info!(
      "desktop settings: shown by {} visibility checks{}",
      guards.len(),
      if native {
         ""
      } else {
         ", opening the settings window"
      }
   );
}

/// The value, or logs why this build has none.
fn found<T>(result: Found<T>) -> Option<T> {
   result
      .map_err(|err| log::warn!("desktop settings: {err}"))
      .ok()
}

/// Falls back to page 24 as Sunbird: its `Phoenix` name and the `PHOENIX` that
/// ends its id replaced in place.
fn relabel(image: &Image<'static>, ix: &Index<'_>) {
   const NAME: &[u8] = b"Sunbird";
   const SUFFIX: &[u8] = b"SUNBIRD";
   const _: () = assert!(NAME.len() == b"Phoenix".len());
   const _: () = assert!(SUFFIX.len() == b"PHOENIX".len());

   let Some(label) = found(locate::settings_label(ix)) else {
      return;
   };
   // SAFETY: exact-length string patches from `prepare`, before Discord's
   // threads run. The id ends in the suffix, so it becomes `DESKTOP_SUNBIRD` or
   // `SUNBIRD`.
   let written = unsafe {
      image
         .write_code(label.name, NAME)
         .and_then(|()| image.write_code(label.suffix, SUFFIX))
   };
   match written {
      Ok(()) => {
         let _ = MODE.set(Mode::TakeOver);
         log::info!("desktop settings: page {PHOENIX_PAGE} is Sunbird");
      },
      Err(err) => log::warn!("desktop settings: relabel failed: {err}"),
   }
}

fn patch_page_visibility(image: &Image<'static>, ix: &Index<'_>) {
   let Some(guards) = found(locate::settings_visibility(ix)) else {
      return;
   };
   // Tightening `cmp r8d, 0xf` -> `0x0e` excludes page 24 (`24 - 9 = 15`) from
   // the gated switch so `is_page_visible(24)` returns `true` unconditionally.
   for guard in guards {
      // SAFETY: single-byte immediate patch in `.text` executed from `DllMain`.
      match unsafe { image.write_code(guard.imm8(), &[0x0E]) } {
         Ok(()) => log::info!("desktop settings: page 24 visible @ {:#x}", guard.cmp),
         Err(err) => log::warn!("desktop settings: visibility patch failed: {err}"),
      }
   }
}

/// Hooks `host::section` and returns the active page's offset in its state.
fn hook_section(image: &Image<'static>, section: Found<locate::Section>) -> Option<usize> {
   let section = found(section)?;
   // SAFETY: runs from `prepare` before Discord's threads; `section_detour`
   // matches `host::section`'s 6-argument sret ABI.
   let orig = unsafe {
      crate::hook_sret::<RenderFn>(
         image,
         section.func,
         section_detour as *const () as usize,
         "desktop settings: section hook failed",
      )
   }?;
   let _ = ORIG_SECTION.set(orig);
   PAGE_OFFSET.store(section.page_offset, Ordering::Release);
   log::info!(
      "desktop settings: hooked section render @ {:#x} (page offset {:#x})",
      section.func,
      section.page_offset
   );
   Some(section.page_offset)
}

/// Hooks the page-changed handler, then the sidebar's entry push. The
/// handler's hook only closes Sunbird, so it is harmless if the push's then
/// fails.
fn install_entry(image: &Image<'static>, ix: &Index<'_>, page_offset: usize) -> bool {
   if !crate::X86_CODE {
      return false;
   }
   let (Some(rows), Some(changed)) = (
      found(locate::settings_sidebar_rows(ix, page_offset)),
      found(locate::settings_page_changed(ix, page_offset)),
   ) else {
      return false;
   };
   if rows.entry_size > MAX_ENTRY {
      log::warn!(
         "desktop settings: sidebar entries are {:#x} bytes",
         rows.entry_size
      );
      return false;
   }
   let (Some(size), Some(align)) = (
      image.read_usize(rows.click_vtable + 8),
      image.read_usize(rows.click_vtable + 16),
   ) else {
      log::warn!("desktop settings: Log Out's click unmapped");
      return false;
   };
   if size > MAX_CLOSURE {
      log::warn!("desktop settings: Log Out's click captures {size:#x} bytes");
      return false;
   }
   let (Some(_), Some(_)) = (
      found(phoenix_sigs::desktop::entry_moved(image, rows.push)),
      found(phoenix_sigs::desktop::entry_moved(image, changed)),
   ) else {
      return false;
   };
   let _ = ROWS.set(Rows {
      // SAFETY: the constructor the locator decoded, with `RowFn`'s ABI.
      row:          unsafe { std::mem::transmute::<usize, RowFn>(rows.row) },
      click_vtable: rows.click_vtable,
      click_offset: rows.click_offset,
      icon:         rows.gear,
      click:        RcFnVtbl {
         drop_in_place: ptr::null(),
         size,
         align,
         call_once: entry_clicked as *const c_void,
         call_mut: entry_clicked as *const c_void,
         call: entry_clicked as *const c_void,
         extra: ptr::null(),
      },
   });
   // SAFETY: runs from `prepare` before Discord's threads; the detour has the
   // handler's `fn(state, cx)` ABI.
   let Some(orig) = (unsafe {
      crate::hook::<PageChangedFn>(
         image,
         changed,
         page_changed_detour as *const () as usize,
         "desktop settings: page-changed hook failed",
      )
   }) else {
      return false;
   };
   let _ = ORIG_PAGE_CHANGED.set(orig);
   // SAFETY: as above; the detour has the push's `fn(list, entry)` ABI.
   let Some(orig) = (unsafe {
      crate::hook::<PushFn>(
         image,
         rows.push,
         push_detour as *const () as usize,
         "desktop settings: sidebar push hook failed",
      )
   }) else {
      return false;
   };
   let _ = ORIG_PUSH.set(orig);
   log::info!(
      "desktop settings: hooked sidebar push @ {:#x}, page changes @ {changed:#x}",
      rows.push
   );
   if !hook_unselect(image, &rows) {
      log::warn!("desktop settings: the active page stays selected beside Sunbird");
   }
   true
}

/// Hooks the selected-page map, then the sidebar render that has it select no
/// page row while Sunbird is open. The map's hook alone changes nothing.
fn hook_unselect(image: &Image<'static>, rows: &locate::SidebarRows) -> bool {
   let Some(_) = found(phoenix_sigs::desktop::entry_moved(image, rows.sidebar)) else {
      return false;
   };
   let Some(orig) = hook_selected(image, rows) else {
      return false;
   };
   let _ = ORIG_SELECTED.set(orig);
   // SAFETY: runs from `prepare` before Discord's threads; the sidebar render
   // has `host::section`'s ABI.
   let Some(orig) = (unsafe {
      crate::hook_sret::<RenderFn>(
         image,
         rows.sidebar,
         sidebar_detour as *const () as usize,
         "desktop settings: sidebar hook failed",
      )
   }) else {
      return false;
   };
   let _ = ORIG_SIDEBAR.set(orig);
   true
}

/// Hooks the selected-page map at its entry, or where it is too short (Linux),
/// swaps the pointer the sidebar calls it through.
fn hook_selected(image: &Image<'static>, rows: &locate::SidebarRows) -> Option<SelectedFn> {
   let detour = selected_detour as *const () as usize;
   match (
      phoenix_sigs::desktop::entry_moved(image, rows.selected),
      rows.selected_via,
   ) {
      (Ok(_), _) => {
         // SAFETY: runs from `prepare` before Discord's threads; the detour has
         // the map's `fn(u8) -> u8` ABI.
         unsafe {
            crate::hook::<SelectedFn>(
               image,
               rows.selected,
               detour,
               "desktop settings: selected-page hook failed",
            )
         }
      },
      (Err(_), Some(slot)) => {
         // SAFETY: an aligned pointer slot in the image, swapped from `prepare`
         // before Discord's threads; the old value is the map, the detour has
         // its ABI.
         let old = unsafe { image.swap_words(&[(slot, detour)]) }
            .map_err(|err| log::warn!("desktop settings: selected-page swap failed: {err}"))
            .ok()?;
         // SAFETY: the slot held the map's address.
         Some(unsafe { std::mem::transmute::<usize, SelectedFn>(*old.first()?) })
      },
      (Err(err), None) => {
         log::warn!("desktop settings: {err}");
         None
      },
   }
}

/// The sidebar render, marked for [`selected_detour`] while Sunbird is open.
unsafe extern "C" fn sidebar_detour(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   let Some(orig) = ORIG_SIDEBAR.get().copied() else {
      return sret;
   };
   UNSELECT.store(OPEN.load(Ordering::Acquire), Ordering::Release);
   // SAFETY: forwards all arguments verbatim to the original render.
   let out = unsafe { orig(sret, host_state, host_data, host_vtable, window, cx) };
   UNSELECT.store(false, Ordering::Release);
   out
}

/// No page while the sidebar renders with Sunbird open, so only Sunbird's
/// row is selected. Its other caller indexes with the page, so it always gets
/// the real one.
unsafe extern "C" fn selected_detour(page: u8) -> u8 {
   if UNSELECT.load(Ordering::Acquire) {
      return NO_PAGE;
   }
   let Some(orig) = ORIG_SELECTED.get().copied() else {
      return page;
   };
   // SAFETY: forwards the page to the original map.
   unsafe { orig(page) }
}

unsafe extern "C" fn page_changed_detour(state: *mut c_void, cx: *mut c_void) -> usize {
   OPEN.store(false, Ordering::Release);
   let Some(orig) = ORIG_PAGE_CHANGED.get().copied() else {
      return 0;
   };
   // SAFETY: forwards both arguments verbatim to the original handler.
   unsafe { orig(state, cx) }
}

/// Pushes Sunbird's row right before Log Out's, so it sits under the last
/// page, Phoenix.
unsafe extern "C" fn push_detour(list: *mut c_void, entry: *mut u8) {
   let Some(orig) = ORIG_PUSH.get().copied() else {
      return;
   };
   if let Some(rows) = ROWS.get()
      // SAFETY: every entry is the push's size, which holds the click slot.
      && unsafe { entry.add(rows.click_offset).cast::<usize>().read_unaligned() }
         == rows.click_vtable
   {
      let _ = std::panic::catch_unwind(|| push_sunbird(orig, list, rows));
   }
   // SAFETY: forwards both arguments verbatim to the original push.
   unsafe { orig(list, entry) };
}

/// Builds Sunbird's row with Log Out's constructor, then swaps in its click.
fn push_sunbird(push: PushFn, list: *mut c_void, rows: &Rows) {
   const ID: &str = "sunbird-settings";
   let mut entry = [0u64; MAX_ENTRY / 8];
   let closure = [0u64; MAX_CLOSURE / 8];
   let label = SharedString::from_static("Sunbird");
   let open = OPEN.load(Ordering::Acquire);
   // SAFETY: the constructor fills the entry (at most `MAX_ENTRY` bytes) and
   // moves in the static label and the closure's `size` zero bytes, boxed with
   // Log Out's layout. Sunbird's vtable keeps that layout and drops nothing,
   // so Discord frees the box as usual, and the push moves the entry in.
   unsafe {
      let entry = entry.as_mut_ptr().cast::<u8>();
      (rows.row)(
         entry,
         ID.as_ptr(),
         ID.len(),
         &raw const label,
         rows.icon,
         open,
         false,
         closure.as_ptr().cast(),
      );
      entry
         .add(rows.click_offset)
         .cast::<*const RcFnVtbl>()
         .write_unaligned(&raw const rows.click);
      push(list, entry);
   }
}

/// The Sunbird row's click, in the shape of [`gpui`]'s button callbacks.
unsafe extern "C" fn entry_clicked(
   _self: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   OPEN.store(true, Ordering::Release);
   crate::modal::force_redraw();
}

unsafe extern "C" fn section_detour(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   if !cx.is_null() {
      // SAFETY: `[cx]` is `*mut gpui::App` inside `Context<T>`.
      let app = unsafe { *(cx.cast::<*mut c_void>()) };
      // SAFETY: `app` is null or a live `&mut gpui::App` on the UI thread.
      unsafe { crate::mana_theme::flush_pending(app) };
   }
   if !host_state.is_null() && !sret.is_null() {
      let offset = PAGE_OFFSET.load(Ordering::Acquire);
      // SAFETY: `host_state` is Discord's live `&mut HostState` for this render
      // call.
      let page = unsafe { *host_state.cast::<u8>().add(offset) };
      let last = LAST_PAGE.swap(usize::from(page), Ordering::AcqRel);
      let mode = MODE.get().copied();
      let show = match mode {
         Some(Mode::Entry) => OPEN.load(Ordering::Acquire),
         Some(Mode::TakeOver) => page == PHOENIX_PAGE,
         None => false,
      };
      if !crate::X86_CODE
         && show
         && mode == Some(Mode::TakeOver)
         && last != usize::from(page)
         && gpui::layout().is_none()
      {
         // Opening spawns a browser, which must not run on GPUI's thread.
         let _ = std::thread::Builder::new()
            .name("sunbird-settings".to_owned())
            .spawn(phoenix_ui_desktop::open_devtools);
      }
      if show {
         let rendered =
            std::panic::catch_unwind(|| gpui::render_sunbird_page(sret)).unwrap_or(false);
         if rendered {
            return sret;
         }
      }
   }
   let Some(orig) = ORIG_SECTION.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original `host::section`.
   unsafe { orig(sret, host_state, host_data, host_vtable, window, cx) }
}
