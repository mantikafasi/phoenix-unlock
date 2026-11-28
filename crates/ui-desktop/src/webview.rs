//! Lightweight Discord main-window subclass for Desktop Phoenix.
//!
//! Subclasses Discord's GPUI `HWND`s (`Zed::Window`) so pressing `Ctrl+Shift+I`
//! while Discord Phoenix is focused opens the Sunbird `DevTools` / Runtime
//! Console without registering a global hotkey that would steal `Ctrl+Shift+I`
//! from browsers or editors. Plugin shortcuts (`phoenix.ui.addShortcut`) come
//! through the same subclass.

use std::{
   ffi::c_void,
   ptr,
   sync::{
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
   time::Duration,
};

const WM_NULL: u32 = 0x0000;
const WM_NCDESTROY: u32 = 0x0082;
const WM_KEYDOWN: u32 = 0x0100;
const WM_SYSKEYDOWN: u32 = 0x0104;

const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_LCONTROL: i32 = 0xA2;
const VK_RCONTROL: i32 = 0xA3;
const VK_LMENU: i32 = 0xA4;
const VK_RMENU: i32 = 0xA5;

const WH_CALLWNDPROC: i32 = 4;
const HC_ACTION: i32 = 0;
const SMTO_ABORTIFHUNG: u32 = 0x0002;

const EVENT_OBJECT_SHOW: u32 = 0x8002;
const OBJID_WINDOW: i32 = 0;
const CHILDID_SELF: i32 = 0;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

const DISCORD_CLASS: &str = "Zed::Window";
const SUBCLASS_ID: usize = 0x5342_4454; // "SBDT"

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
static HOOKED_HWNDS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

type SubclassProc =
   unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize;
type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;
type WinEventProc = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, i32, i32, u32, u32);

#[repr(C)]
struct Point {
   x: i32,
   y: i32,
}

#[repr(C)]
struct Msg {
   hwnd: *mut c_void,
   message: u32,
   wparam: usize,
   lparam: isize,
   time: u32,
   pt: Point,
}

#[repr(C)]
struct CwpStruct {
   lparam:  isize,
   wparam:  usize,
   message: u32,
   hwnd:    *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn EnumWindows(cb: unsafe extern "system" fn(*mut c_void, isize) -> i32, lparam: isize) -> i32;
   fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
   fn IsWindowVisible(hwnd: *mut c_void) -> i32;
   fn GetClassNameW(hwnd: *mut c_void, buf: *mut u16, max: i32) -> i32;
   fn GetKeyState(vk: i32) -> i16;
   fn SetWindowsHookExW(id: i32, proc: HookProc, hmod: *mut c_void, tid: u32) -> *mut c_void;
   fn UnhookWindowsHookEx(hook: *mut c_void) -> i32;
   fn CallNextHookEx(hook: *mut c_void, code: i32, wparam: usize, lparam: isize) -> isize;
   fn SendMessageTimeoutW(
      hwnd: *mut c_void,
      msg: u32,
      wparam: usize,
      lparam: isize,
      flags: u32,
      timeout_ms: u32,
      result: *mut usize,
   ) -> isize;
   fn SetWinEventHook(
      event_min: u32,
      event_max: u32,
      hmod_win_event_proc: *mut c_void,
      pfn_win_event_proc: WinEventProc,
      id_process: u32,
      id_thread: u32,
      dw_flags: u32,
   ) -> *mut c_void;
   fn UnhookWinEvent(h_win_event_hook: *mut c_void) -> i32;
   fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
   fn TranslateMessage(msg: *const Msg) -> i32;
   fn DispatchMessageW(msg: *const Msg) -> isize;
}

