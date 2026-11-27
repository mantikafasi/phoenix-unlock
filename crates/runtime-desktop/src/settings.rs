//! Injects a native GPUI **Sunbird** category (`Plugins`, `Store`, `Settings`)
//! directly into Desktop Phoenix's User Settings sidebar and renders native
//! Mana GPUI settings pages when selected.
//!
//! All patches are build-independent anchor lookups run from [`crate::prepare`]
//! before Discord initializes:
//! 1. Relabel settings page `24` (`Other -> Phoenix`) to `Sunbird` in-place and
//!    invalidate its compiled i18n lookup hash when present.
//! 2. Resolve Discord's native GPUI/Mana widget builders ([`crate::gpui`]) and
//!    hook `host::section` so page `24` draws Sunbird's native GPUI UI.
//! 3. Hook `Vec<SidebarItem>::push` in `SettingsHost::render_sidebar` using the
//!    `SidebarLayout` decoded by `sigs`, inserting a native `Sunbird` category
//!    right before Discord's Experience section (identified by Experience page
//!    icon ids, independent of locale). If the sidebar hook is unavailable,
//!    lower `is_page_visible` so page `24` appears in `Other` as a fallback.

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
use phoenix_sigs::desktop::{
   self,
   SidebarLayout,
};

use crate::gpui::{
   self,
   RawAnyElement,
   RcFnVtbl,
   SharedString,
};

/// The last flag-gated settings page, which Sunbird takes over (`24`, from
/// `sub #9; cmp #0xf`).
const SUNBIRD_PAGE: u8 = 24;

type SectionFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement;

type SidebarPushFn = unsafe extern "C" fn(vec: *mut c_void, item: *mut c_void);

#[repr(C, align(8))]
struct RawSidebarItem([u8; 0x140]);

impl RawSidebarItem {
   const fn zeroed() -> Self {
      Self([0u8; 0x140])
   }

   fn put<T>(&mut self, offset: usize, val: T) {
      if offset + size_of::<T>() <= self.0.len() {
         // SAFETY: `offset + size_of::<T>()` is inside `self.0`.
         unsafe {
            self.0
               .as_mut_ptr()
               .add(offset)
               .cast::<T>()
               .write_unaligned(val);
         }
      }
   }
}

struct SidebarHook {
   orig_push:        SidebarPushFn,
   layout:           SidebarLayout,
   icon_plugins:     u16,
   icon_store:       u16,
   icon_settings:    u16,
   experience_icons: [u16; 6],
}

static ORIG_SECTION: OnceLock<SectionFn> = OnceLock::new();
static SIDEBAR_HOOK: OnceLock<SidebarHook> = OnceLock::new();
static PAGE_OFFSET: AtomicUsize = AtomicUsize::new(0);
/// The page the last section render showed, used for sidebar active-item
/// highlighting and arm64 fallback.
static LAST_PAGE: AtomicUsize = AtomicUsize::new(usize::MAX);
static SIDEBAR_ACTIVE: AtomicBool = AtomicBool::new(false);

static SIDEBAR_CLICK_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: noop_sidebar_drop as *const c_void,
   size:          0,
   align:         1,
   call_once:     sidebar_click_callback as *const c_void,
   call_mut:      sidebar_click_callback as *const c_void,
   call:          sidebar_click_callback as *const c_void,
   extra:         ptr::null(),
};

const unsafe extern "C" fn noop_sidebar_drop(_data: *mut u8) {}

/// Whether Sunbird's native sidebar category (`Plugins`, `Store`, `Settings`)
/// is installed in Discord's left Settings sidebar.
pub fn has_sidebar_category() -> bool {
   SIDEBAR_ACTIVE.load(Ordering::Acquire)
}

