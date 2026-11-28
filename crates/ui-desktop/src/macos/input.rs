//! Screenshots of Discord's window and input posted into its own event queue,
//! for driving it from the debug shell.
//!
//! A process may capture its own windows and post events to itself without
//! Screen Recording or Accessibility.

use std::{
   ffi::{
      CStr,
      c_char,
      c_void,
   },
   ptr,
   sync::mpsc,
   time::Duration,
};

use super::{
   Id,
   Sel,
   class,
   objc_msgSend,
   on_main,
   sel,
   send,
   send_id,
   send_index,
   send_tag,
   send_usize,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
   x: f64,
   y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
   origin: Point,
   width:  f64,
   height: f64,
}

msg_send!(send_rect() -> Rect);
msg_send!(send_double() -> f64);
msg_send!(send_flag() -> bool);
msg_send!(send_post(event: Id, at_start: bool) -> ());
msg_send!(send_png(kind: usize, properties: Id) -> Id);
msg_send!(
   send_mouse(
      kind: u64,
      location: Point,
      flags: u64,
      time: f64,
      window: isize,
      context: Id,
      number: isize,
      clicks: isize,
      pressure: f32,
   ) -> Id
);
msg_send!(
   send_key(
      kind: u64,
      location: Point,
      flags: u64,
      time: f64,
      window: isize,
      context: Id,
      characters: Id,
      unmodified: Id,
      repeat: bool,
      code: u16,
   ) -> Id
);

