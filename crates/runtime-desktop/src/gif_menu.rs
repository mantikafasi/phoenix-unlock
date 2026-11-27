//! The GIF picker's right-click menu (`phoenix.ui.addGifAction`) on Desktop
//! Phoenix.
//!
//! Discord builds it in one function from the tile's URL, with a single
//! `Copy Link` item. The detour runs it, then builds one more `MenuItem` per
//! plugin action with the same Mana helpers and pushes it, its click handler
//! an `Rc<u64>` token on Discord's heap that maps back to the action and URL.

use std::{
   collections::{
      HashMap,
      VecDeque,
   },
   ffi::c_void,
   ptr,
   sync::{
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
      RwLock,
      atomic::{
         AtomicU64,
         Ordering,
      },
      mpsc,
   },
};

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};
use phoenix_sigs::desktop::{
   self,
   GIF_MENU_ITEM_SIZE,
   GIF_MENU_SIZE,
   GifMenu,
};

use crate::{
   context_menu,
   gpui::{
      self,
      RcFnVtbl,
      Scratch,
      SharedString,
   },
};

/// Win64: `push rbp; push rsi; push rdi; push rbx; sub rsp, imm32;
/// lea rbp, [rsp + disp32]`, all position independent.
const BUILD_PROLOGUE: Pattern =
   Pattern::new("55 56 57 53 48 81 EC ?? ?? ?? ?? 48 8D AC 24 ?? ?? ?? ??");
const BUILD_PROLOGUE_LEN: usize = 19;
/// Room for a `MenuItem` (`GIF_MENU_ITEM_SIZE`) with slack.
const ITEM_CAP: usize = 0x200;
const HANDLER_SIZE: usize = 0x38;

type BuildFn = unsafe extern "C" fn(*mut u8, *const u8) -> *mut u8;
type ItemNewFn = unsafe extern "C" fn(*mut u8, *const u8, usize, *mut SharedString) -> *mut u8;
type ItemIconFn = unsafe extern "C" fn(*mut u8, *mut u8, *const u8) -> *mut u8;
type SetClickFn =
   unsafe extern "C" fn(*mut u8, *mut u8, *mut gpui::RcInner, *const RcFnVtbl) -> *mut u8;
type MenuPushFn = unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> *mut u8;

struct Symbols {
   orig:      BuildFn,
   item_new:  ItemNewFn,
   item_icon: ItemIconFn,
   set_click: SetClickFn,
   menu_push: MenuPushFn,
   found:     GifMenu,
}

// SAFETY: function pointers and offsets into `Discord.exe`.
unsafe impl Send for Symbols {}
unsafe impl Sync for Symbols {}

static SYMS: OnceLock<Symbols> = OnceLock::new();

/// One registered `phoenix.ui.addGifAction`, as the menu thread reads it.
#[derive(Clone, Debug)]
pub struct GifActionMeta {
   pub id:       u64,
   pub label:    String,
   pub icon:     Option<String>,
   pub priority: i32,
}

static ACTIONS: LazyLock<RwLock<Vec<GifActionMeta>>> = LazyLock::new(|| RwLock::new(Vec::new()));

#[derive(Clone)]
struct PendingClick {
   action: u64,
   url:    String,
}

const MAX_PENDING: usize = 256;
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
/// Pending clicks by token, oldest first, capped at [`MAX_PENDING`].
type ClickTable = (VecDeque<u64>, HashMap<u64, PendingClick>);

static CLICKS: LazyLock<Mutex<ClickTable>> =
   LazyLock::new(|| Mutex::new((VecDeque::new(), HashMap::new())));
static WORKER: OnceLock<mpsc::Sender<PendingClick>> = OnceLock::new();

static CLICK_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     on_click as *const c_void,
   call_mut:      on_click as *const c_void,
   call:          on_click as *const c_void,
   extra:         ptr::null(),
};

/// Whether this build's GIF menu was found and hooked.
pub fn supported() -> bool {
   SYMS.get().is_some()
}

/// Replaces the menu thread's snapshot of registered actions.
pub fn sync_actions(mut items: Vec<GifActionMeta>) {
   items.sort_by_key(|item| (item.priority, item.id));
   let any = !items.is_empty();
   *ACTIONS.write().unwrap_or_else(PoisonError::into_inner) = items;
   if any {
      worker();
   }
}

fn worker() -> &'static mpsc::Sender<PendingClick> {
   WORKER.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<PendingClick>();
      let _ = std::thread::Builder::new()
         .name("sunbird-gif-act".to_owned())
         .spawn(move || {
            while let Ok(click) = rx.recv() {
               if let Some(engine) = phoenix_runtime::engine() {
                  crate::js::ui::invoke_gif_action(&engine, click.action, &click.url);
               }
            }
         });
      tx
   })
}

fn register_click(click: PendingClick) -> u64 {
   let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
   let mut table = CLICKS.lock().unwrap_or_else(PoisonError::into_inner);
   while table.0.len() >= MAX_PENDING {
      if let Some(old) = table.0.pop_front() {
         table.1.remove(&old);
      }
   }
   table.0.push_back(token);
   table.1.insert(token, click);
   token
}

unsafe extern "C" fn on_click(
   data: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if data.is_null() {
      return;
   }
   // SAFETY: `data` is the `u64` inside the `RcInner` from `alloc_rc_u64`.
   let token = unsafe { ptr::read_unaligned(data) };
   let click = CLICKS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .1
      .get(&token)
      .cloned();
   if let Some(click) = click {
      let _ = worker().send(click);
   }
}

