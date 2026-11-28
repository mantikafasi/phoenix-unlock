//! Font overrides: Discord's own families swapped for installed ones.
//!
//! GPUI's `TextSystem::resolve_font(&self, font: &Font)` is observed; on entry,
//! before its caches, a `gg sans` or `ABC Ginto` family becomes the main font
//! and `gg sans mono` the code font, so a change shows once text is laid out
//! again. Families the system can't find fall back to its UI font, as GPUI
//! does for any font.

use std::sync::{
   PoisonError,
   RwLock,
};

/// The main and code families, empty for Discord's own.
static FONTS: RwLock<(&'static str, &'static str)> = RwLock::new(("", ""));

/// Sets both families, `""` keeping Discord's.
pub fn set(main: &str, mono: &str) {
   let main = crate::gpui::intern_str(main.trim());
   let mono = crate::gpui::intern_str(mono.trim());
   *FONTS.write().unwrap_or_else(PoisonError::into_inner) = (main, mono);
}

pub fn clear() {
   set("", "");
}

#[cfg(target_arch = "x86_64")]
pub fn is_supported() -> bool {
   hook::installed()
}

#[cfg(not(target_arch = "x86_64"))]
pub const fn is_supported() -> bool {
   false
}

#[cfg(target_arch = "x86_64")]
pub fn install(image: &phoenix_hook::image::Image<'static>) {
   hook::install(image);
}

/// The system font collection's family names, the ones GPUI's DirectWrite
/// text system finds by name, through raw COM vtables.
#[cfg(windows)]
pub mod dwrite {
   type Com = *mut *const usize;

   #[repr(C)]
   struct Guid(u32, u16, u16, [u8; 8]);

   static IID_IDWRITE_FACTORY: Guid = Guid(0xB859_EE5A, 0xD838, 0x4B5B, [
      0xA2, 0xE8, 0x1A, 0xDC, 0x7D, 0x93, 0xDB, 0x48,
   ]);

   #[link(name = "dwrite")]
   unsafe extern "system" {
      fn DWriteCreateFactory(kind: u32, iid: *const Guid, factory: *mut Com) -> i32;
   }

   /// Method `index` of `this`'s vtable, as `F`.
   ///
   /// # Safety
   ///
   /// `this` is a live COM object whose method at `index` has the signature
   /// `F`.
   unsafe fn method<F: Copy>(this: Com, index: usize) -> F {
      // SAFETY: forwarded from the caller; a vtable slot is a function pointer.
      unsafe { std::mem::transmute_copy(&*(*this).add(index)) }
   }

   /// # Safety
   ///
   /// `this` is a live COM object the caller owns a reference to.
   unsafe fn release(this: Com) {
      // SAFETY: `IUnknown::Release` is slot 2 of every COM vtable.
      unsafe { method::<unsafe extern "system" fn(Com) -> u32>(this, 2)(this) };
   }

   /// The `en-us` name of `names`, or its first.
   ///
   /// # Safety
   ///
   /// `names` is a live `IDWriteLocalizedStrings`.
   unsafe fn english(names: Com) -> Option<String> {
      // SAFETY: slots 4, 7 and 8 are `FindLocaleName`, `GetStringLength` and
      // `GetString`, and every buffer holds what they write.
      unsafe {
         let find: unsafe extern "system" fn(Com, *const u16, *mut u32, *mut i32) -> i32 =
            method(names, 4);
         let len: unsafe extern "system" fn(Com, u32, *mut u32) -> i32 = method(names, 7);
         let get: unsafe extern "system" fn(Com, u32, *mut u16, u32) -> i32 = method(names, 8);
         let locale: Vec<u16> = "en-us".encode_utf16().chain([0]).collect();
         let (mut index, mut exists) = (0, 0);
         if find(names, locale.as_ptr(), &raw mut index, &raw mut exists) < 0 || exists == 0 {
            index = 0;
         }
         let mut n = 0;
         if len(names, index, &raw mut n) < 0 {
            return None;
         }
         let mut buf = vec![0u16; n as usize + 1];
         if get(names, index, buf.as_mut_ptr(), n + 1) < 0 {
            return None;
         }
         buf.truncate(n as usize);
         String::from_utf16(&buf).ok()
      }
   }