/// Rust ABI callback for `SidebarItem::Action`'s `Box<dyn Fn(&mut Window, &mut App)>`.
unsafe fn sidebar_click_callback(data: *const u8, _window: *mut c_void, app: *mut c_void) {
   let _ = std::panic::catch_unwind(|| {
      let encoded = data.addr();
      if encoded == 0 {
         return;
      }
      let tab_idx = encoded - 1;
      gpui::select_sidebar_tab(tab_idx);
      LAST_PAGE.store(usize::from(SUNBIRD_PAGE), Ordering::Release);
      let page_off = PAGE_OFFSET.load(Ordering::Acquire);
      let (lo, hi) = gpui::settings_entity_id();
      if !app.is_null() && (lo != 0 || hi != 0) && page_off != 0 {
         // SAFETY: `app` is the live `&mut App` on GPUI's main thread.
         let host = unsafe { crate::modal::get_entity(app, lo, hi) };
         if !host.is_null() {
            // SAFETY: writes `SettingsPage::DesktopPhoenix` into `SettingsHost.active_page`.
            unsafe { host.cast::<u8>().add(page_off).write(SUNBIRD_PAGE) };
         }
         // SAFETY: marks `SettingsHost` dirty so both sidebar and content re-render immediately.
         let notified = unsafe { crate::modal::notify_entity(app, lo, hi) };
         if !notified {
            crate::modal::force_redraw();
         }
      } else {
         crate::modal::force_redraw();
      }
   });
}

/// Installs the Desktop User Settings sidebar label, visibility patch, native
/// GPUI builder bindings, `host::section` render hook, and native `Sunbird`
/// sidebar category hook.
pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE {
      #[cfg(target_os = "macos")]
      install_arm64(image);
      return;
   }
   patch_sidebar_label(image);
   let sidebar_ok = gpui::locate(image) && {
      hook_section_render(image);
      hook_sidebar_push(image)
   };
   if !sidebar_ok {
      patch_page_visibility(image);
   }
}