/// Locates and hooks the GIF menu builder from `DllMain`.
pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE || !cfg!(windows) {
      return;
   }
   let found = match desktop::gif_menu(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("gif menu: {err}, addGifAction stays off");
         return;
      },
   };
   if image
      .read(found.build_fn, BUILD_PROLOGUE_LEN)
      .is_none_or(|bytes| BUILD_PROLOGUE.find_in(bytes).first() != Some(&0))
   {
      log::warn!("gif menu: builder prologue changed, addGifAction stays off");
      return;
   }
   // SAFETY: runs from `prepare` before Discord's threads reach the builder,
   // whose `(sret, &url)` ABI `build_detour` shares.
   let orig = match unsafe {
      image.hook_entry(
         found.build_fn,
         BUILD_PROLOGUE,
         BUILD_PROLOGUE_LEN,
         build_detour as *const () as usize,
      )
   } {
      // SAFETY: the trampoline runs the builder's displaced prologue.
      Ok(trampoline) => unsafe { std::mem::transmute::<usize, BuildFn>(trampoline) },
      Err(err) => {
         log::warn!("gif menu: hook failed: {err}");
         return;
      },
   };
   // SAFETY: each address was decoded by `desktop::gif_menu` from the calls
   // that build `Copy Link`, with these signatures.
   let syms = unsafe {
      Symbols {
         orig,
         item_new: std::mem::transmute::<usize, ItemNewFn>(found.item_new),
         item_icon: std::mem::transmute::<usize, ItemIconFn>(found.item_icon),
         set_click: std::mem::transmute::<usize, SetClickFn>(found.set_click),
         menu_push: std::mem::transmute::<usize, MenuPushFn>(found.menu_push),
         found,
      }
   };
   let _ = SYMS.set(syms);
   log::info!("gif menu: hooked @ {:#x}", found.build_fn);
}

unsafe extern "C" fn build_detour(sret: *mut u8, url: *const u8) -> *mut u8 {
   let Some(syms) = SYMS.get() else {
      return sret;
   };
   // Read the URL first: the builder clones it, but `url` stays borrowed.
   let url_text = (!url.is_null())
      .then(|| {
         // SAFETY: `url` is the tile's live `String`, at offsets decoded from
         // the builder's clone of it.
         unsafe {
            let data = url
               .add(syms.found.url_ptr)
               .cast::<*const u8>()
               .read_unaligned();
            let len = url.add(syms.found.url_len).cast::<usize>().read_unaligned();
            (!data.is_null() && len < 1 << 16)
               .then(|| String::from_utf8_lossy(std::slice::from_raw_parts(data, len)).into_owned())
         }
      })
      .flatten();
   // SAFETY: forwards the original arguments.
   let out = unsafe { (syms.orig)(sret, url) };
   let Some(url_text) = url_text else {
      return out;
   };
   let actions = ACTIONS
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone();
   if actions.is_empty() || out.is_null() {
      return out;
   }
   let _ = std::panic::catch_unwind(|| {
      for action in &actions {
         // SAFETY: `out` holds the `Menu` the builder returned.
         unsafe { push_action(syms, out, action, &url_text) };
      }
   });
   out
}

/// Builds `action` as a `MenuItem` and pushes it onto `menu`.
///
/// # Safety
///
/// `menu` is a live `Menu` of [`GIF_MENU_SIZE`] bytes.
unsafe fn push_action(syms: &Symbols, menu: *mut u8, action: &GifActionMeta, url: &str) {
   const { assert!(GIF_MENU_ITEM_SIZE <= ITEM_CAP && GIF_MENU_SIZE > 0x18) };
   let Some(rc) = gpui::alloc_rc_u64(register_click(PendingClick {
      action: action.id,
      url:    url.to_owned(),
   })) else {
      return;
   };
   let id = gpui::intern_str(&format!("sunbird-gif-action-{}", action.id));
   let mut label = SharedString::from_str(&action.label);
   let mut item = Scratch::<ITEM_CAP>::zeroed();
   // SAFETY: Discord's `MenuItem::new`, taking the label by move.
   unsafe { (syms.item_new)(item.as_mut_ptr(), id.as_ptr(), id.len(), &raw mut label) };
   if let Some(icon) = context_menu::map_icon(action.icon.as_deref()) {
      let mut icon_arg = Scratch::<16>::zeroed();
      icon_arg.put(0, u64::from(syms.found.icon_tag));
      icon_arg.put(8, icon);
      let mut iconed = Scratch::<ITEM_CAP>::zeroed();
      // SAFETY: `MenuItem::icon` moves `item` into `iconed`.
      unsafe {
         (syms.item_icon)(
            iconed.as_mut_ptr(),
            item.as_mut_ptr(),
            icon_arg.as_mut_ptr(),
         )
      };
      item = iconed;
   }
   let at = syms.found.handler_offset;
   let mut old = Scratch::<HANDLER_SIZE>::zeroed();
   let mut new = Scratch::<HANDLER_SIZE>::zeroed();
   // SAFETY: the handler sits at `at` inside the item, checked against its
   // size by the locator; `set_click` moves it and the Rc into `new`.
   unsafe {
      ptr::copy_nonoverlapping(item.as_mut_ptr().add(at), old.as_mut_ptr(), HANDLER_SIZE);
      (syms.set_click)(
         new.as_mut_ptr(),
         old.as_mut_ptr(),
         rc,
         &raw const CLICK_VTBL,
      );
      ptr::copy_nonoverlapping(new.as_mut_ptr(), item.as_mut_ptr().add(at), HANDLER_SIZE);
      // `Menu::item` pushes, then copies the menu to its result, here itself.
      (syms.menu_push)(menu, menu, item.as_mut_ptr());
   }
}
