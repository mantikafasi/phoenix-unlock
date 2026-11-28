//! Keyboard shortcuts for `phoenix.ui.addShortcut`. Discord's UI thread only
//! checks the pressed key against what plugins registered and hands a match to
//! a worker, which runs the handlers, so the window never waits for the
//! plugin engine.

use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
   mpsc,
};

use phoenix_ui_desktop::Shortcut;

/// The keys plugins registered, each with how many handlers hold it.
static KEYS: Mutex<Vec<(Shortcut, usize)>> = Mutex::new(Vec::new());
static WORKER: OnceLock<mpsc::Sender<Shortcut>> = OnceLock::new();

/// Starts taking `shortcut` from Discord, until the matching [`release`].
pub fn hold(shortcut: Shortcut) {
   phoenix_ui_desktop::set_shortcut_handler(take);
   let mut keys = KEYS.lock().unwrap_or_else(PoisonError::into_inner);
   match keys.iter_mut().find(|(key, _)| *key == shortcut) {
      Some((_, holders)) => *holders += 1,
      None => keys.push((shortcut, 1)),
   }
}

/// Leaves `shortcut` to Discord again once no handler holds it.
pub fn release(shortcut: Shortcut) {
   let mut keys = KEYS.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some((_, holders)) = keys.iter_mut().find(|(key, _)| *key == shortcut) {
      *holders = holders.saturating_sub(1);
   }
   keys.retain(|&(_, holders)| holders > 0);
}

/// On Discord's UI thread: whether a plugin holds `shortcut`, queueing its
/// handlers when one does.
fn take(shortcut: Shortcut) -> bool {
   let held = KEYS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .any(|&(key, _)| key == shortcut);
   held && worker().send(shortcut).is_ok()
}

fn worker() -> &'static mpsc::Sender<Shortcut> {
   WORKER.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<Shortcut>();
      let _ = std::thread::Builder::new()
         .name("sunbird-shortcuts".to_owned())
         .spawn(move || {
            while let Ok(shortcut) = rx.recv() {
               if let Some(engine) = phoenix_runtime::engine() {
                  crate::js::ui::run_shortcut(&engine, shortcut);
               }
            }
         });
      tx
   })
}

/// Linux GPUI reads keys from libwayland-client and libxcb, both shared
/// libraries, so their event paths are hooked there and need no locator.
/// Keys are matched by evdev code, the physical key, as `X11` key codes are
/// evdev codes plus 8.
#[cfg(target_os = "linux")]
pub mod linux {
   use std::{
      ffi::{
         CStr,
         c_char,
         c_int,
         c_void,
      },
      ptr::{
         self,
         NonNull,
      },
      sync::{
         OnceLock,
         atomic::{
            AtomicPtr,
            AtomicU32,
            Ordering,
         },
      },
   };

   use phoenix_hook::Inline;
   use phoenix_ui_desktop::Shortcut;

   /// libwayland-client's `wl_dispatcher_func_t`, with `union wl_argument`
   /// read as the 8-byte words it is.
   type Dispatcher =
      unsafe extern "C" fn(*const c_void, *mut c_void, u32, *const c_void, *const u64) -> c_int;
   type AddDispatcher =
      unsafe extern "C" fn(*mut c_void, Dispatcher, *const c_void, *mut c_void) -> c_int;
   type GetClass = unsafe extern "C" fn(*mut c_void) -> *const c_char;
   type NextEvent = unsafe extern "C" fn(*mut c_void) -> *mut u8;

   /// The same bits in `X11` key state and in a Wayland keymap's modifier
   /// mask, whose first indices xkbcommon fixes as Shift, Lock, Control and
   /// Mod1 (Alt).
   const SHIFT: u32 = 1 << 0;
   const CONTROL: u32 = 1 << 2;
   const ALT: u32 = 1 << 3;

   static ADD_DISPATCHER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
   static POLL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
   static WAIT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
   static GET_CLASS: OnceLock<GetClass> = OnceLock::new();
   static KEYBOARD: OnceLock<Dispatcher> = OnceLock::new();
   static WAYLAND_MODS: AtomicU32 = AtomicU32::new(0);
   /// The evdev code of the taken key still held down, 0 for none.
   static HELD: AtomicU32 = AtomicU32::new(0);