#[link(name = "comctl32")]
unsafe extern "system" {
   fn GetWindowSubclass(
      hwnd: *mut c_void,
      proc: SubclassProc,
      id: usize,
      ref_data: *mut usize,
   ) -> i32;
   fn SetWindowSubclass(hwnd: *mut c_void, proc: SubclassProc, id: usize, ref_data: usize) -> i32;
   fn RemoveWindowSubclass(hwnd: *mut c_void, proc: SubclassProc, id: usize) -> i32;
   fn DefSubclassProc(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GetCurrentProcessId() -> u32;
}

#[derive(Clone, Copy, Default)]
struct ModifierKey {
   any: bool,
   left: bool,
   right: bool,
}

impl ModifierKey {
   const fn is_down(self) -> bool {
      self.any || self.left || self.right
   }
}

#[derive(Clone, Copy, Default)]
struct Modifiers {
   ctrl: ModifierKey,
   alt: ModifierKey,
   shift: bool,
}

const fn resolve_shortcut(key: u8, mods: Modifiers) -> Option<crate::Shortcut> {
   if !(key.is_ascii_uppercase() || key.is_ascii_digit()) {
      return None;
   }
   // On non-US Windows layouts, AltGr synthesizes VK_LCONTROL + VK_RMENU. Let
   // those strokes pass through so third/fourth-level characters still type.
   if mods.ctrl.left && mods.alt.right && !mods.ctrl.right && !mods.alt.left {
      return None;
   }
   let ctrl = mods.ctrl.is_down();
   let alt = mods.alt.is_down();
   if !ctrl && !alt {
      return None;
   }
   Some(crate::Shortcut {
      key,
      ctrl,
      shift: mods.shift,
      alt,
   })
}

fn key_down(vk: i32) -> bool {
   // SAFETY: `GetKeyState` reads the message thread's key state for `vk`.
   unsafe { GetKeyState(vk) < 0 }
}

fn current_modifiers() -> Modifiers {
   Modifiers {
      ctrl: ModifierKey {
         any: key_down(VK_CONTROL),
         left: key_down(VK_LCONTROL),
         right: key_down(VK_RCONTROL),
      },
      alt: ModifierKey {
         any: key_down(VK_MENU),
         left: key_down(VK_LMENU),
         right: key_down(VK_RMENU),
      },
      shift: key_down(VK_SHIFT),
   }
}

unsafe extern "system" fn discord_subclass_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
   _uid: usize,
   _ref_data: usize,
) -> isize {
   match msg {
      WM_KEYDOWN | WM_SYSKEYDOWN => {
         let is_repeat = (lparam & 0x4000_0000) != 0;
         if !is_repeat
            && let Ok(key) = u8::try_from(wparam)
            && let Some(shortcut) = resolve_shortcut(key, current_modifiers())
         {
            if shortcut.key == b'I' && shortcut.ctrl && shortcut.shift && !shortcut.alt {
               crate::open_devtools();
               return 0;
            }
            if crate::take_shortcut(shortcut) {
               return 0;
            }
         }
      },
      WM_NCDESTROY => {
         HOOKED_HWNDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|&h| h != hwnd as usize);
         // SAFETY: removes this window's own subclass before teardown completes.
         unsafe { RemoveWindowSubclass(hwnd, discord_subclass_proc, SUBCLASS_ID) };
      },
      _ => {},
   }
   // SAFETY: Win32 subclass callback forwarding unhandled messages on Discord's UI
   // thread.
   unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn is_discord_class(units: &[u16]) -> bool {
   units.iter().copied().eq(DISCORD_CLASS.encode_utf16())
}

fn discord_window_tid(hwnd: *mut c_void, want_pid: u32) -> Option<u32> {
   // SAFETY: queries window metadata for `hwnd` using stack-owned buffers.
   unsafe {
      let mut pid = 0u32;
      let tid = GetWindowThreadProcessId(hwnd, &raw mut pid);
      if tid == 0 || pid != want_pid || IsWindowVisible(hwnd) == 0 {
         return None;
      }
      let mut cls = [0u16; 64];
      let len = GetClassNameW(hwnd, cls.as_mut_ptr(), 64);
      if len <= 0 {
         return None;
      }
      #[expect(clippy::cast_sign_loss, reason = "GetClassNameW length checked > 0")]
      let matched = is_discord_class(&cls[..len as usize]);
      matched.then_some(tid)
   }
}