/// macOS Phoenix, arm64: the same sidebar category, or fallback page 24
/// unhidden in `Other` if the sidebar locator misses.
#[cfg(target_os = "macos")]
fn install_arm64(image: &Image<'static>) {
   use phoenix_sigs::macos;

   const NEW_NAME: &[u8] = b"Sunbird";
   const NEW_ID: &[u8] = b"DESKTOP_SUNBIRD";

   // Locate GPUI symbols and Discord's allocator up front so early exits below
   // cannot silently disable icons, modals, or other native GPUI features.
   let native = gpui::locate(image);

   let (label, limits, section) = match (
      macos::settings_label_parts(image),
      macos::settings_visibility(image),
      macos::settings_section(image),
   ) {
      (Ok(label), Ok(limits), Ok(section)) => (label, limits, section),
      (label, limits, section) => {
         let err = [label.err(), limits.err(), section.err()]
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
      .map(|&limit| {
         image
            .read_u32(limit)
            .map(|cmp| (limit, macos::lowered_guard(cmp)))
      })
      .collect::<Option<Vec<_>>>()
   else {
      log::warn!("desktop settings: visibility guard unmapped");
      return;
   };
   PAGE_OFFSET.store(section.page_offset, Ordering::Release);
   // SAFETY: runs from `prepare` before Discord's threads; `section_detour`
   // matches `host::section`'s x86-style 6-argument sret ABI.
   let Some(orig) = (unsafe {
      crate::hook_sret::<SectionFn>(
         image,
         |pro| pro.frame,
         section.func,
         section_detour as *const () as usize,
         "desktop settings: section hook failed",
      )
   }) else {
      return;
   };
   let _ = ORIG_SECTION.set(orig);
   let sidebar_ok = native && hook_sidebar_push(image);
   // SAFETY: exact-length string and one-instruction patches from `prepare`,
   // before Discord's threads run. Only lower `is_page_visible` when the native
   // `Sunbird` sidebar category hook isn't active, avoiding a duplicate item.
   let written = unsafe {
      let vis = if sidebar_ok {
         Ok(())
      } else {
         guards
            .iter()
            .try_for_each(|&(limit, lowered)| image.write_code(limit, &lowered.to_le_bytes()))
      };
      vis.and_then(|()| image.write_code(label.name, NEW_NAME))
         .and_then(|()| image.write_code(label.id, NEW_ID))
         .and_then(|()| {
            if let Some(hash_at) = label.hash_imm
               && let Some(word) = image.read_u32(hash_at)
            {
               // Flip bit 0 of the 16-bit immediate in `movz x2, #imm16` so the
               // i18n lookup misses and falls back to `"Sunbird"`.
               image.write_code(hash_at, &(word ^ (1 << 5)).to_le_bytes())
            } else {
               Ok(())
            }
         })
   };
   match written {
      Ok(()) => {
         log::info!(
            "desktop settings: page {SUNBIRD_PAGE} is Sunbird ({}), {}",
            if sidebar_ok {
               "native Sunbird sidebar category".to_owned()
            } else {
               format!("shown by {} visibility checks", guards.len())
            },
            if native {
               "drawn natively"
            } else {
               "and opens the settings window"
            }
         );
      },
      Err(err) => log::warn!("desktop settings: {err}"),
   }
}

fn patch_sidebar_label(image: &Image<'static>) {
   const NEW_NAME: &[u8] = b"Sunbird";
   const NEW_ID: &[u8] = b"SUNBIRD";
   const _: () = assert!(NEW_NAME.len() == b"Phoenix".len());
   const _: () = assert!(NEW_ID.len() == b"PHOENIX".len());

   let label = match desktop::settings_label_parts(image) {
      Ok(label) => label,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving sidebar label");
         return;
      },
   };
   // SAFETY: exact-length `.rodata`/`.rdata` string patches executed from
   // `prepare` before Discord threads run.
   if let Err(err) = unsafe { image.write_code(label.name, NEW_NAME) } {
      log::warn!("desktop settings: relabel name failed: {err}");
      return;
   }
   if let Err(err) = unsafe { image.write_code(label.id, NEW_ID) } {
      log::warn!("desktop settings: relabel id failed: {err}");
      return;
   }
   if let Some(hash_at) = label.hash_imm
      && let Some(&[byte]) = image.read(hash_at, 1)
   {
      // Flip one byte of the 64-bit i18n hash immediate so the locale lookup
      // misses and falls back to `"Sunbird"`.
      // SAFETY: 1-byte immediate patch inside `mov r64, imm64` from `prepare`.
      if let Err(err) = unsafe { image.write_code(hash_at, &[byte ^ 0xFF]) } {
         log::warn!("desktop settings: clear label hash failed: {err}");
      }
   }
   log::info!("desktop settings: relabeled page 24 to Sunbird");
}

fn patch_page_visibility(image: &Image<'static>) {
   let limit = match desktop::settings_visibility(image) {
      Ok(limit) => limit,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving it");
         return;
      },
   };
   // Tightening `cmp r8d, 0xf` -> `0x0e` excludes page 24 (`24 - 9 = 15`) from
   // the gated switch so `is_page_visible(24)` returns `true` unconditionally.
   // SAFETY: single-byte immediate patch in `.text` executed from `DllMain`.
   match unsafe { image.write_code(limit, &[0x0E]) } {
      Ok(()) => log::info!("desktop settings: page 24 visibility unlocked"),
      Err(err) => log::warn!("desktop settings: visibility patch failed: {err}"),
   }
}

fn hook_section_render(image: &Image<'static>) {
   let (fn_start, offset) = match desktop::settings_section(image) {
      Ok(section) => (section.func, section.page_offset),
      Err(err) => {
         log::warn!("desktop settings: {err}, skipping section hook");
         return;
      },
   };
   PAGE_OFFSET.store(offset, Ordering::Release);

   // SAFETY: runs from `prepare` before Discord threads execute;
   // `section_detour` matches `host::section`'s exact 6-argument sret ABI.
   let Some(orig) = (unsafe {
      crate::hook_sret::<SectionFn>(
         image,
         |pro| pro.frame,
         fn_start,
         section_detour as *const () as usize,
         "desktop settings: section hook failed",
      )
   }) else {
      return;
   };
   let _ = ORIG_SECTION.set(orig);
   log::info!("desktop settings: hooked section render @ {fn_start:#x} (page offset {offset:#x})");
}