   /// Hooks both libraries before GPUI connects, loading libwayland-client
   /// early when it is installed. libxkbcommon-x11, which Discord links,
   /// always loads libxcb.
   pub fn install() {
      if let Some(xcb) = open(c"libxcb.so.1") {
         attach(xcb, c"xcb_poll_for_event", poll_detour as *const (), &POLL);
         attach(xcb, c"xcb_wait_for_event", wait_detour as *const (), &WAIT);
      }
      let Some(wayland) = open(c"libwayland-client.so.0") else {
         log::info!("shortcuts: no libwayland-client, X11 keys only");
         return;
      };
      // SAFETY: a live handle and a NUL-terminated name.
      let get_class = unsafe { libc::dlsym(wayland, c"wl_proxy_get_class".as_ptr()) };
      if get_class.is_null() {
         log::warn!("shortcuts: libwayland-client has no wl_proxy_get_class");
         return;
      }
      // SAFETY: `wl_proxy_get_class` has this signature.
      let _ = GET_CLASS.set(unsafe { std::mem::transmute::<*mut c_void, GetClass>(get_class) });
      attach(
         wayland,
         c"wl_proxy_add_dispatcher",
         add_dispatcher_detour as *const (),
         &ADD_DISPATCHER,
      );
   }

   fn open(name: &CStr) -> Option<*mut c_void> {
      // SAFETY: a NUL-terminated library name.
      let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) };
      (!lib.is_null()).then_some(lib)
   }

   fn attach(lib: *mut c_void, name: &CStr, detour: *const (), slot: &'static AtomicPtr<c_void>) {
      // SAFETY: a live handle and a NUL-terminated name.
      let target = NonNull::new(unsafe { libc::dlsym(lib, name.as_ptr()) });
      let (Some(target), Some(detour)) = (target, NonNull::new(detour.cast_mut().cast())) else {
         log::warn!("shortcuts: {} not found", name.to_string_lossy());
         return;
      };
      // SAFETY: each detour has its target's C signature and reads the
      // original from `slot`, which sunhook fills before the jump.
      match unsafe { Inline::attach_into(target, detour, slot) } {
         Ok(_) => log::info!("shortcuts: hooked {}", name.to_string_lossy()),
         Err(err) => log::warn!("shortcuts: {}: {err}", name.to_string_lossy()),
      }
   }

   /// Whether Discord loses the key with evdev `code`. Releases and repeats of
   /// a taken key go with it, so GPUI never sees half a key.
   fn filter(code: u32, pressed: bool, mods: u32) -> bool {
      const ROWS: [(u32, &[u8]); 4] = [
         (2, b"1234567890"),
         (16, b"QWERTYUIOP"),
         (30, b"ASDFGHJKL"),
         (44, b"ZXCVBNM"),
      ];

      if !pressed {
         return HELD
            .compare_exchange(code, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok();
      }
      if HELD.load(Ordering::Relaxed) == code {
         return true;
      }
      let Some(key) = ROWS.iter().find_map(|&(first, keys)| {
         let at = usize::try_from(code.checked_sub(first)?).ok()?;
         keys.get(at).copied()
      }) else {
         return false;
      };
      let shortcut = Shortcut {
         key,
         ctrl: mods & CONTROL != 0,
         shift: mods & SHIFT != 0,
         alt: mods & ALT != 0,
      };
      let taken = if key == b'I' && shortcut.ctrl && shortcut.shift && !shortcut.alt {
         // Opening spawns a browser, which must not block GPUI's thread.
         std::thread::spawn(phoenix_ui_desktop::open_devtools);
         true
      } else {
         (shortcut.ctrl || shortcut.alt) && crate::shortcuts::take(shortcut)
      };
      if taken {
         HELD.store(code, Ordering::Relaxed);
      }
      taken
   }

   /// Frees the key events [`filter`] takes and reads the next one.
   fn next_event(connection: *mut c_void, slot: &AtomicPtr<c_void>) -> *mut u8 {
      const KEY_PRESS: u8 = 2;
      const KEY_RELEASE: u8 = 3;
      const EVDEV: u32 = 8;

      // SAFETY: sunhook filled `slot` with the original before hooking.
      let next =
         unsafe { std::mem::transmute::<*mut c_void, NextEvent>(slot.load(Ordering::Acquire)) };
      loop {
         // SAFETY: forwards the caller's connection.
         let event = unsafe { next(connection) };
         if event.is_null() {
            return event;
         }
         // SAFETY: every xcb event is at least 32 bytes; a key event has its
         // key code at 1 and its `u16` modifier state at 28.
         let (kind, code, state) = unsafe {
            (
               *event & 0x7F,
               u32::from(*event.add(1)),
               u32::from(event.add(28).cast::<u16>().read_unaligned()),
            )
         };
         let key = matches!(kind, KEY_PRESS | KEY_RELEASE)
            && filter(code.wrapping_sub(EVDEV), kind == KEY_PRESS, state);
         if !key {
            return event;
         }
         // SAFETY: xcb hands the caller each event to free.
         unsafe { libc::free(event.cast()) };
      }
   }

   unsafe extern "C" fn poll_detour(connection: *mut c_void) -> *mut u8 {
      next_event(connection, &POLL)
   }

   unsafe extern "C" fn wait_detour(connection: *mut c_void) -> *mut u8 {
      next_event(connection, &WAIT)
   }

   /// Puts [`keyboard_detour`] in front of wayland-backend's dispatcher on
   /// each `wl_keyboard`.
   unsafe extern "C" fn add_dispatcher_detour(
      proxy: *mut c_void,
      dispatcher: Dispatcher,
      implementation: *const c_void,
      data: *mut c_void,
   ) -> c_int {
      // SAFETY: sunhook filled the slot with the original before hooking.
      let original = unsafe {
         std::mem::transmute::<*mut c_void, AddDispatcher>(ADD_DISPATCHER.load(Ordering::Acquire))
      };
      let keyboard = GET_CLASS.get().is_some_and(|get_class| {
         // SAFETY: `proxy` is live and has its interface from creation, whose
         // name is a static NUL-terminated string.
         unsafe {
            let class = get_class(proxy);
            !class.is_null() && CStr::from_ptr(class) == c"wl_keyboard"
         }
      });
      let wrapped = keyboard && ptr::fn_addr_eq(*KEYBOARD.get_or_init(|| dispatcher), dispatcher);
      let dispatcher = if wrapped { keyboard_detour } else { dispatcher };
      // SAFETY: forwards the caller's arguments.
      unsafe { original(proxy, dispatcher, implementation, data) }
   }

   /// Tracks `wl_keyboard.modifiers` and drops `wl_keyboard.key` events that
   /// [`filter`] takes.
   unsafe extern "C" fn keyboard_detour(
      implementation: *const c_void,
      proxy: *mut c_void,
      opcode: u32,
      message: *const c_void,
      args: *const u64,
   ) -> c_int {
      const KEY: u32 = 3;
      const MODIFIERS: u32 = 4;

      // SAFETY: libwayland passes the event's arguments, `key(serial, time,
      // key, state)` or `modifiers(serial, depressed, latched, locked,
      // group)`, each a word whose low half is the `u32`.
      let word = |at: usize| unsafe { args.add(at).cast::<u32>().read() };
      match opcode {
         MODIFIERS => WAYLAND_MODS.store(word(1) | word(2), Ordering::Relaxed),
         KEY if filter(word(2), word(3) != 0, WAYLAND_MODS.load(Ordering::Relaxed)) => {
            return 0;
         },
         _ => {},
      }
      let Some(dispatcher) = KEYBOARD.get() else {
         return 0;
      };
      // SAFETY: forwards the event to the dispatcher it was meant for.
      unsafe { dispatcher(implementation, proxy, opcode, message, args) }
   }
}