fn install_on_ui_thread(hwnd: *mut c_void) {
   let mut ref_data = 0usize;
   // SAFETY: runs on `hwnd`'s owning UI thread inside `WH_CALLWNDPROC`;
   // `GetWindowSubclass` guards against duplicate installation.
   unsafe {
      if GetWindowSubclass(hwnd, discord_subclass_proc, SUBCLASS_ID, &raw mut ref_data) != 0 {
         return;
      }
      if SetWindowSubclass(hwnd, discord_subclass_proc, SUBCLASS_ID, 0) != 0 {
         {
            let mut hooked = HOOKED_HWNDS.lock().unwrap_or_else(PoisonError::into_inner);
            if !hooked.contains(&(hwnd as usize)) {
               hooked.push(hwnd as usize);
            }
         }
         log::info!("devtools: attached shortcuts to Discord window ({hwnd:p})");
      }
   }
}

unsafe extern "system" fn cwp_attach_proc(code: i32, wparam: usize, lparam: isize) -> isize {
   if code == HC_ACTION && lparam != 0 {
      // SAFETY: `HC_ACTION` guarantees `lparam` points to a valid `CwpStruct`.
      let cwp = unsafe { &*(lparam as *const CwpStruct) };
      if cwp.message == WM_NULL && !cwp.hwnd.is_null() {
         // SAFETY: `GetCurrentProcessId` has no preconditions.
         let pid = unsafe { GetCurrentProcessId() };
         if discord_window_tid(cwp.hwnd, pid).is_some() {
            install_on_ui_thread(cwp.hwnd);
         }
      }
   }
   // SAFETY: forwards the hook event down the chain.
   unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

fn try_subclass(hwnd: *mut c_void, want_pid: u32) {
   let Some(tid) = discord_window_tid(hwnd, want_pid) else {
      return;
   };
   if HOOKED_HWNDS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .contains(&(hwnd as usize))
   {
      return;
   }
   // `SetWindowSubclass` fails when called off the window's thread, so marshal
   // attachment onto `tid` via a temporary `WH_CALLWNDPROC` + `WM_NULL` ping.
   // SAFETY: `tid` is a live window thread in this process; the hook is always
   // removed after `SendMessageTimeoutW` completes or times out.
   unsafe {
      let hook = SetWindowsHookExW(WH_CALLWNDPROC, cwp_attach_proc, ptr::null_mut(), tid);
      if hook.is_null() {
         return;
      }
      let mut result = 0usize;
      SendMessageTimeoutW(hwnd, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 1000, &raw mut result);
      UnhookWindowsHookEx(hook);
   }
}

unsafe extern "system" fn enum_discord_windows(hwnd: *mut c_void, _lparam: isize) -> i32 {
   // SAFETY: `GetCurrentProcessId` has no preconditions.
   let pid = unsafe { GetCurrentProcessId() };
   try_subclass(hwnd, pid);
   1
}

fn attach_all_windows() {
   // SAFETY: `EnumWindows` calls `enum_discord_windows` synchronously for each
   // window.
   unsafe {
      EnumWindows(enum_discord_windows, 0);
   }
}

unsafe extern "system" fn win_event_proc(
   _hook: *mut c_void,
   event: u32,
   hwnd: *mut c_void,
   id_object: i32,
   id_child: i32,
   _event_thread: u32,
   _event_time: u32,
) {
   if event == EVENT_OBJECT_SHOW
      && id_object == OBJID_WINDOW
      && id_child == CHILDID_SELF
      && !hwnd.is_null()
   {
      // SAFETY: `GetCurrentProcessId` has no preconditions.
      let pid = unsafe { GetCurrentProcessId() };
      try_subclass(hwnd, pid);
   }
}

/// Starts a background watcher that attaches our subclass to every visible
/// Discord GPUI window (`Zed::Window`) as it opens or reopens, so
/// `Ctrl+Shift+I` and plugin shortcuts stay active across tray hides and extra
/// windows.
pub fn start_window_hook() {
   if WATCHER_STARTED.swap(true, Ordering::AcqRel) {
      return;
   }
   let _ = thread::Builder::new()
      .name("sunbird-hwnd".to_owned())
      .spawn(|| {
         // SAFETY: installs a per-process out-of-context WinEvent hook and pumps
         // this thread's message queue until process exit.
         unsafe {
            let pid = GetCurrentProcessId();
            let hook = SetWinEventHook(
               EVENT_OBJECT_SHOW,
               EVENT_OBJECT_SHOW,
               ptr::null_mut(),
               win_event_proc,
               pid,
               0,
               WINEVENT_OUTOFCONTEXT,
            );
            attach_all_windows();
            if hook.is_null() {
               loop {
                  thread::sleep(Duration::from_millis(500));
                  attach_all_windows();
               }
            }
            let mut msg = std::mem::zeroed::<Msg>();
            while GetMessageW(&raw mut msg, ptr::null_mut(), 0, 0) > 0 {
               TranslateMessage(&raw const msg);
               DispatchMessageW(&raw const msg);
            }
            UnhookWinEvent(hook);
         }
      });
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn resolves_devtools_and_digit_shortcuts() {
      let devtools = resolve_shortcut(
         b'I',
         Modifiers {
            ctrl: ModifierKey {
               any: true,
               left: true,
               ..ModifierKey::default()
            },
            shift: true,
            ..Modifiers::default()
         },
      );
      assert_eq!(
         devtools,
         Some(crate::Shortcut {
            key: b'I',
            ctrl: true,
            shift: true,
            alt: false,
         })
      );

      let alt_num = resolve_shortcut(
         b'2',
         Modifiers {
            alt: ModifierKey {
               any: true,
               left: true,
               ..ModifierKey::default()
            },
            ..Modifiers::default()
         },
      );
      assert_eq!(
         alt_num,
         Some(crate::Shortcut {
            key: b'2',
            ctrl: false,
            shift: false,
            alt: true,
         })
      );
   }

   #[test]
   fn ignores_alt_gr_keystrokes() {
      let alt_gr = Modifiers {
         ctrl: ModifierKey {
            any: true,
            left: true,
            ..ModifierKey::default()
         },
         alt: ModifierKey {
            any: true,
            right: true,
            ..ModifierKey::default()
         },
         ..Modifiers::default()
      };
      assert_eq!(resolve_shortcut(b'Q', alt_gr), None);
      assert_eq!(
         resolve_shortcut(
            b'A',
            Modifiers {
               shift: true,
               ..alt_gr
            }
         ),
         None
      );
   }

   #[test]
   fn keeps_explicit_ctrl_alt_and_synthetic_modifiers() {
      assert_eq!(
         resolve_shortcut(
            b'K',
            Modifiers {
               ctrl: ModifierKey {
                  any: true,
                  left: true,
                  ..ModifierKey::default()
               },
               alt: ModifierKey {
                  any: true,
                  left: true,
                  ..ModifierKey::default()
               },
               ..Modifiers::default()
            }
         ),
         Some(crate::Shortcut {
            key: b'K',
            ctrl: true,
            shift: false,
            alt: true,
         })
      );
      assert_eq!(
         resolve_shortcut(
            b'K',
            Modifiers {
               ctrl: ModifierKey {
                  any: true,
                  right: true,
                  ..ModifierKey::default()
               },
               alt: ModifierKey {
                  any: true,
                  right: true,
                  ..ModifierKey::default()
               },
               ..Modifiers::default()
            }
         ),
         Some(crate::Shortcut {
            key: b'K',
            ctrl: true,
            shift: false,
            alt: true,
         })
      );
      assert_eq!(
         resolve_shortcut(
            b'B',
            Modifiers {
               ctrl: ModifierKey {
                  any: true,
                  ..ModifierKey::default()
               },
               shift: true,
               ..Modifiers::default()
            }
         ),
         Some(crate::Shortcut {
            key: b'B',
            ctrl: true,
            shift: true,
            alt: false,
         })
      );
   }

   #[test]
   fn ignores_unmodified_and_function_key_codes() {
      assert_eq!(resolve_shortcut(b'A', Modifiers::default()), None);
      // VK_F4 (0x73 == b's') shares ASCII lowercase range and is not a Shortcut key.
      assert_eq!(
         resolve_shortcut(
            0x73,
            Modifiers {
               alt: ModifierKey {
                  any: true,
                  left: true,
                  ..ModifierKey::default()
               },
               ..Modifiers::default()
            }
         ),
         None
      );
   }

   #[test]
   fn matches_only_gpui_window_class() {
      let zed: Vec<u16> = "Zed::Window".encode_utf16().collect();
      let tray: Vec<u16> = "SunbirdTrayWindow".encode_utf16().collect();
      assert!(is_discord_class(&zed));
      assert!(!is_discord_class(&tray));
      assert!(!is_discord_class(&[]));
   }
}