fn hook_sidebar_push(image: &Image<'static>) -> bool {
   let found = if crate::X86_CODE {
      desktop::settings_sidebar(image)
   } else {
      #[cfg(target_os = "macos")]
      {
         phoenix_sigs::macos::settings_sidebar(image)
      }
      #[cfg(not(target_os = "macos"))]
      {
         Err("no arm64 sidebar locator off macOS".to_owned())
      }
   };
   let found = match found {
      Ok(found) => found,
      Err(err) => {
         log::info!("desktop settings: native sidebar category unavailable ({err})");
         return false;
      },
   };
   if found.layout.size > size_of::<RawSidebarItem>() {
      log::warn!(
         "desktop settings: sidebar item size {:#x} exceeds scratch buffer",
         found.layout.size
      );
      return false;
   }
   let Some(orig_push) = (if crate::X86_CODE {
      let Ok(pro) = desktop::entry_prologue(image, found.vec_push) else {
         log::warn!(
            "desktop settings: sidebar push prologue unreadable @ {:#x}",
            found.vec_push
         );
         return false;
      };
      // SAFETY: runs from `prepare` before Discord's threads start.
      match unsafe {
         image.hook_entry(
            found.vec_push,
            pro.pattern,
            pro.len,
            sidebar_push_detour as *const () as usize,
         )
      } {
         Ok(trampoline) => Some(unsafe { std::mem::transmute::<usize, SidebarPushFn>(trampoline) }),
         Err(err) => {
            log::warn!("desktop settings: sidebar push hook failed: {err}");
            None
         },
      }
   } else {
      // SAFETY: runs from `prepare` before Discord's threads start; arm64
      // decodes its own displaced prologue.
      unsafe {
         crate::hook::<SidebarPushFn>(
            image,
            |pro| pro.frame,
            found.vec_push,
            sidebar_push_detour as *const () as usize,
            "desktop settings: sidebar push hook failed",
         )
      }
   }) else {
      return false;
   };
   let _ = SIDEBAR_HOOK.set(SidebarHook {
      orig_push,
      layout: found.layout,
      icon_plugins: found.icon_plugins,
      icon_store: found.icon_store,
      icon_settings: found.icon_settings,
      experience_icons: found.experience_icons,
   });
   SIDEBAR_ACTIVE.store(true, Ordering::Release);
   log::info!(
      "desktop settings: hooked sidebar Vec::push @ {:#x} ({:x?}, icons [{}, {}, {}])",
      found.vec_push,
      found.layout,
      found.icon_plugins,
      found.icon_store,
      found.icon_settings,
   );
   true
}

/// Borrows the UTF-8 bytes of a GPUI `SharedString` without taking ownership.
///
/// # Safety
///
/// `ss` must be a valid GPUI `SharedString` initialized by Discord.
unsafe fn shared_string_bytes(ss: &SharedString) -> &[u8] {
   let tag = ss.tag;
   if tag <= 0x17 {
      // SAFETY: inline bytes live at offsets `1..=tag` inside `SharedString`.
      return unsafe {
         std::slice::from_raw_parts(ptr::from_ref(ss).cast::<u8>().add(1), usize::from(tag))
      };
   }
   if tag == 0x18 && !ss.ptr.is_null() {
      // SAFETY: static slice `ptr..ptr + len`.
      return unsafe { std::slice::from_raw_parts(ss.ptr, ss.len) };
   }
   if tag == 0x19 && !ss.ptr.is_null() {
      // SAFETY: `ArcInner` payload lives at `ptr + 0x10` for `len` bytes.
      return unsafe { std::slice::from_raw_parts(ss.ptr.add(0x10), ss.len) };
   }
   &[]
}

/// `Vec<SidebarItem>` memory layout (`cap`, `ptr`, `len` on Rust 1.85+).
#[repr(C)]
struct RawSidebarVec {
   cap: usize,
   ptr: *mut u8,
   len: usize,
}

