//! The fonts Japanese text falls back to on Windows.
//!
//! GPUI lays out every line with `IDWriteFactory::CreateTextLayout` and never
//! sets a fallback, so the kana and kanji gg sans lacks come from DirectWrite's
//! system fallback, Yu Gothic UI. The factory vtable's `CreateTextLayout` slot
//! is swapped once, and while families are set each new layout gets a fallback
//! that maps the Japanese ranges to them ahead of the system fallback.

use std::{
   mem::ManuallyDrop,
   ptr,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicPtr,
         Ordering,
      },
   },
};

use crate::gpui_font::dwrite::{
   Com,
   DWriteCreateFactory,
   Guid,
   method,
   release,
};

static IID_IDWRITE_FACTORY2: Guid = Guid(0x0439_FC60, 0xCA44, 0x4994, [
   0x8D, 0xEE, 0x3A, 0x9A, 0xF7, 0xB7, 0x32, 0xEC,
]);
static IID_IDWRITE_TEXT_LAYOUT2: Guid = Guid(0x1093_C18F, 0x8D5E, 0x43F0, [
   0xB0, 0x64, 0x09, 0x17, 0x31, 0x1B, 0x52, 0x5E,
]);

const FACTORY_CREATE_TEXT_LAYOUT: usize = 18;
const FACTORY2_GET_SYSTEM_FONT_FALLBACK: usize = 26;
const FACTORY2_CREATE_FONT_FALLBACK_BUILDER: usize = 27;
const BUILDER_ADD_MAPPING: usize = 3;
const BUILDER_ADD_MAPPINGS: usize = 4;
const BUILDER_CREATE_FONT_FALLBACK: usize = 5;
const LAYOUT2_SET_FONT_FALLBACK: usize = 78;

/// `DWRITE_UNICODE_RANGE`s: CJK punctuation and kana, kanji with their
/// enclosed and extension blocks, and the full-width forms.
const JAPANESE: [[u32; 2]; JAPANESE_COUNT as usize] = [
   [0x3000, 0x30FF],
   [0x31F0, 0x4DBF],
   [0x4E00, 0x9FFF],
   [0xF900, 0xFAFF],
   [0xFF00, 0xFFEF],
   [0x2_0000, 0x3_134F],
];
const JAPANESE_COUNT: u32 = 6;

type OutFn = unsafe extern "system" fn(Com, *mut Com) -> i32;
type CreateTextLayout =
   unsafe extern "system" fn(Com, *const u16, u32, Com, f32, f32, *mut Com) -> i32;
type AddMapping = unsafe extern "system" fn(
   Com,
   *const [u32; 2],
   u32,
   *const *const u16,
   u32,
   Com,
   *const u16,
   *const u16,
   f32,
) -> i32;

static ORIGINAL: OnceLock<CreateTextLayout> = OnceLock::new();
static FACTORY: OnceLock<Result<usize, String>> = OnceLock::new();
/// The `IDWriteFontFallback` new layouts get, null for DirectWrite's own.
static ACTIVE: AtomicPtr<*const usize> = AtomicPtr::new(ptr::null_mut());
/// Each family list's fallback. None is ever released, since a layout being
/// created on another thread may still be handed one.
static BUILT: Mutex<Vec<(Vec<String>, usize)>> = Mutex::new(Vec::new());

/// A COM reference, released on drop.
struct Owned(Com);

impl Owned {
   fn out(what: &str, call: impl FnOnce(*mut Com) -> i32) -> Result<Self, String> {
      let mut object = ptr::null_mut();
      let hr = call(&raw mut object);
      if hr < 0 || object.is_null() {
         return Err(format!("{what} failed: {hr:#x}"));
      }
      Ok(Self(object))
   }
}

impl Drop for Owned {
   fn drop(&mut self) {
      // SAFETY: `self.0` is a live reference this value owns.
      unsafe { release(self.0) };
   }
}

unsafe extern "system" fn create_text_layout(
   this: Com,
   text: *const u16,
   len: u32,
   format: Com,
   max_width: f32,
   max_height: f32,
   layout: *mut Com,
) -> i32 {
   let original = *ORIGINAL.wait();
   // SAFETY: the caller's arguments, unchanged.
   let hr = unsafe { original(this, text, len, format, max_width, max_height, layout) };
   let fallback = ACTIVE.load(Ordering::Acquire);
   if hr < 0 || fallback.is_null() {
      return hr;
   }
   // SAFETY: a successful call wrote a live layout, slot 0 is
   // `QueryInterface`, and `IDWriteTextLayout2::SetFontFallback` adds its own
   // reference to the leaked fallback.
   unsafe {
      let layout = *layout;
      let query: unsafe extern "system" fn(Com, *const Guid, *mut Com) -> i32 = method(layout, 0);
      if let Ok(layout2) = Owned::out("layout2", |out| {
         query(layout, &raw const IID_IDWRITE_TEXT_LAYOUT2, out)
      }) {
         let set: unsafe extern "system" fn(Com, Com) -> i32 =
            method(layout2.0, LAYOUT2_SET_FONT_FALLBACK);
         set(layout2.0, fallback);
      }
   }
   hr
}