   /// Installed font families by their English name, sorted.
   pub fn families() -> Vec<String> {
      let mut out = Vec::new();
      let mut factory: Com = std::ptr::null_mut();
      // SAFETY: a shared factory (kind 0) written to `factory` on success.
      if unsafe { DWriteCreateFactory(0, &raw const IID_IDWRITE_FACTORY, &raw mut factory) } < 0
         || factory.is_null()
      {
         return out;
      }
      // SAFETY: the slots are `IDWriteFactory::GetSystemFontCollection` (3),
      // `IDWriteFontCollection::GetFontFamilyCount` (3) and `GetFontFamily`
      // (4), and `IDWriteFontFamily::GetFamilyNames` (6); each object got is
      // released.
      unsafe {
         let collection_of: unsafe extern "system" fn(Com, *mut Com, i32) -> i32 =
            method(factory, 3);
         let mut collection: Com = std::ptr::null_mut();
         if collection_of(factory, &raw mut collection, 0) >= 0 && !collection.is_null() {
            let count: unsafe extern "system" fn(Com) -> u32 = method(collection, 3);
            let family_at: unsafe extern "system" fn(Com, u32, *mut Com) -> i32 =
               method(collection, 4);
            for i in 0..count(collection) {
               let mut family: Com = std::ptr::null_mut();
               if family_at(collection, i, &raw mut family) < 0 || family.is_null() {
                  continue;
               }
               let names_of: unsafe extern "system" fn(Com, *mut Com) -> i32 = method(family, 6);
               let mut names: Com = std::ptr::null_mut();
               if names_of(family, &raw mut names) >= 0 && !names.is_null() {
                  out.extend(english(names));
                  release(names);
               }
               release(family);
            }
            release(collection);
         }
         release(factory);
      }
      // Some fonts name their family with nothing printable.
      out.retain(|name| !name.trim().is_empty());
      out.sort_unstable_by_key(|name| name.to_lowercase());
      out.dedup();
      out
   }
}

#[cfg(target_arch = "x86_64")]
mod hook {
   use std::sync::{
      OnceLock,
      PoisonError,
   };

   use phoenix_hook::{
      image::Image,
      x86::{
         Observed,
         Regs,
         observe,
      },
   };
   use phoenix_sigs::desktop;

   use super::FONTS;
   use crate::gpui::SharedString;

   static HOOK: OnceLock<Observed> = OnceLock::new();

   #[derive(Clone, Copy)]
   enum Role {
      Main,
      Mono,
   }

   impl Role {
      fn of(family: &str) -> Option<Self> {
         if family.starts_with("gg sans mono") {
            Some(Self::Mono)
         } else if family.starts_with("gg sans") || family.starts_with("ABC Ginto") {
            Some(Self::Main)
         } else {
            None
         }
      }
   }

   /// `SharedString` tags, as [`SharedString`] documents them. Below `STATIC`
   /// the tag is the length of text stored inline after it.
   const STATIC: u8 = 0x18;
   const ARC: u8 = 0x19;

   pub(super) fn installed() -> bool {
      HOOK.get().is_some()
   }

   pub(super) fn install(image: &Image<'static>) {
      let func = match desktop::resolve_font(image) {
         Ok(func) => func,
         Err(err) => {
            log::warn!("desktop fonts: {err}, fonts disabled");
            return;
         },
      };
      // SAFETY: `resolve_font` found a function start, and `prepare` runs
      // before Discord's render thread exists.
      match unsafe { observe(image, func, on_resolve_font, 0) } {
         Ok(hook) => {
            let _ = HOOK.set(hook);
            log::info!("desktop fonts: observing resolve_font @ {func:#x}");
         },
         Err(err) => log::warn!("desktop fonts: observing resolve_font: {err}"),
      }
   }

   /// The family's text, borrowed from `family`.
   ///
   /// # Safety
   ///
   /// `family` is a live `SharedString`.
   unsafe fn text(family: &SharedString) -> Option<&str> {
      let (bytes, len) = match family.tag {
         // `gg sans` and its siblings are short enough to sit inline.
         len @ 0..STATIC => {
            let inline = std::ptr::from_ref(family).cast::<u8>().wrapping_add(1);
            (inline, len.into())
         },
         STATIC => (family.ptr, family.len),
         // SAFETY: an `Arc<str>`'s bytes follow its two counts.
         ARC => (unsafe { family.ptr.add(16) }, family.len),
         _ => return None,
      };
      if bytes.is_null() || len > 256 {
         return None;
      }
      // SAFETY: the string's own bytes, live as long as `family`.
      std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes, len) }).ok()
   }

   /// `resolve_font(self, font)`: rewrites `font.family`, its first field.
   extern "C" fn on_resolve_font(_: usize, regs: &Regs) {
      let fonts = *FONTS.read().unwrap_or_else(PoisonError::into_inner);
      if fonts == ("", "") {
         return;
      }
      let Some(font) = usize::try_from(regs.args()[1]).ok().filter(|&p| p != 0) else {
         return;
      };
      let family = font as *mut SharedString;
      // A panic must not unwind into the text system.
      let _ = std::panic::catch_unwind(|| {
         // SAFETY: `resolve_font` received a live `&Font` whose `family` starts
         // it.
         let Some(role) = (unsafe { text(&*family) }).and_then(Role::of) else {
            return;
         };
         let to = match role {
            Role::Main => fonts.0,
            Role::Mono => fonts.1,
         };
         if to.is_empty() {
            return;
         }
         // SAFETY: the caller's `Font` outlives the call, and a static string
         // needs no drop. An `Arc` family overwritten keeps one count it never
         // gives back, so its bytes live on, bounded by Discord's few families.
         unsafe { family.write(SharedString::from_static(to)) };
      });
   }
}