/// Finds the Experience section header in `raw_vec` by locating the first
/// action item whose icon belongs to Discord's Experience pages, then walking
/// back to its section header. Works in every locale and during search.
unsafe fn find_experience_header(
   raw_vec: *const RawSidebarVec,
   layout: &SidebarLayout,
   experience_icons: &[u16; 6],
) -> Option<usize> {
   // SAFETY: `raw_vec` is the live `&mut Vec<SidebarItem>` passed to `Vec::push`.
   let raw = unsafe { &*raw_vec };
   if raw.ptr.is_null() || raw.len == 0 || raw.len > raw.cap {
      return None;
   }
   let first_exp = (0..raw.len).find(|&i| {
      // SAFETY: `i < raw.len <= raw.cap`, each item is `layout.size` bytes.
      let elem = unsafe { raw.ptr.add(i * layout.size) };
      let tag = unsafe { elem.cast::<u32>().read_unaligned() };
      if tag != layout.action_tag {
         return false;
      }
      let icon = unsafe { elem.add(layout.icon).cast::<u16>().read_unaligned() };
      icon != 0 && experience_icons.contains(&icon)
   })?;
   (0..first_exp).rev().find(|&i| {
      // SAFETY: `i < first_exp < raw.len <= raw.cap`.
      let elem = unsafe { raw.ptr.add(i * layout.size) };
      let tag = unsafe { elem.cast::<u32>().read_unaligned() };
      tag == layout.header_tag
   })
}