/// A fallback trying `families` in order for Japanese text, then the system
/// fallback for everything else.
fn build(factory: Com, families: &[String]) -> Result<Com, String> {
   let check = |hr: i32, what: &str| {
      if hr < 0 {
         Err(format!("{what} failed: {hr:#x}"))
      } else {
         Ok(())
      }
   };
   let names = families
      .iter()
      .map(|family| family.encode_utf16().chain([0]).collect::<Vec<u16>>())
      .collect::<Vec<_>>();
   let names = names.iter().map(Vec::as_ptr).collect::<Vec<_>>();
   let count = u32::try_from(names.len()).map_err(|_| "too many Japanese fonts")?;
   // SAFETY: `factory` is the live shared `IDWriteFactory2`, each slot has the
   // signature it is called with, and the arrays outlive the calls. A null
   // collection, locale and base family mean the system's and any.
   unsafe {
      let create_builder: OutFn = method(factory, FACTORY2_CREATE_FONT_FALLBACK_BUILDER);
      let builder = Owned::out("CreateFontFallbackBuilder", |out| {
         create_builder(factory, out)
      })?;
      let add_mapping: AddMapping = method(builder.0, BUILDER_ADD_MAPPING);
      check(
         add_mapping(
            builder.0,
            JAPANESE.as_ptr(),
            JAPANESE_COUNT,
            names.as_ptr(),
            count,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            1.0,
         ),
         "AddMapping",
      )?;
      let get_system: OutFn = method(factory, FACTORY2_GET_SYSTEM_FONT_FALLBACK);
      let system = Owned::out("GetSystemFontFallback", |out| get_system(factory, out))?;
      let add_mappings: unsafe extern "system" fn(Com, Com) -> i32 =
         method(builder.0, BUILDER_ADD_MAPPINGS);
      check(add_mappings(builder.0, system.0), "AddMappings")?;
      let create: OutFn = method(builder.0, BUILDER_CREATE_FONT_FALLBACK);
      let fallback = Owned::out("CreateFontFallback", |out| create(builder.0, out))?;
      Ok(ManuallyDrop::new(fallback).0)
   }
}

/// Swaps `CreateTextLayout` in the factory vtable, which every DirectWrite
/// factory in the process shares.
fn install() -> Result<usize, String> {
   let mut factory = ptr::null_mut();
   // SAFETY: a shared factory (kind 0), which lives for the process.
   let hr = unsafe { DWriteCreateFactory(0, &raw const IID_IDWRITE_FACTORY2, &raw mut factory) };
   if hr < 0 || factory.is_null() {
      return Err(format!("DWriteCreateFactory failed: {hr:#x}"));
   }
   // SAFETY: the factory is live and its vtable has slot 18, an aligned word
   // in dwrite.dll. `ORIGINAL` is set before the detour can run.
   unsafe {
      let slot = (*factory).add(FACTORY_CREATE_TEXT_LAYOUT) as usize;
      let _ = ORIGINAL.set(method(factory, FACTORY_CREATE_TEXT_LAYOUT));
      phoenix_hook::image::swap_words(&[(slot, create_text_layout as *const () as usize)])
         .map_err(|err| err.to_string())?;
   }
   log::info!("desktop fonts: hooked CreateTextLayout");
   Ok(factory as usize)
}

/// Draws Japanese text with the first installed font of `families`, or with
/// the system fallback when it is empty, from the next layout on.
pub fn set(families: &[String]) {
   let families = families
      .iter()
      .map(|family| family.trim())
      .filter(|family| !family.is_empty())
      .map(str::to_owned)
      .collect::<Vec<_>>();
   if families.is_empty() {
      ACTIVE.store(ptr::null_mut(), Ordering::Release);
      return;
   }
   let factory = match FACTORY.get_or_init(install) {
      Ok(factory) => *factory as Com,
      Err(err) => {
         log::warn!("desktop fonts: {err}");
         return;
      },
   };
   let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
   let fallback = match built.iter().find(|(known, _)| *known == families) {
      Some(&(_, fallback)) => fallback,
      None => {
         match build(factory, &families) {
            Ok(fallback) => {
               built.push((families, fallback as usize));
               fallback as usize
            },
            Err(err) => {
               log::warn!("desktop fonts: {err}");
               return;
            },
         }
      },
   };
   drop(built);
   ACTIVE.store(fallback as Com, Ordering::Release);
}