unsafe extern "C" {
   fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

const LEFT_MOUSE_DOWN: u64 = 1;
const LEFT_MOUSE_UP: u64 = 2;
const RIGHT_MOUSE_DOWN: u64 = 3;
const RIGHT_MOUSE_UP: u64 = 4;
const MOUSE_MOVED: u64 = 5;
const KEY_DOWN: u64 = 10;
const KEY_UP: u64 = 11;

/// Keys by their ANSI key code, the index. `\0` marks codes without a
/// printable key.
const KEY_CODES: &[u8; 51] = b"asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0 `";

/// A key `press` sends, with the modifier flags held.
#[derive(Clone, Copy, Debug)]
pub enum Key {
   Char(char),
   Return,
   Tab,
   Escape,
   Backspace,
   Delete,
   Left,
   Right,
   Down,
   Up,
   Home,
   End,
   PageUp,
   PageDown,
}

impl Key {
   /// Its key code and the characters `NSEvent` reports for it.
   fn code(self) -> (u16, String) {
      let named = |code: u16, char: u32| (code, char::from_u32(char).unwrap_or('\0').to_string());
      match self {
         Self::Char(char) => {
            let lower = char.to_ascii_lowercase();
            let code = KEY_CODES
               .iter()
               .position(|&key| key != 0 && char::from(key) == lower)
               .and_then(|at| u16::try_from(at).ok())
               .unwrap_or(0);
            (code, char.to_string())
         },
         Self::Return => named(36, 0x0D),
         Self::Tab => named(48, 0x09),
         Self::Escape => named(53, 0x1B),
         Self::Backspace => named(51, 0x7F),
         Self::Delete => named(117, 0xF728),
         Self::Left => named(123, 0xF702),
         Self::Right => named(124, 0xF703),
         Self::Down => named(125, 0xF701),
         Self::Up => named(126, 0xF700),
         Self::Home => named(115, 0xF729),
         Self::End => named(119, 0xF72B),
         Self::PageUp => named(116, 0xF72C),
         Self::PageDown => named(121, 0xF72D),
      }
   }
}

/// `NSEventModifierFlags`.
pub mod modifier {
   pub const SHIFT: u64 = 1 << 17;
   pub const CONTROL: u64 = 1 << 18;
   pub const OPTION: u64 = 1 << 19;
   pub const COMMAND: u64 = 1 << 20;
}

/// Discord's key window when it is visible, else its largest visible one.
///
/// # Safety
///
/// On the main thread.
unsafe fn window() -> Option<Id> {
   // SAFETY: plain `NSApplication` and `NSWindow` getters on the main thread.
   unsafe {
      let app = send(class(c"NSApplication"), sel(c"sharedApplication"));
      let visible = |window: Id| !window.is_null() && send_flag(window, sel(c"isVisible"));
      let key = send(app, sel(c"keyWindow"));
      if visible(key) {
         return Some(key);
      }
      let windows = send(app, sel(c"windows"));
      (0..send_usize(windows, sel(c"count")))
         .map(|at| send_index(windows, sel(c"objectAtIndex:"), at))
         .filter(|&window| visible(window))
         .max_by(|&one, &other| {
            let area = |window: Id| {
               let frame = send_rect(window, sel(c"frame"));
               frame.width * frame.height
            };
            area(one).total_cmp(&area(other))
         })
   }
}

/// Runs `work` on the main thread with the window, and waits up to 5 s for
/// its answer.
fn with_window<T: Send + 'static>(work: impl FnOnce(Id) -> T + Send + 'static) -> Option<T> {
   let (answer, answered) = mpsc::sync_channel(1);
   on_main(move || {
      // SAFETY: on the main thread.
      let found = unsafe { window() }.map(work);
      let _ = answer.send(found);
   });
   answered.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// The window as a PNG, one pixel per point.
#[must_use]
pub fn screenshot() -> Option<Vec<u8>> {
   type CreateImage = unsafe extern "C" fn(Rect, u32, u32, u32) -> *mut c_void;
   type Release = unsafe extern "C" fn(*mut c_void);
   const INCLUDING_WINDOW: u32 = 1 << 3;
   const IGNORE_FRAMING: u32 = 1 << 0;
   const NOMINAL_RESOLUTION: u32 = 1 << 4;
   const PNG: usize = 4;

   let symbol = |name: &CStr| {
      // SAFETY: a lookup in the images already loaded.
      let found = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
      (!found.is_null()).then_some(found)
   };
   // Deprecated in the SDK since macOS 14, still exported.
   let create = symbol(c"CGWindowListCreateImage")?;
   let release = symbol(c"CGImageRelease")?;
   // SAFETY: CoreGraphics' own signatures.
   let (create, release) = unsafe {
      (
         std::mem::transmute::<*mut c_void, CreateImage>(create),
         std::mem::transmute::<*mut c_void, Release>(release),
      )
   };
   with_window(move |window| {
      // SAFETY: on the main thread with a live window; the image is released
      // once its PNG is copied out.
      unsafe {
         let number = u32::try_from(send_tag(window, sel(c"windowNumber"))).ok()?;
         let null = Rect {
            origin: Point {
               x: f64::INFINITY,
               y: f64::INFINITY,
            },
            width:  0.0,
            height: 0.0,
         };
         let image = create(
            null,
            INCLUDING_WINDOW,
            number,
            IGNORE_FRAMING | NOMINAL_RESOLUTION,
         );
         if image.is_null() {
            return None;
         }
         let bitmap = send(class(c"NSBitmapImageRep"), sel(c"alloc"));
         let bitmap = send_id(bitmap, sel(c"initWithCGImage:"), image);
         release(image);
         let data = send_png(
            bitmap,
            sel(c"representationUsingType:properties:"),
            PNG,
            send(class(c"NSDictionary"), sel(c"dictionary")),
         );
         let png = (!data.is_null()).then(|| {
            let bytes = send(data, sel(c"bytes")).cast::<u8>();
            let length = send_usize(data, sel(c"length"));
            std::slice::from_raw_parts(bytes, length).to_vec()
         });
         send(bitmap, sel(c"release"));
         png
      }
   })
   .flatten()
}

/// The window's point for `(x, y)` from its top left, and its number.
///
/// # Safety
///
/// On the main thread with a live window.
unsafe fn locate(window: Id, x: f64, y: f64) -> (Point, isize) {
   // SAFETY: forwarded from the caller.
   unsafe {
      let frame = send_rect(window, sel(c"frame"));
      let number = send_tag(window, sel(c"windowNumber"));
      (
         Point {
            x,
            y: frame.height - y,
         },
         number,
      )
   }
}

/// # Safety
///
/// On the main thread.
unsafe fn uptime() -> f64 {
   // SAFETY: plain `NSProcessInfo` getters.
   unsafe {
      send_double(
         send(class(c"NSProcessInfo"), sel(c"processInfo")),
         sel(c"systemUptime"),
      )
   }
}

/// # Safety
///
/// On the main thread, `event` nil or an `NSEvent`.
unsafe fn post(event: Id) {
   if event.is_null() {
      return;
   }
   // SAFETY: forwarded from the caller.
   unsafe {
      let app = send(class(c"NSApplication"), sel(c"sharedApplication"));
      send_post(app, sel(c"postEvent:atStart:"), event, false);
   }
}

/// Moves to `(x, y)` points from the window's top left and clicks `count`
/// times, with the right button when `right`.
#[must_use]
pub fn click(x: f64, y: f64, count: u8, right: bool) -> bool {
   let (down, up) = if right {
      (RIGHT_MOUSE_DOWN, RIGHT_MOUSE_UP)
   } else {
      (LEFT_MOUSE_DOWN, LEFT_MOUSE_UP)
   };
   with_window(move |window| {
      // SAFETY: on the main thread with a live window.
      unsafe {
         let (location, number) = locate(window, x, y);
         let mouse = |kind: u64, clicks: u8, pressure: f32| {
            send_mouse(
               class(c"NSEvent"),
               sel(c"mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:"),
               kind,
               location,
               0,
               uptime(),
               number,
               ptr::null_mut(),
               0,
               isize::from(clicks),
               pressure,
            )
         };
         post(mouse(MOUSE_MOVED, 0, 0.0));
         for clicks in 1..=count.max(1) {
            post(mouse(down, clicks, 1.0));
            post(mouse(up, clicks, 0.0));
         }
      }
   })
   .is_some()
}

/// Scrolls by `(dx, dy)` pixels with the pointer at `(x, y)` points from the
/// window's top left. Positive `dy` scrolls content down, toward older
/// messages.
#[must_use]
pub fn scroll(x: f64, y: f64, dx: i32, dy: i32) -> bool {
   type Create = unsafe extern "C" fn(*mut c_void, u32, u32, i32, i32, i32) -> *mut c_void;
   type SetLocation = unsafe extern "C" fn(*mut c_void, Point);
   type Release = unsafe extern "C" fn(*mut c_void);

   let symbol = |name: &CStr| {
      // SAFETY: a lookup in the images already loaded.
      let found = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
      (!found.is_null()).then_some(found)
   };
   let (Some(create), Some(set_location), Some(release)) = (
      symbol(c"CGEventCreateScrollWheelEvent2"),
      symbol(c"CGEventSetLocation"),
      symbol(c"CFRelease"),
   ) else {
      return false;
   };
   // SAFETY: CoreGraphics' own signatures.
   let (create, set_location, release) = unsafe {
      (
         std::mem::transmute::<*mut c_void, Create>(create),
         std::mem::transmute::<*mut c_void, SetLocation>(set_location),
         std::mem::transmute::<*mut c_void, Release>(release),
      )
   };
   with_window(move |window| {
      // SAFETY: on the main thread with a live window; the event is released
      // after AppKit copies it.
      unsafe {
         let frame = send_rect(window, sel(c"frame"));
         let screens = send(class(c"NSScreen"), sel(c"screens"));
         let primary = send_rect(
            send_index(screens, sel(c"objectAtIndex:"), 0),
            sel(c"frame"),
         );
         // Quartz counts from the primary screen's top left, AppKit from its
         // bottom left.
         let global = Point {
            x: frame.origin.x + x,
            y: primary.height - (frame.origin.y + frame.height - y),
         };
         let event = create(ptr::null_mut(), 0, 2, -dy, -dx, 0);
         if event.is_null() {
            return;
         }
         set_location(event, global);
         post(send_id(class(c"NSEvent"), sel(c"eventWithCGEvent:"), event));
         release(event);
      }
   })
   .is_some()
}

/// Presses and releases `key` with `modifiers` held.
#[must_use]
pub fn press(key: Key, modifiers: u64) -> bool {
   with_window(move |window| {
      // SAFETY: on the main thread with a live window.
      unsafe { press_in(window, key, modifiers) }
   })
   .is_some()
}

/// Types `text` one key at a time.
#[must_use]
pub fn type_text(text: &str) -> bool {
   let text = text.to_owned();
   with_window(move |window| {
      for char in text.chars() {
         let shift = if char.is_ascii_uppercase() {
            modifier::SHIFT
         } else {
            0
         };
         // SAFETY: on the main thread with a live window.
         unsafe { press_in(window, Key::Char(char), shift) };
      }
   })
   .is_some()
}

/// # Safety
///
/// On the main thread with a live window.
unsafe fn press_in(window: Id, key: Key, modifiers: u64) {
   let (code, characters) = key.code();
   // SAFETY: forwarded from the caller.
   unsafe {
      let (location, number) = locate(window, 0.0, 0.0);
      let chars = super::ns_string(&characters);
      let unmodified = super::ns_string(&characters.to_lowercase());
      for kind in [KEY_DOWN, KEY_UP] {
         post(send_key(
            class(c"NSEvent"),
            sel(c"keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:"),
            kind,
            location,
            modifiers,
            uptime(),
            number,
            ptr::null_mut(),
            chars,
            unmodified,
            false,
            code,
         ));
      }
   }
}