unsafe extern "C" fn sidebar_push_detour(vec: *mut c_void, item: *mut c_void) {
   // `prepare` sets `SIDEBAR_HOOK` before Discord's render threads start.
   let Some(hook) = SIDEBAR_HOOK.get() else {
      return;
   };
   let at = &hook.layout;
   let _ = std::panic::catch_unwind(|| {
      if vec.is_null() || item.is_null() {
         return;
      }
      // SAFETY: `item` points to a live `SidebarItem` of `at.size` bytes.
      let tag = unsafe { item.cast::<u32>().read_unaligned() };
      if tag != at.action_tag {
         return;
      }
      // SAFETY: `SidebarItem::Action` stores `id: SharedString` at `at.id`.
      let id_ss = unsafe {
         item
            .cast::<u8>()
            .add(at.id)
            .cast::<SharedString>()
            .read_unaligned()
      };
      // SAFETY: borrows `id_ss` only for this comparison (pod copy, no Drop).
      let is_logout = unsafe { shared_string_bytes(&id_ss) } == b"us-logout";
      if !is_logout {
         return;
      }

      let on_sunbird = LAST_PAGE.load(Ordering::Acquire) == usize::from(SUNBIRD_PAGE);
      let active_tab = gpui::active_sidebar_tab();

      let raw_vec = vec.cast::<RawSidebarVec>();
      // SAFETY: `vec` is the live `&mut Vec<SidebarItem>` passed to `Vec::push`.
      let orig_len = unsafe { (*raw_vec).len };
      let insert_at = unsafe { find_experience_header(raw_vec, at, &hook.experience_icons) };

      let push_raw = |raw: &mut RawSidebarItem| {
         // SAFETY: `orig_push` moves `at.size` bytes out of `raw` into `vec`.
         unsafe { (hook.orig_push)(vec, (&raw mut raw.0).cast::<c_void>()) };
      };

      // When falling back to above Log Out, add a leading separator; when
      // inserting before Experience, Discord already pushed one after Billing.
      if insert_at.is_none() {
         let mut sep = RawSidebarItem::zeroed();
         sep.put(0, at.separator_tag);
         push_raw(&mut sep);
      }

      // 1. Native "Sunbird" category header
      let mut hdr = RawSidebarItem::zeroed();
      hdr.put(0, at.header_tag);
      hdr.put(at.header_label, SharedString::from_static("Sunbird"));
      push_raw(&mut hdr);

      // 2. Three native sidebar items cloned from the live `us-logout` Action template
      let entries: [(&'static str, &'static str, u16, usize); 3] = [
         ("sunbird-plugins", "Plugins", hook.icon_plugins, 0),
         ("sunbird-store", "Store", hook.icon_store, 1),
         ("sunbird-settings", "Settings", hook.icon_settings, 2),
      ];
      for (id, label, icon, tab_idx) in entries {
         let mut row = RawSidebarItem::zeroed();
         // SAFETY: copies Discord's valid `None` niche tags from `item`.
         unsafe {
            ptr::copy_nonoverlapping(item.cast::<u8>(), row.0.as_mut_ptr(), at.size);
         }
         row.put(0, at.action_tag);
         row.put(at.id, SharedString::from_static(id));
         row.put(at.label, SharedString::from_static(label));
         row.put(at.on_click, ptr::without_provenance_mut::<u8>(tab_idx + 1));
         row.put(at.on_click + 8, &raw const SIDEBAR_CLICK_VTBL);
         row.put(at.icon, icon);
         row.put(at.active, u8::from(on_sunbird && active_tab == tab_idx));
         row.put(at.destructive, 0u8);
         push_raw(&mut row);
      }

      // 3. Trailing divider separating Sunbird from Experience (or Log Out)
      let mut sep_end = RawSidebarItem::zeroed();
      sep_end.put(0, at.separator_tag);
      push_raw(&mut sep_end);

      // Rotate the 5 freshly-pushed Sunbird items into place right before Experience.
      if let Some(insert_idx) = insert_at {
         const ADDED: usize = 5;
         // SAFETY: re-reads `raw_vec` after `orig_push` (which may have reallocated).
         let raw = unsafe { &*raw_vec };
         if !raw.ptr.is_null() && raw.len == orig_len + ADDED && insert_idx < orig_len {
            let tail_bytes = (raw.len - insert_idx) * at.size;
            // SAFETY: `insert_idx..raw.len` is initialized inside `raw.ptr`, and
            // `SidebarItem` is trivially bitwise-relocatable like any Rust value.
            let tail = unsafe {
               std::slice::from_raw_parts_mut(raw.ptr.add(insert_idx * at.size), tail_bytes)
            };
            tail.rotate_right(ADDED * at.size);
         }
      }
   });
   // SAFETY: push the original item (e.g. `us-logout`) via the trampoline.
   unsafe { (hook.orig_push)(vec, item) };
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
      gpui::record_settings_entity(cx);
   }
   if !host_state.is_null() && !sret.is_null() {
      let offset = PAGE_OFFSET.load(Ordering::Acquire);
      // SAFETY: `host_state` is Discord's live `&mut HostState` for this render
      // call.
      let page = unsafe { *host_state.cast::<u8>().add(offset) };
      let last = LAST_PAGE.swap(usize::from(page), Ordering::AcqRel);
      // If we just navigated away from Sunbird to a native Discord page and the
      // sidebar already rendered with stale `on_sunbird`, notify once so the
      // Sunbird sidebar highlight clears cleanly on the next frame.
      if SIDEBAR_ACTIVE.load(Ordering::Acquire)
         && last == usize::from(SUNBIRD_PAGE)
         && page != SUNBIRD_PAGE
         && !cx.is_null()
      {
         let (lo, hi) = gpui::settings_entity_id();
         // SAFETY: `[cx]` is `*mut App` inside `Context<SettingsHost>`.
         let app = unsafe { *(cx.cast::<*mut c_void>()) };
         // SAFETY: live `&mut App` on GPUI main thread.
         unsafe { crate::modal::notify_entity(app, lo, hi) };
      }
      if !crate::X86_CODE
         && page == SUNBIRD_PAGE
         && last != usize::from(page)
         && gpui::layout().is_none()
      {
         // Opening spawns a browser, which must not run on GPUI's thread.
         let _ = std::thread::Builder::new()
            .name("sunbird-settings".to_owned())
            .spawn(phoenix_ui_desktop::open_devtools);
      }
      if page == SUNBIRD_PAGE {
         let rendered = std::panic::catch_unwind(|| gpui::render_sunbird_page(sret, window, cx))
            .unwrap_or(false);
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
