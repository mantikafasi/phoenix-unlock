//! Native GPUI / Mana widget bindings for Desktop Phoenix User Settings.
//!
//! Resolves `Discord.exe`'s own compiled GPUI (`Div`, `StyleRefinement`,
//! `AnyElement`, `SharedString`) and Mana (`Switch`, `Button`, `TextBlock`,
//! `ActionRow`) builders from unique string anchors during [`crate::prepare`] —
//! no hardcoded addresses and no webview overlay. All controls render natively
//! inside Discord's GPUI render tree with category tabs, rounded bordered
//! cards, collapsible plugin settings dropdowns, and native select dropdowns.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   ffi::c_void,
   mem::MaybeUninit,
   ptr,
   sync::{
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU64,
         Ordering,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_hook::image::Image;
use phoenix_runtime::{
   Chosen,
   Control,
   update::{
      self,
      Catalog,
      Listed,
   },
};
use phoenix_sigs::desktop::{
   self,
   GpuiLayout,
   StyleOffsets,
   layout,
};

use crate::modal::{
   FieldKind,
   SettingField,
};

/// `gpui::SharedString` (24 bytes): tag `0x18` is `Static(&'static str)` and
/// `0x19` is `Arc<str>`, the `ArcInner` at `ptr` with the bytes at `+0x10`.
/// Not `Copy`: a heap one must reach Discord exactly once, which drops it.
#[repr(C)]
pub struct SharedString {
   pub tag: u8,
   _pad:    [u8; 7],
   pub ptr: *const u8,
   pub len: usize,
}

impl SharedString {
   pub const fn from_static(s: &'static str) -> Self {
      Self {
         tag:  0x18,
         _pad: [0; 7],
         ptr:  s.as_ptr(),
         len:  s.len(),
      }
   }

   /// An `Arc<str>` on Discord's heap, which Discord frees with the element
   /// it moves into. Interned only before the builders resolve.
   pub fn from_str(s: &str) -> Self {
      let Some(sym) = SYMBOLS.get() else {
         return Self::from_static(intern(s));
      };
      // SAFETY: Discord's own allocator, with the layout `Arc<str>`'s drop
      // frees: two counts, then the bytes, padded to 8.
      let inner = unsafe { (sym.alloc_or_oom)(8, (16 + s.len()).next_multiple_of(8)) };
      if inner.is_null() {
         return Self::from_static(intern(s));
      }
      // SAFETY: `inner` holds `16 + s.len()` bytes, aligned to 8.
      unsafe {
         inner.cast::<[usize; 2]>().write_unaligned([1, 1]);
         ptr::copy_nonoverlapping(s.as_ptr(), inner.add(16), s.len());
      }
      Self {
         tag:  0x19,
         _pad: [0; 7],
         ptr:  inner.cast_const(),
         len:  s.len(),
      }
   }
}

/// Process-lifetime strings for element IDs and badge URLs, which Discord
/// borrows as `&'static str`. Only bounded sets of strings belong here.
static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
   LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn intern_str(s: &str) -> &'static str {
   intern(s)
}

fn intern(s: &str) -> &'static str {
   let mut set = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(&existing) = set.get(s) {
      return existing;
   }
   let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
   set.insert(leaked);
   leaked
}

/// A zeroed buffer at least as large as the Discord struct built in it, whose
/// size is decoded at startup and checked against `N` by `sigs`.
#[repr(C, align(16))]
pub struct Scratch<const N: usize>([u8; N]);

impl<const N: usize> Scratch<N> {
   pub const fn zeroed() -> Self {
      Self([0; N])
   }

   pub const fn as_mut_ptr(&mut self) -> *mut u8 {
      self.0.as_mut_ptr()
   }

   /// Writes `value` at `at`, or nothing if it would not fit.
   pub fn put<T>(&mut self, at: usize, value: T) {
      if at + size_of::<T>() <= N {
         // SAFETY: in bounds of `self.0`, checked above.
         unsafe {
            self
               .0
               .as_mut_ptr()
               .add(at)
               .cast::<T>()
               .write_unaligned(value);
         }
      }
   }
}

pub type RawDiv = Scratch<{ layout::DIV_CAP }>;
type RawSwitch = Scratch<{ layout::SCRATCH_CAP }>;
pub type RawButton = Scratch<{ layout::SCRATCH_CAP }>;

#[repr(C, align(8))]
pub struct RawAnyElement([u8; layout::ANY_ELEMENT]);

#[repr(C)]
pub struct RcInner {
   strong:    usize,
   weak:      usize,
   action_id: u64,
}

/// An `Rc<u64>` whose counts never reach zero, so Discord never frees it:
/// for when Discord's allocator isn't located (macOS, whose Rust heap may be
/// its own malloc zone). Leaks 24 bytes.
pub fn pinned_rc_u64(value: u64) -> *mut RcInner {
   Box::into_raw(Box::new(RcInner {
      strong:    usize::MAX / 4,
      weak:      usize::MAX / 4,
      action_id: value,
   }))
}

#[repr(C)]
pub struct RcFnVtbl {
   pub drop_in_place: *const c_void,
   pub size:          usize,
   pub align:         usize,
   pub call_once:     *const c_void,
   pub call_mut:      *const c_void,
   pub call:          *const c_void,
   pub extra:         *const c_void,
}

// SAFETY: vtables contain only function pointers and plain integers.
unsafe impl Sync for RcFnVtbl {}

static SWITCH_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     switch_callback as *const c_void,
   call_mut:      switch_callback as *const c_void,
   call:          switch_callback as *const c_void,
   extra:         ptr::null(),
};

static BUTTON_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     button_callback as *const c_void,
   call_mut:      button_callback as *const c_void,
   call:          button_callback as *const c_void,
   extra:         ptr::null(),
};

/// Zero-sized mouse-down listener vtable for clickable custom `Div`s (tabs,
/// filter chips, compact icon actions). `data` encodes `action_id + 1` as a
/// non-null integer with no heap allocation.
static DIV_CLICK_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: noop_div_drop as *const c_void,
   size:          0,
   align:         1,
   call_once:     div_click_callback as *const c_void,
   call_mut:      div_click_callback as *const c_void,
   call:          div_click_callback as *const c_void,
   extra:         ptr::null(),
};

const unsafe extern "C" fn noop_div_drop(_data: *mut u8) {}

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum BtnVariant {
   Primary   = 0,
   Secondary = 1,
   Critical  = 3,
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum BtnSize {
   Small  = 1,
   Medium = 2,
}

/// Encodes `Some(Length::Definite(DefiniteLength::Fraction(1.0)))` (`w_full` /
/// `h_full`) in GPUI's `StyleRefinement`.
pub const W_FULL: u64 = 0x3F80_0000_0000_0002;

/// Encodes `Some(AbsoluteLength::Pixels(px))` in GPUI's `StyleRefinement`.
pub fn gpui_px(px: f32) -> u64 {
   u64::from(px.to_bits()) << 32
}

/// Encodes `Some(AbsoluteLength::Rems(base_px / 16.0))` in GPUI's
/// `StyleRefinement` so typography, icons, and spacing scale automatically with
/// Discord's UI zoom / rem size (`1rem = 16px` at 100%, `20px` at 125%, etc.).
pub fn gpui_rem(base_px: f32) -> u64 {
   (u64::from((base_px / 16.0).to_bits()) << 32) | 1
}

type DivNewFn = unsafe extern "C" fn(*mut RawDiv, *const c_void) -> *mut RawDiv;
type DivMutFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivStyleMutFn = unsafe extern "C" fn(*mut RawDiv) -> *mut u8;
type DivChildDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivChildBtnFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawButton) -> *mut RawDiv;
type DivIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawDiv) -> *mut RawAnyElement;
type DivPushAnyFn = unsafe extern "C" fn(*mut c_void, *mut RawAnyElement);
type StrIntoAnyFn =
   unsafe extern "C" fn(*mut RawAnyElement, *mut SharedString) -> *mut RawAnyElement;
type SwitchNewFn = unsafe extern "C" fn(*mut RawSwitch, *const u8, usize, u8) -> *mut RawSwitch;
type SwitchIntoDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawSwitch) -> *mut RawDiv;
type ButtonNewFn =
   unsafe extern "C" fn(*mut RawButton, *const u8, usize, *const SharedString) -> *mut RawButton;
type ActionRowFn = unsafe extern "C" fn(
   *mut RawDiv,
   *const SharedString,
   *const SharedString,
   *mut RawButton,
) -> *mut RawDiv;
/// Discord's `alloc_or_oom(align: usize, size: usize) -> *mut u8`.
type AllocOrOomFn = unsafe extern "C" fn(usize, usize) -> *mut u8;

struct GpuiSymbols {
   caller_loc:       *const c_void,
   div_new:          DivNewFn,
   div_flex:         DivMutFn,
   div_flex_col:     DivMutFn,
   div_style_mut:    DivStyleMutFn,
   div_child_div:    DivChildDivFn,
   div_child_button: DivChildBtnFn,
   div_into_any:     DivIntoAnyFn,
   div_into_any_raw: usize,
   div_push_any:     Option<DivPushAnyFn>,
   str_into_any:     Option<StrIntoAnyFn>,
   switch_new:       SwitchNewFn,
   switch_into_div:  SwitchIntoDivFn,
   button_new:       ButtonNewFn,
   action_row:       ActionRowFn,
   alloc_or_oom:     AllocOrOomFn,
   layout:           GpuiLayout,
}

// SAFETY: function pointers and static `.rdata` `Location` pointer in
// `Discord.exe`.
unsafe impl Send for GpuiSymbols {}
unsafe impl Sync for GpuiSymbols {}

static SYMBOLS: OnceLock<GpuiSymbols> = OnceLock::new();

// ---------------------------------------------------------------------------
// Interactive UI state (tabs, filters, collapsible dropdowns, open selects)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
   Plugins,
   Catalog,
   General,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginFilter {
   All,
   Enabled,
   Disabled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogFilter {
   All,
   Available,
   Installed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginSort {
   AlphaAsc,
   AlphaDesc,
   EnabledFirst,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogSort {
   AlphaAsc,
   AlphaDesc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StoreKind {
   Plugins,
   Themes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenPopover {
   None,
   StoreSort,
   StoreFilter,
   PluginsSort,
}

const COMPAT_FULL: u8 = 1 << 0;
const COMPAT_PARTIAL: u8 = 1 << 1;
const COMPAT_MOBILE: u8 = 1 << 2;
const COMPAT_UNVERIFIED: u8 = 1 << 3;
const COMPAT_ALL: u8 = COMPAT_FULL | COMPAT_PARTIAL | COMPAT_MOBILE | COMPAT_UNVERIFIED;

#[derive(Clone)]
struct OpenSelect {
   plugin:    String,
   key:       String,
   anchor:    Option<[f32; 4]>,
   opened_at: Instant,
   closing:   Option<(Instant, f32)>,
}

impl OpenSelect {
   fn is_open_for(&self, plugin: &str, key: &str) -> bool {
      self.closing.is_none() && self.plugin == plugin && self.key == key
   }

   fn progress(&self) -> (f32, bool) {
      if let Some((started, from)) = self.closing {
         let t = (started.elapsed().as_secs_f32() / SELECT_CLOSE_SECS).clamp(0.0, 1.0);
         let val = from * t.mul_add(-t, 1.0);
         (val, t >= 1.0)
      } else {
         let t = (self.opened_at.elapsed().as_secs_f32() / SELECT_ANIM_SECS).clamp(0.0, 1.0);
         let val = 1.0 - (1.0 - t).powi(3);
         (val, false)
      }
   }
}

struct UiState {
   tab:              Tab,
   store_kind:       StoreKind,
   filter:           PluginFilter,
   sort:             PluginSort,
   cat_filter:       CatalogFilter,
   cat_sort:         CatalogSort,
   expanded_plugins: HashSet<String>,
   open_select:      Option<OpenSelect>,
   compat_mask:      u8,
   popover:          OpenPopover,
}

impl Default for UiState {
   fn default() -> Self {
      Self {
         tab:              Tab::Plugins,
         store_kind:       StoreKind::Plugins,
         filter:           PluginFilter::All,
         sort:             PluginSort::AlphaAsc,
         cat_filter:       CatalogFilter::All,
         cat_sort:         CatalogSort::AlphaAsc,
         expanded_plugins: HashSet::new(),
         open_select:      None,
         compat_mask:      COMPAT_FULL | COMPAT_PARTIAL,
         popover:          OpenPopover::None,
      }
   }
}

static UI_STATE: LazyLock<Mutex<UiState>> = LazyLock::new(|| Mutex::new(UiState::default()));

/// Clears any open select dropdown inside the plugin settings view.
pub fn clear_open_select() {
   UI_STATE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .open_select = None;
}

/// Records `SettingsHost`'s GPUI `(id_lo, id_hi)` from `&mut Context<SettingsHost>`
/// on every settings section render so sidebar clicks can switch pages immediately.
pub fn record_settings_entity(cx: *mut c_void) {
   // SAFETY: `cx` is the live `&mut Context<SettingsHost>` passed to the
   // settings section render callback.
   if let Some((lo, hi)) = unsafe { crate::modal::context_entity_id(cx) } {
      SETTINGS_ENTITY.store((u64::from(hi) << 32) | u64::from(lo), Ordering::Release);
   }
}

/// Returns the recorded `SettingsHost` GPUI `(id_lo, id_hi)`.
pub fn settings_entity_id() -> (u32, u32) {
   let ent = SETTINGS_ENTITY.load(Ordering::Acquire);
   #[expect(
      clippy::cast_possible_truncation,
      reason = "low 32 bits hold entity id_lo"
   )]
   let lo = ent as u32;
   (lo, (ent >> 32) as u32)
}

/// Selects which Sunbird page (`0 = Plugins`, `1 = Store`, `2 = Settings`) is
/// active when clicked from the native `Sunbird` sidebar category.
pub fn select_sidebar_tab(idx: usize) {
   let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
   ui.tab = match idx {
      1 => Tab::Catalog,
      2 => Tab::General,
      _ => Tab::Plugins,
   };
   ui.open_select = None;
   ui.popover = OpenPopover::None;
}

/// Returns the active Sunbird sidebar tab index (`0 = Plugins`, `1 = Store`, `2 = Settings`).
pub fn active_sidebar_tab() -> usize {
   let tab = UI_STATE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .tab;
   match tab {
      Tab::Plugins => 0,
      Tab::Catalog => 1,
      Tab::General => 2,
   }
}

#[derive(Clone)]
enum Action {
   SetTab(Tab),
   SetStoreKind(StoreKind),
   SetFilter(PluginFilter),
   SetPluginSort(PluginSort),
   SetCatalogFilter(CatalogFilter),
   SetCatalogSort(CatalogSort),
   OpenPluginSettings(String),
   ToggleSelectDropdown {
      plugin: String,
      key:    String,
   },
   PickSelectOption {
      plugin: String,
      key:    String,
      value:  String,
   },
   CloseSelect,
   TogglePopover(OpenPopover),
   ClosePopover,
   ToggleCompatBit(u8),
   ResetStoreFilters,
   ToggleStaff,
   ReloadAll,
   OpenDevTools,
   CheckUpdates,
   ApplyUpdates,
   RefreshCatalog,
   TogglePlugin(String),
   ReloadPlugin(String),
   EditPluginCode(String),
   EditSetting(SettingField),
   OpenInstallUrl,
   InstallFromFile,
   UninstallPlugin(String),
   ToggleSetting {
      plugin: String,
      key:    String,
   },
   PressButton {
      plugin: String,
      key:    String,
   },
   InstallCatalog(String),
   OpenStoreDetail(Listed),
}

/// Distinguishes actions registered by the plugin settings modal from those
/// registered by the Settings page, so re-rendering one never wipes the other.
const MODAL_ACTION_BIT: u64 = 1 << 62;

static ACTIONS: LazyLock<Mutex<Vec<Action>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static MODAL_SETTINGS_ACTIONS: LazyLock<Mutex<Vec<Action>>> =
   LazyLock::new(|| Mutex::new(Vec::new()));
/// Packed `(id_hi << 32) | id_lo` of `SettingsHost` from `Context<SettingsHost>`,
/// notified via `App::notify` on custom `Div` clicks for instant 1-frame redraws.
static SETTINGS_ENTITY: AtomicU64 = AtomicU64::new(0);
/// Ensures nested clickable `Div`s (like an `Install` pill inside a clickable
/// Store card) only dispatch the innermost hovered target per mouse-down.
static CLICK_CONSUMED: AtomicBool = AtomicBool::new(false);
/// Store plugin names currently downloading/installing in the background.
static INSTALLING: LazyLock<Mutex<HashSet<String>>> =
   LazyLock::new(|| Mutex::new(HashSet::new()));
/// Whether the user explicitly clicked the Store Refresh button, so completion
/// flashes a confirmation checkmark on the button.
static MANUAL_CATALOG_REFRESH: AtomicBool = AtomicBool::new(false);
/// Whether a background reload-all pass is currently running.
static RELOADING_ALL: AtomicBool = AtomicBool::new(false);
/// Individual plugins currently reloading in the background.
static RELOADING_PLUGINS: LazyLock<Mutex<HashSet<String>>> =
   LazyLock::new(|| Mutex::new(HashSet::new()));
/// Short-lived confirmation flashes (element id -> expiry timestamp) so
/// refresh and reload buttons briefly show a crisp green checkmark.
static FLASH_DONE: LazyLock<Mutex<HashMap<String, Instant>>> =
   LazyLock::new(|| Mutex::new(HashMap::new()));
/// Pristine `StyleRefinement::default()` snapshot used as the template for
/// native GPUI `Interactivity::hover_style` allocations.
static DEFAULT_STYLE: OnceLock<Vec<u8>> = OnceLock::new();

const FLASH_DURATION: Duration = Duration::from_millis(1200);
/// Matches Discord's `mh-popout-scale` (`120ms ease-out`) popout open duration.
const SELECT_ANIM_SECS: f32 = 0.120;
const SELECT_CLOSE_SECS: f32 = 0.090;

fn start_select_anim(duration_ms: u64) {
   std::thread::spawn(move || {
      let start = Instant::now();
      while start.elapsed() < Duration::from_millis(duration_ms) {
         std::thread::sleep(Duration::from_millis(16));
         let keep = {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            match ui.open_select.as_ref() {
               Some(sel) if sel.progress().1 => {
                  ui.open_select = None;
                  false
               },
               Some(_) => true,
               None => false,
            }
         };
         crate::modal::force_redraw();
         if !keep {
            return;
         }
      }
      {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         if ui.open_select.as_ref().is_some_and(|s| s.progress().1) {
            ui.open_select = None;
         }
      }
      crate::modal::force_redraw();
   });
}

fn begin_close_select() {
   let start_anim = {
      let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
      if let Some(sel) = ui.open_select.as_mut() {
         if sel.closing.is_none() && sel.anchor.is_some() {
            let (cur, _) = sel.progress();
            sel.closing = Some((Instant::now(), cur));
            true
         } else if sel.anchor.is_none() {
            ui.open_select = None;
            false
         } else {
            false
         }
      } else {
         false
      }
   };
   if start_anim {
      start_select_anim(120);
   }
}

/// `gpui::Hitbox` prefix: `HitboxId(u64)` followed by `Bounds<Pixels>` (`x, y, w, h`).
#[repr(C)]
struct RawHitbox {
   _id:    u64,
   x:      f32,
   y:      f32,
   width:  f32,
   height: f32,
}

unsafe fn hitbox_bounds(hitbox: *const c_void) -> Option<[f32; 4]> {
   if hitbox.is_null() {
      return None;
   }
   // SAFETY: GPUI passes a live `&Hitbox` to every mouse-down listener.
   let hb = unsafe { &*hitbox.cast::<RawHitbox>() };
   let ok = hb.x.is_finite()
      && hb.y.is_finite()
      && hb.width.is_finite()
      && hb.height.is_finite()
      && hb.width > 8.0
      && hb.width < 4000.0
      && hb.height > 8.0
      && hb.height < 1000.0;
   ok.then_some([hb.x, hb.y, hb.width, hb.height])
}

fn flash_done(id: impl Into<String>) {
   let id = id.into();
   let until = Instant::now() + FLASH_DURATION;
   FLASH_DONE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(id, until);
   crate::modal::force_redraw();
   std::thread::spawn(|| {
      std::thread::sleep(FLASH_DURATION + Duration::from_millis(60));
      let now = Instant::now();
      let mut map = FLASH_DONE.lock().unwrap_or_else(PoisonError::into_inner);
      map.retain(|_, exp| *exp > now);
      drop(map);
      crate::modal::force_redraw();
   });
}

fn is_flashed(id: &str) -> bool {
   let now = Instant::now();
   FLASH_DONE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(id)
      .is_some_and(|exp| *exp > now)
}

fn is_plugin_reloading(name: &str) -> bool {
   RELOADING_PLUGINS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .contains(name)
}

fn start_reload_all() {
   if RELOADING_ALL.swap(true, Ordering::AcqRel) {
      return;
   }
   std::thread::spawn(|| {
      phoenix_runtime::reload_all();
      RELOADING_ALL.store(false, Ordering::Release);
      flash_done("sb-reload-all");
   });
}

fn start_reload_plugin(name: String) {
   {
      let mut set = RELOADING_PLUGINS
         .lock()
         .unwrap_or_else(PoisonError::into_inner);
      if !set.insert(name.clone()) {
         return;
      }
   }
   std::thread::spawn(move || {
      phoenix_runtime::reload(&name);
      RELOADING_PLUGINS
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .remove(&name);
      flash_done(format!("sb-reload-{name}"));
   });
}

/// Whether `name` is currently being downloaded and installed from the Store.
pub fn is_store_installing(name: &str) -> bool {
   INSTALLING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .contains(name)
}

/// Starts installing a Store plugin in the background and immediately marks it
/// as installing so cards and detail modals show feedback on the next frame.
pub fn start_store_install(name: String) {
   {
      let mut set = INSTALLING.lock().unwrap_or_else(PoisonError::into_inner);
      if !set.insert(name.clone()) {
         return;
      }
   }
   std::thread::spawn(move || {
      let res = update::install_listed(&name);
      INSTALLING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .remove(&name);
      match res {
         Ok(installed) => {
            log::info!("sunbird settings: {installed}");
            phoenix_ui_desktop::toast(&installed.to_string());
         },
         Err(err) => {
            log::warn!("sunbird settings: install {name}: {err}");
            phoenix_ui_desktop::toast(&format!("Install failed: {err}"));
         },
      }
      crate::modal::force_redraw();
   });
}

fn register_action(action: Action, in_modal: bool) -> u64 {
   if in_modal {
      let mut list = MODAL_SETTINGS_ACTIONS
         .lock()
         .unwrap_or_else(PoisonError::into_inner);
      let id = (list.len() as u64) | MODAL_ACTION_BIT;
      list.push(action);
      id
   } else {
      let mut list = ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
      let id = list.len() as u64;
      list.push(action);
      id
   }
}

fn lookup_action(id: u64) -> Option<Action> {
   let (store, raw) = if (id & MODAL_ACTION_BIT) != 0 {
      (&MODAL_SETTINGS_ACTIONS, id & !MODAL_ACTION_BIT)
   } else {
      (&ACTIONS, id)
   };
   let list = store.lock().unwrap_or_else(PoisonError::into_inner);
   usize::try_from(raw)
      .ok()
      .and_then(|idx| list.get(idx).cloned())
}

/// Rust ABI like the `Fn::call` slot it fills: on Linux `extern "C"` would
/// assume the caller widened `checked`, which Rust callers do not.
unsafe fn switch_callback(
   self_ptr: *const u64,
   checked: u8,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id` inside our `Rc<dyn
      // Fn>`.
      let id = unsafe { *self_ptr };
      let on = checked != 0;
      let Some(action) = lookup_action(id) else {
         return;
      };
      match action {
         Action::ToggleStaff => phoenix_runtime::set_staff_enabled(on),
         Action::TogglePlugin(name) => phoenix_runtime::set_enabled(&name, on),
         Action::ToggleSetting { plugin, key } => {
            phoenix_runtime::set_setting(&plugin, &key, Chosen::Switch(on));
         },
         _ => {},
      }
   });
}

/// Rust ABI mouse-down handler for custom clickable `Div`s.
unsafe fn div_click_callback(
   data: *const u8,
   _event: *const c_void,
   phase: u8,
   hitbox: *const c_void,
   window: *const c_void,
   app: *mut u8,
) {
   let _ = std::panic::catch_unwind(|| {
      if phase == crate::modal::PHASE_CAPTURE {
         CLICK_CONSUMED.store(false, Ordering::Relaxed);
         return;
      }
      if phase != crate::modal::PHASE_BUBBLE || data.is_null() {
         return;
      }
      if !crate::modal::hovered(hitbox, window) {
         return;
      }
      if CLICK_CONSUMED.swap(true, Ordering::AcqRel) {
         return;
      }
      let encoded = data.addr();
      if encoded == 0 {
         return;
      }
      let action_id = (encoded - 1) as u64;
      let in_modal = (action_id & MODAL_ACTION_BIT) != 0;
      if in_modal {
         crate::modal::mark_card_press();
      }
      // SAFETY: `app` is the live `&mut App` passed by GPUI.
      unsafe { crate::modal::stop_propagation(app.cast()) };
      // SAFETY: `hitbox` is the live `&Hitbox` passed by GPUI.
      let bounds = unsafe { hitbox_bounds(hitbox) };
      run_action(action_id, bounds);
      let ent = SETTINGS_ENTITY.load(Ordering::Acquire);
      let notified_host = !in_modal
         && ent != 0
         && unsafe {
            #[expect(
               clippy::cast_possible_truncation,
               reason = "low 32 bits hold entity id_lo"
            )]
            let lo = ent as u32;
            crate::modal::notify_entity(app.cast(), lo, (ent >> 32) as u32)
         };
      if in_modal || !notified_host {
         crate::modal::force_redraw();
      }
   });
}

unsafe extern "C" fn button_callback(
   self_ptr: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id`.
      let id = unsafe { *self_ptr };
      run_action(id, None);
      if (id & MODAL_ACTION_BIT) != 0 {
         crate::modal::force_redraw();
      }
   });
}

#[expect(clippy::too_many_lines, reason = "one arm per callback kind")]
fn run_action(id: u64, bounds: Option<[f32; 4]>) {
   let Some(action) = lookup_action(id) else {
      return;
   };
   match action {
      Action::SetTab(tab) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.tab = tab;
         ui.open_select = None;
         ui.popover = OpenPopover::None;
      },
      Action::SetStoreKind(kind) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.store_kind = kind;
         ui.popover = OpenPopover::None;
      },
      Action::SetFilter(filter) => {
         UI_STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .filter = filter;
      },
      Action::SetPluginSort(sort) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.sort = sort;
         ui.popover = OpenPopover::None;
      },
      Action::SetCatalogFilter(filter) => {
         UI_STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cat_filter = filter;
      },
      Action::SetCatalogSort(sort) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.cat_sort = sort;
         ui.popover = OpenPopover::None;
      },
      Action::OpenPluginSettings(name) => {
         if crate::modal::is_ready() {
            crate::modal::open_plugin_settings(name);
         } else {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            if !ui.expanded_plugins.remove(&name) {
               ui.expanded_plugins.insert(name);
            }
            ui.open_select = None;
         }
      },
      Action::ToggleSelectDropdown { plugin, key } => {
         let closing = {
            let ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            ui.open_select
               .as_ref()
               .is_some_and(|s| s.is_open_for(&plugin, &key))
         };
         if closing {
            begin_close_select();
         } else {
            UI_STATE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .open_select = Some(OpenSelect {
               plugin,
               key,
               anchor: bounds,
               opened_at: Instant::now(),
               closing: None,
            });
            start_select_anim(145);
         }
      },
      Action::PickSelectOption { plugin, key, value } => {
         phoenix_runtime::set_setting(&plugin, &key, Chosen::Text(value));
         begin_close_select();
      },
      Action::CloseSelect => {
         begin_close_select();
      },
      Action::TogglePopover(kind) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.popover = if ui.popover == kind {
            OpenPopover::None
         } else {
            kind
         };
      },
      Action::ClosePopover => {
         UI_STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .popover = OpenPopover::None;
      },
      Action::ToggleCompatBit(bit) => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.compat_mask ^= bit;
      },
      Action::ResetStoreFilters => {
         let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
         ui.compat_mask = COMPAT_ALL;
         ui.cat_filter = CatalogFilter::All;
         ui.popover = OpenPopover::None;
      },
      Action::ReloadAll => start_reload_all(),
      Action::OpenDevTools => phoenix_ui_desktop::open_devtools(),
      Action::CheckUpdates => update::check_now(),
      Action::ApplyUpdates => update::apply(),
      Action::RefreshCatalog => {
         UI_STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .popover = OpenPopover::None;
         if update::refresh_catalog() {
            MANUAL_CATALOG_REFRESH.store(true, Ordering::Release);
         }
      },
      Action::ReloadPlugin(name) => start_reload_plugin(name),
      Action::EditPluginCode(name) => {
         let Some(source) = phoenix_runtime::source(&name) else {
            phoenix_ui_desktop::toast(&format!("Could not read source for {name}"));
            return;
         };
         crate::modal::open_code_editor(name, source);
      },
      Action::OpenInstallUrl => {
         crate::modal::open_install_url();
      },
      Action::EditSetting(field) => {
         crate::modal::open_setting_editor(field);
      },
      Action::InstallFromFile => {
         std::thread::spawn(|| {
            let Some((file_name, bytes)) = phoenix_ui_desktop::pick_js_file() else {
               return;
            };
            match phoenix_runtime::install_file(&file_name, bytes) {
               Ok(installed) => {
                  log::info!("sunbird settings: {installed} from {file_name}");
                  phoenix_ui_desktop::toast(&installed.to_string());
                  crate::modal::force_redraw();
               },
               Err(err) => {
                  log::warn!("sunbird settings: install file {file_name}: {err}");
                  phoenix_ui_desktop::toast(&format!("Install failed: {err}"));
               },
            }
         });
      },
      Action::UninstallPlugin(name) => {
         if let Err(err) = phoenix_runtime::uninstall(&name) {
            log::warn!("sunbird settings: uninstall {name}: {err}");
         }
      },
      Action::PressButton { plugin, key } => phoenix_runtime::press(&plugin, &key),
      Action::InstallCatalog(name) => start_store_install(name),
      Action::OpenStoreDetail(item) => crate::modal::open_store_detail(item),
      _ => {},
   }
}

/// Resolves Discord's native GPUI / Mana widget builders from unique string
/// anchors. Safe to call from `DllMain` (`prepare()`).
pub fn locate(image: &Image<'static>) -> bool {
   if SYMBOLS.get().is_some() {
      return true;
   }

   let found = if crate::X86_CODE {
      desktop::gpui(image).and_then(|gpui| {
         let layout = desktop::gpui_layouts(image, &gpui)?;
         let raw = gpui.div_into_any;
         Ok((gpui, layout, raw))
      })
   } else {
      arm64_builders(image)
   };
   let (
      desktop::Gpui {
         caller_loc,
         div_new,
         div_flex,
         div_flex_col,
         div_style_mut,
         div_child_div,
         div_child_button,
         div_into_any,
         div_push_any,
         str_into_any,
         switch_new,
         switch_into_div,
         button_new,
         action_row,
         alloc_or_oom,
         ..
      },
      found_layout,
      div_into_any_raw,
   ) = match found {
      Ok(found) => found,
      Err(err) => {
         log::warn!("desktop gpui: {err}, leaving the settings page and accessories off");
         return false;
      },
   };

   // SAFETY: the builders were found by their call order and cross-checked,
   // their layouts match `layout`, and they have these Rust-ABI signatures.
   let symbols = unsafe {
      GpuiSymbols {
         caller_loc:       caller_loc as *const c_void,
         div_new:          std::mem::transmute::<usize, DivNewFn>(div_new),
         div_flex:         std::mem::transmute::<usize, DivMutFn>(div_flex),
         div_flex_col:     std::mem::transmute::<usize, DivMutFn>(div_flex_col),
         div_style_mut:    std::mem::transmute::<usize, DivStyleMutFn>(div_style_mut),
         div_child_div:    std::mem::transmute::<usize, DivChildDivFn>(div_child_div),
         div_child_button: std::mem::transmute::<usize, DivChildBtnFn>(div_child_button),
         div_into_any:     std::mem::transmute::<usize, DivIntoAnyFn>(div_into_any),
         div_into_any_raw,
         div_push_any:     div_push_any
            .map(|addr| std::mem::transmute::<usize, DivPushAnyFn>(addr)),
         str_into_any:     str_into_any
            .map(|addr| std::mem::transmute::<usize, StrIntoAnyFn>(addr)),
         switch_new:       std::mem::transmute::<usize, SwitchNewFn>(switch_new),
         switch_into_div:  std::mem::transmute::<usize, SwitchIntoDivFn>(switch_into_div),
         button_new:       std::mem::transmute::<usize, ButtonNewFn>(button_new),
         action_row:       std::mem::transmute::<usize, ActionRowFn>(action_row),
         alloc_or_oom:     std::mem::transmute::<usize, AllocOrOomFn>(alloc_or_oom),
         layout:           found_layout,
      }
   };
   let _ = SYMBOLS.set(symbols);
   log::info!(
      "desktop gpui: resolved native Mana builders (div={div_new:#x}, style={div_style_mut:#x}, \
       switch={switch_new:#x}, button={button_new:#x}, row={action_row:#x}, \
       push_any={div_push_any:#x?}, str_any={str_into_any:#x?})"
   );
   log::info!("desktop gpui: {found_layout:x?}");
   true
}

/// The arm64 builders ([`phoenix_sigs::macos::gpui`]) with every one that
/// returns through `x8` swapped for an `image::sret_caller` thunk, so they
/// are called the x86 way, result slot first, like the types here say.
#[cfg(target_os = "macos")]
fn arm64_builders(image: &Image<'_>) -> Result<(desktop::Gpui, GpuiLayout, usize), String> {
   let gpui = phoenix_sigs::macos::gpui(image)?;
   let layout = phoenix_sigs::macos::gpui_layouts(image, &gpui)?;
   let raw_div_into_any = gpui.div_into_any;
   let thunk = |addr: usize| {
      phoenix_hook::image::sret_caller(addr).ok_or_else(|| "no memory for a thunk".to_owned())
   };
   let thunked = desktop::Gpui {
      div_new: thunk(gpui.div_new)?,
      div_flex: thunk(gpui.div_flex)?,
      div_flex_col: thunk(gpui.div_flex_col)?,
      div_child_div: thunk(gpui.div_child_div)?,
      div_child_button: thunk(gpui.div_child_button)?,
      div_into_any: thunk(gpui.div_into_any)?,
      str_into_any: gpui.str_into_any.map(thunk).transpose()?,
      text_block: thunk(gpui.text_block)?,
      switch_new: thunk(gpui.switch_new)?,
      switch_into_div: thunk(gpui.switch_into_div)?,
      button_new: thunk(gpui.button_new)?,
      action_row: thunk(gpui.action_row)?,
      ..gpui
   };
   Ok((thunked, layout, raw_div_into_any))
}

#[cfg(not(target_os = "macos"))]
fn arm64_builders(_image: &Image<'_>) -> Result<(desktop::Gpui, GpuiLayout, usize), String> {
   Err("no arm64 builders off macOS".to_owned())
}

/// `div.f(..)` for a `Div` builder taking and returning it by value.
fn chain(f: DivMutFn, mut div: RawDiv) -> RawDiv {
   let mut out = RawDiv::zeroed();
   // SAFETY: `f` moves `div` into `out`, both buffers fit a `Div`.
   unsafe { f(&raw mut out, &raw mut div) };
   out
}

/// `size` bytes aligned to `align` from Discord's allocator, once the builders
/// are located.
pub fn host_alloc(size: usize, align: usize) -> Option<*mut u8> {
   // SAFETY: Discord's own `alloc_or_oom`, which aborts rather than return null.
   SYMBOLS.get().map(|sym| unsafe { (sym.alloc_or_oom)(align, size) })
}

/// The `GpuiLayout` decoded from this `Discord.exe`.
pub fn layout() -> Option<&'static GpuiLayout> {
   SYMBOLS.get().map(|sym| &sym.layout)
}

/// Discord's `alloc_or_oom`, once the builders are located.
pub fn alloc_or_oom_addr() -> Option<usize> {
   SYMBOLS.get().map(|sym| sym.alloc_or_oom as usize)
}

/// A `Div`'s `StyleRefinement`, written through the decoded field offsets.
pub struct Style {
   ptr:    *mut u8,
   pub at: &'static StyleOffsets,
}

impl Style {
   fn put<T>(&self, field: usize, value: T) {
      if field + size_of::<T>() <= self.at.bytes {
         // SAFETY: `ptr` is the live `StyleRefinement` of `at.bytes` bytes that
         // `div_style_mut` returned, and the write is inside it.
         unsafe { self.ptr.add(field).cast::<T>().write_unaligned(value) };
      }
   }

   /// A one-byte enum field.
   pub fn set(&self, field: usize, value: u8) {
      self.put(field, value);
   }

   /// An `Option<Length>` or similar 8-byte field, [`gpui_px`] or [`W_FULL`].
   pub fn length(&self, field: usize, value: u64) {
      self.put(field, value);
   }

   pub fn lengths(&self, fields: &[usize], value: u64) {
      for &field in fields {
         self.length(field, value);
      }
   }

   /// `Some(Hsla)`: a `u32` tag, then four `f32`s.
   pub fn hsla(&self, field: usize, hsla: [f32; 4]) {
      if field + 20 <= self.at.bytes {
         self.put(field, 1u32);
         self.put(field + 4, hsla);
      }
   }

   /// `Some(f32)`: a `u32` tag, then the value.
   pub fn float(&self, field: usize, value: f32) {
      if field + 8 <= self.at.bytes {
         self.put(field, 1u32);
         self.put(field + 4, value);
      }
   }

   pub fn shared_string(&self, field: usize, value: SharedString) {
      self.put(field, value);
   }

   /// `Some(Background::Solid(hsla))`: tag `0`, the color at `+8`, and zeroed
   /// gradient stops up to `+0x48`.
   pub fn solid_background(&self, hsla: [f32; 4]) {
      let field = self.at.background;
      if field + 0x48 <= self.at.bytes {
         self.put(field, 0u64);
         self.put(field + 8, hsla);
         self.put(field + 0x18, [0u64; 6]);
      }
   }
}

// ---------------------------------------------------------------------------
// Native GPUI / Mana Layout & Styling Helpers
// ---------------------------------------------------------------------------

struct Builder {
   sym:      &'static GpuiSymbols,
   in_modal: bool,
}

impl Builder {
   const fn page(sym: &'static GpuiSymbols) -> Self {
      Self {
         sym,
         in_modal: false,
      }
   }

   const fn modal(sym: &'static GpuiSymbols) -> Self {
      Self {
         sym,
         in_modal: true,
      }
   }

   #[expect(
      clippy::cast_ptr_alignment,
      reason = "alloc_or_oom returns memory aligned to align_of::<RcInner>()"
   )]
   fn alloc_rc_action(&self, action: Action) -> *mut RcInner {
      let id = register_action(action, self.in_modal);
      // SAFETY: calls Discord's own `alloc_or_oom(align=8, size=24)` so
      // Discord's `Rc::drop` frees it with the matching allocator.
      unsafe {
         let raw = (self.sym.alloc_or_oom)(align_of::<RcInner>(), size_of::<RcInner>());
         let inner = raw.cast::<RcInner>();
         inner.write(RcInner {
            strong:    1,
            weak:      1,
            action_id: id,
         });
         inner
      }
   }

   fn raw_div(&self) -> RawDiv {
      let mut d = RawDiv::zeroed();
      // SAFETY: constructs an empty GPUI `Div` into a buffer that fits it.
      unsafe { (self.sym.div_new)(&raw mut d, self.sym.caller_loc) };
      if DEFAULT_STYLE.get().is_none() {
         // SAFETY: `d` was just constructed with an untouched
         // `StyleRefinement::default()` containing only POD `None` tags.
         let ptr = unsafe { (self.sym.div_style_mut)(&raw mut d) };
         if !ptr.is_null() {
            // SAFETY: `ptr` points to `style.bytes` valid initialized bytes.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, self.sym.layout.style.bytes) };
            let _ = DEFAULT_STYLE.set(bytes.to_vec());
         }
      }
      d
   }

   fn style_div(&self, div: &mut RawDiv, f: impl FnOnce(&Style)) {
      // SAFETY: `div_style_mut` returns `Div`'s boxed `StyleRefinement`.
      let ptr = unsafe { (self.sym.div_style_mut)(&raw mut *div) };
      if !ptr.is_null() {
         f(&Style {
            ptr,
            at: &self.sym.layout.style,
         });
      }
   }

   /// Attaches a native GPUI `Interactivity::hover_style` allocated with
   /// Discord's allocator so GPUI applies hover highlights at 60fps and frees
   /// the box cleanly on drop.
   #[expect(
      clippy::cast_ptr_alignment,
      reason = "read_unaligned/write_unaligned on decoded pointer slot"
   )]
   fn hover_div(&self, div: &mut RawDiv, f: impl FnOnce(&Style)) {
      let Some(hover_off) = self.sym.layout.div.hover_style else {
         return;
      };
      let Some(template) = DEFAULT_STYLE.get() else {
         return;
      };
      let style_bytes = self.sym.layout.style.bytes;
      if template.len() != style_bytes {
         return;
      }
      // SAFETY: writes a freshly allocated `Box<StyleRefinement>` into an
      // empty `Interactivity::hover_style` slot decoded from GPUI.
      unsafe {
         let interactivity = div
            .as_mut_ptr()
            .add(self.sym.layout.div.interactivity)
            .cast::<*mut u8>()
            .read_unaligned();
         if interactivity.is_null() {
            return;
         }
         let slot = interactivity.add(hover_off).cast::<*mut u8>();
         if !slot.read_unaligned().is_null() {
            return;
         }
         let boxed = (self.sym.alloc_or_oom)(8, style_bytes);
         if boxed.is_null() {
            return;
         }
         ptr::copy_nonoverlapping(template.as_ptr(), boxed, style_bytes);
         f(&Style {
            ptr: boxed,
            at:  &self.sym.layout.style,
         });
         slot.write_unaligned(boxed);
      }
   }

   fn v_stack(&self, gap_px: f32) -> RawDiv {
      let d = chain(self.sym.div_flex, self.raw_div());
      let mut d = chain(self.sym.div_flex_col, d);
      self.style_div(&mut d, |s| s.lengths(&s.at.gap, gpui_rem(gap_px)));
      d
   }

   fn h_row(&self, gap_px: f32, space_between: bool) -> RawDiv {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_SPACE_BETWEEN: u8 = 6;
      let mut d = chain(self.sym.div_flex, self.raw_div());
      self.style_div(&mut d, |s| {
         s.lengths(&s.at.gap, gpui_rem(gap_px));
         s.set(s.at.align_items, ALIGN_CENTER);
         if space_between {
            s.set(s.at.justify_content, JUSTIFY_SPACE_BETWEEN);
         }
      });
      d
   }

   /// Rounded bordered card container with a subtle translucent fill that
   /// adapts across Onyx, Dark, and Light Discord themes.
   fn surface_card(&self, gap_px: f32, pad_px: f32, radius_px: f32) -> RawDiv {
      let mut d = self.v_stack(gap_px);
      self.style_div(&mut d, |s| {
         s.length(s.at.size[0], W_FULL);
         s.solid_background(UI_SURFACE);
         s.lengths(&s.at.border_widths, gpui_px(1.0));
         s.lengths(&s.at.corner_radii, gpui_rem(radius_px));
         s.lengths(&s.at.padding, gpui_rem(pad_px));
         s.hsla(s.at.border_color, UI_BORDER);
      });
      d
   }

   /// Attaches a static `ElementId` (for 60fps GPUI hover tracking), a
   /// pointing-hand cursor, and a zero-allocation mouse-down click handler.
   fn clickable(&self, div: RawDiv, id: &str, action: Action) -> RawDiv {
      const CURSOR_POINTING_HAND: u8 = 5;
      let mut div = crate::modal::set_div_id(div, intern(id));
      self.style_div(&mut div, |s| s.set(s.at.mouse_cursor, CURSOR_POINTING_HAND));
      let action_id = register_action(action, self.in_modal);
      let data = usize::try_from(action_id + 1).map_or(
         ptr::dangling_mut::<u8>(),
         ptr::without_provenance_mut::<u8>,
      );
      crate::modal::listen_mouse_down(&mut div, data, &DIV_CLICK_VTBL);
      div
   }

   /// Standalone native Mana toggle switch without label/description text.
   fn toggle_switch(&self, id: &str, checked: bool, action: Action) -> RawDiv {
      let at = &self.sym.layout.switch;
      let id_static = intern(id);
      let mut sw = RawSwitch::zeroed();
      // SAFETY: creates a native Mana `Switch` in a buffer that fits it.
      unsafe {
         (self.sym.switch_new)(
            &raw mut sw,
            id_static.as_ptr(),
            id_static.len(),
            u8::from(checked),
         );
      }
      sw.put(at.on_change, self.alloc_rc_action(action));
      sw.put(at.on_change + 8, &raw const SWITCH_FN_VTBL);

      let mut out = RawDiv::zeroed();
      // SAFETY: moves the `Switch` into its `Div`.
      unsafe { (self.sym.switch_into_div)(&raw mut out, &raw mut sw) };
      self.style_div(&mut out, |s| s.float(s.at.flex_shrink, 0.0));
      out
   }

   /// Compact SVG icon element tinted to `color` (scaled in `rem`), or
   /// `fallback` text if SVG builders are unavailable.
   fn icon_or_text(
      &self,
      svg_src: &'static str,
      slot: &'static OnceLock<Option<&'static str>>,
      fallback: &str,
      base_px: f32,
      color: [f32; 4],
   ) -> Option<RawDiv> {
      if let Some(svg) = SVG.get()
         && let Some(path) = *slot.get_or_init(|| crate::icons::register(svg_src))
      {
         let mut wrap = self.h_row(0.0, false);
         let mut el = svg_icon_len(self, svg, path, gpui_rem(base_px), color);
         if self.push_any(&mut wrap, &mut el) {
            return Some(wrap);
         }
      }
      self.ui_text_line(fallback, color, base_px - 1.0)
   }

   /// Compact 30x30 ghost icon button for inline plugin row actions (gear,
   /// code, reload, trash).
   #[expect(clippy::too_many_arguments, reason = "icon button layout and action")]
   fn row_icon_btn(
      &self,
      id: &str,
      svg_src: &'static str,
      slot: &'static OnceLock<Option<&'static str>>,
      fallback: &str,
      color: [f32; 4],
      highlight: Option<[f32; 4]>,
      action: Action,
   ) -> RawDiv {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_CENTER: u8 = 4;
      let mut btn = self.h_row(0.0, false);
      self.style_div(&mut btn, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.size[0], gpui_rem(ROW_ICON_SIZE));
         s.length(s.at.size[1], gpui_rem(ROW_ICON_SIZE));
         s.set(s.at.align_items, ALIGN_CENTER);
         s.set(s.at.justify_content, JUSTIFY_CENTER);
         s.lengths(&s.at.corner_radii, gpui_rem(7.0));
         if let Some(bg) = highlight {
            s.solid_background(bg);
         }
      });
      if let Some(icon) = self.icon_or_text(svg_src, slot, fallback, 16.5, color) {
         btn = self.push_div(btn, icon);
      }
      let is_danger = color[0] > 0.90;
      let hover_bg = if is_danger {
         UI_DANGER_HOVER
      } else {
         UI_ROW_HOVER
      };
      self.hover_div(&mut btn, |s| {
         s.solid_background(hover_bg);
      });
      self.clickable(btn, id, action)
   }

   /// Compact clickable button/chip (`28rem` tall, `6rem` radius) for card
   /// actions (`Install`) and inline select option pickers.
   #[expect(clippy::too_many_arguments, reason = "style parameters for compact chips")]
   fn chip(
      &self,
      id: &str,
      label: &str,
      icon: Option<IconSpec>,
      text_color: [f32; 4],
      bg: Option<[f32; 4]>,
      border: Option<[f32; 4]>,
      action: Action,
   ) -> RawDiv {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_CENTER: u8 = 4;
      let mut d = self.h_row(5.0, false);
      self.style_div(&mut d, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.size[1], gpui_rem(COMPACT_HEIGHT));
         s.set(s.at.align_items, ALIGN_CENTER);
         s.set(s.at.justify_content, JUSTIFY_CENTER);
         s.lengths(&s.at.corner_radii, gpui_rem(COMPACT_RADIUS));
         s.length(s.at.padding[1], gpui_rem(11.0));
         s.length(s.at.padding[3], gpui_rem(11.0));
         if let Some(bg) = bg {
            s.solid_background(bg);
         }
         if let Some(border) = border {
            s.lengths(&s.at.border_widths, gpui_px(1.0));
            s.hsla(s.at.border_color, border);
         }
      });
      if let Some((svg_src, slot, fallback)) = icon
         && let Some(ic) = self.icon_or_text(svg_src, slot, fallback, 14.0, text_color)
      {
         d = self.push_div(d, ic);
      }
      if let Some(txt) = self.ui_text_line(label, text_color, 13.0) {
         d = self.push_div(d, txt);
      }
      let is_primary = bg.is_some_and(|c| c[1] > 0.50);
      let hover_bg = if is_primary {
         UI_BLURPLE_HOVER
      } else {
         UI_CONTROL_HOVER
      };
      self.hover_div(&mut d, |s| {
         s.solid_background(hover_bg);
         if border.is_some() {
            s.hsla(s.at.border_color, UI_CONTROL_BORDER_HOVER);
         }
      });
      self.clickable(d, id, action)
   }

   /// Subtle uppercase section label above a grouped card.
   fn section_label(&self, text: &str) -> RawDiv {
      let mut wrap = self.h_row(6.0, false);
      self.style_div(&mut wrap, |s| {
         s.length(s.at.padding[0], gpui_rem(4.0));
         s.length(s.at.padding[3], gpui_rem(2.0));
      });
      if let Some(line) = self.ui_text_line(text, UI_MUTED, 12.5) {
         wrap = self.push_div(wrap, line);
      }
      wrap
   }

   /// Subtle 1px horizontal divider line between sections or rows.
   fn divider(&self) -> RawDiv {
      let mut d = self.raw_div();
      self.style_div(&mut d, |s| {
         s.length(s.at.size[0], W_FULL);
         s.length(s.at.border_widths[2], gpui_px(1.0));
         s.length(s.at.size[1], gpui_px(1.0));
         s.hsla(s.at.border_color, UI_DIVIDER);
      });
      d
   }

   fn push_div(&self, mut parent: RawDiv, mut child: RawDiv) -> RawDiv {
      let mut out = RawDiv::zeroed();
      // SAFETY: moves both `RawDiv`s into `div_child_div`.
      unsafe { (self.sym.div_child_div)(&raw mut out, &raw mut parent, &raw mut child) };
      out
   }

   fn build_button(
      &self,
      id: &str,
      label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawButton {
      let rc_ptr = self.alloc_rc_action(action);
      new_button(
         self.sym,
         id,
         label,
         variant,
         size,
         &raw const BUTTON_FN_VTBL,
         rc_ptr,
      )
   }

   fn switch_row(
      &self,
      id: &str,
      label: &str,
      description: Option<&str>,
      checked: bool,
      action: Action,
   ) -> RawDiv {
      let at = &self.sym.layout.switch;
      let id_static = intern(id);
      let mut sw = RawSwitch::zeroed();
      // SAFETY: creates a native Mana `Switch` in a buffer that fits it.
      unsafe {
         (self.sym.switch_new)(
            &raw mut sw,
            id_static.as_ptr(),
            id_static.len(),
            u8::from(checked),
         );
      }
      // The defaults these overwrite are static `SharedString`s and `None`,
      // which own nothing.
      sw.put(at.label, SharedString::from_str(label));
      if let Some(desc) = description
         && !desc.is_empty()
      {
         sw.put(at.description, SharedString::from_str(desc));
      }
      sw.put(at.on_change, self.alloc_rc_action(action));
      sw.put(at.on_change + 8, &raw const SWITCH_FN_VTBL);

      let mut out = RawDiv::zeroed();
      // SAFETY: moves the `Switch` into its `Div`.
      unsafe { (self.sym.switch_into_div)(&raw mut out, &raw mut sw) };
      out
   }

   #[expect(clippy::too_many_arguments, reason = "one per GPUI row part")]
   fn action_row(
      &self,
      id: &str,
      title: &str,
      subtitle: &str,
      button_label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawDiv {
      let title_ss = SharedString::from_str(title);
      let sub_ss = SharedString::from_str(subtitle);
      let mut btn = self.build_button(id, button_label, variant, size, action);
      let mut out = RawDiv::zeroed();
      // SAFETY: builds Discord's native `ActionRow(title, subtitle, button)`.
      unsafe {
         (self.sym.action_row)(
            &raw mut out,
            &raw const title_ss,
            &raw const sub_ss,
            &raw mut btn,
         );
      }
      out
   }

   fn write_any(&self, mut div: RawDiv, sret: *mut RawAnyElement) {
      // SAFETY: writes the 32-byte `gpui::AnyElement` directly to `sret`.
      unsafe {
         (self.sym.div_into_any)(sret, &raw mut div);
      }
   }

   fn push_any(&self, parent: &mut RawDiv, child: &mut RawAnyElement) -> bool {
      let Some(push) = self.sym.div_push_any else {
         return false;
      };
      // SAFETY: `children` is the `Div`'s `SmallVec<[AnyElement; 2]>`, decoded
      // from `div_child_div` and inside the `Div`.
      unsafe {
         let children = parent
            .as_mut_ptr()
            .add(self.sym.layout.div.children)
            .cast::<c_void>();
         push(children, child);
      }
      true
   }

   fn text_line(&self, text: &str, color_hsla: [f32; 4], font_px: f32) -> Option<RawDiv> {
      self.styled_text_line(
         text,
         color_hsla,
         gpui_px(font_px),
         gpui_px(font_px + 4.0),
      )
   }

   fn ui_text_line(&self, text: &str, color_hsla: [f32; 4], base_px: f32) -> Option<RawDiv> {
      self.styled_text_line(
         text,
         color_hsla,
         gpui_rem(base_px),
         gpui_rem(base_px * 1.35),
      )
   }

   fn styled_text_line(
      &self,
      text: &str,
      color_hsla: [f32; 4],
      font_size: u64,
      line_height: u64,
   ) -> Option<RawDiv> {
      let str_into_any = self.sym.str_into_any?;
      let mut line_div = self.raw_div();
      self.style_div(&mut line_div, |s| {
         s.length(s.at.font_size, font_size);
         s.length(s.at.line_height, line_height);
         s.hsla(s.at.text_color, color_hsla);
      });
      let mut ss = SharedString::from_str(text);
      // SAFETY: moves the `SharedString` into a 32-byte `AnyElement` and
      // pushes it.
      let mut any = unsafe {
         let mut out = MaybeUninit::<RawAnyElement>::uninit();
         str_into_any(out.as_mut_ptr(), &raw mut ss);
         out.assume_init()
      };
      if !self.push_any(&mut line_div, &mut any) {
         return None;
      }
      Some(line_div)
   }
}

/// Returns the resolved address of `Div::into_any_element` for sibling desktop
/// locators (`crate::modal`).
pub fn div_into_any_addr() -> Option<usize> {
   SYMBOLS.get().map(|s| s.div_into_any_raw)
}

pub fn raw_div() -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder::page(sym).raw_div())
}

pub fn style_div(div: &mut RawDiv, f: impl FnOnce(&Style)) {
   if let Some(sym) = SYMBOLS.get() {
      Builder::page(sym).style_div(div, f);
   }
}

pub fn v_stack(gap_px: f32) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder::page(sym).v_stack(gap_px))
}

pub fn h_row(gap_px: f32, space_between: bool) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   Some(Builder::page(sym).h_row(gap_px, space_between))
}

pub fn push_div(parent: RawDiv, child: RawDiv) -> RawDiv {
   let Some(sym) = SYMBOLS.get() else {
      return parent;
   };
   Builder::page(sym).push_div(parent, child)
}

pub fn push_any(parent: &mut RawDiv, child: &mut RawAnyElement) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   Builder::page(sym).push_any(parent, child)
}

pub fn str_to_any(text: &str) -> Option<RawAnyElement> {
   let str_into_any = SYMBOLS.get()?.str_into_any?;
   let mut ss = SharedString::from_str(text);
   // SAFETY: moves the `SharedString` into a 32-byte `AnyElement`.
   unsafe {
      let mut out = MaybeUninit::<RawAnyElement>::uninit();
      str_into_any(out.as_mut_ptr(), &raw mut ss);
      Some(out.assume_init())
   }
}

pub fn build_raw_button(
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   vtbl: *const RcFnVtbl,
   action_id: u64,
) -> Option<RawButton> {
   let sym = SYMBOLS.get()?;
   let rc_ptr = alloc_rc_u64(action_id)?;
   Some(new_button(sym, id, label, variant, size, vtbl, rc_ptr))
}

/// A native Mana `Button` with our `variant`, `size` and `Rc<dyn Fn>` click
/// handler.
fn new_button(
   sym: &GpuiSymbols,
   id: &str,
   label: &str,
   variant: BtnVariant,
   size: BtnSize,
   vtbl: *const RcFnVtbl,
   rc_ptr: *mut RcInner,
) -> RawButton {
   let at = &sym.layout.button;
   let id_static = intern(id);
   let label_ss = SharedString::from_str(label);
   let mut btn = RawButton::zeroed();
   // SAFETY: constructs a native Mana `Button` in a buffer that fits it.
   unsafe {
      (sym.button_new)(
         &raw mut btn,
         id_static.as_ptr(),
         id_static.len(),
         &raw const label_ss,
      );
   }
   btn.put(at.variant, variant as u8);
   btn.put(at.scale, size as u8);
   btn.put(at.on_click, rc_ptr);
   btn.put(at.on_click + 8, vtbl);
   btn
}

pub fn push_raw_button(mut parent: RawDiv, mut btn: RawButton) -> RawDiv {
   let Some(sym) = SYMBOLS.get() else {
      return parent;
   };
   let mut out = RawDiv::zeroed();
   // SAFETY: appends native Mana `Button` directly into `parent`.
   unsafe { (sym.div_child_button)(&raw mut out, &raw mut parent, &raw mut btn) };
   out
}

pub fn write_any(div: RawDiv, sret: *mut RawAnyElement) {
   if let Some(sym) = SYMBOLS.get() {
      Builder::page(sym).write_any(div, sret);
   }
}

/// Converts 32-bit ARGB (`0xAARRGGBB`) into GPUI's `[h, s, l, a]`
/// (`0.0..=1.0`).
#[expect(clippy::many_single_char_names, reason = "the HSL formula's own names")]
pub fn argb_to_hsla(argb: u32) -> [f32; 4] {
   let [alpha, r, g, b] = argb.to_be_bytes().map(|byte| f32::from(byte) / 255.0);
   let a = if alpha == 0.0 && argb != 0 {
      1.0
   } else {
      alpha
   };

   let max = r.max(g).max(b);
   let min = r.min(g).min(b);
   let delta = max - min;
   let l = f32::midpoint(max, min);

   let (h, s) = if delta == 0.0 {
      (0.0, 0.0)
   } else {
      let s = delta / (1.0 - 2.0f32.mul_add(l, -1.0).abs());
      let mut h = if (max - r).abs() < f32::EPSILON {
         ((g - b) / delta) % 6.0
      } else if (max - g).abs() < f32::EPSILON {
         (b - r) / delta + 2.0
      } else {
         (r - g) / delta + 4.0
      };
      h /= 6.0;
      if h < 0.0 {
         h += 1.0;
      }
      (h, s)
   };
   [h, s, l, a]
}

/// Allocates an `RcInner { strong: 1, weak: 1, action_id: value }` on
/// `Discord.exe`'s own heap so Discord's `Rc::drop` frees it cleanly.
pub fn alloc_rc_u64(value: u64) -> Option<*mut RcInner> {
   let sym = SYMBOLS.get()?;
   // SAFETY: uses Discord's resolved `alloc_or_oom(align, size)` for a 24-byte
   // `RcInner` layout matching `std::rc::RcBox<u64>`.
   unsafe {
      #[expect(
         clippy::cast_ptr_alignment,
         reason = "allocated with `RcInner`'s alignment"
      )]
      let ptr = (sym.alloc_or_oom)(align_of::<RcInner>(), size_of::<RcInner>()).cast::<RcInner>();
      if ptr.is_null() {
         return None;
      }
      ptr.write(RcInner {
         strong:    1,
         weak:      1,
         action_id: value,
      });
      Some(ptr)
   }
}

/// Whether `div_push_any` resolved, which every native modal needs to take
/// Discord's own `Modal` element as a child.
#[must_use]
pub fn supports_push_any() -> bool {
   SYMBOLS.get().is_some_and(|s| s.div_push_any.is_some())
}

/// Whether `div_push_any` and `str_into_any` resolved for native message
/// accessories.
#[must_use]
pub fn supports_accessories() -> bool {
   SYMBOLS
      .get()
      .is_some_and(|s| s.div_push_any.is_some() && s.str_into_any.is_some())
}

/// Wraps a rendered message row `RawAnyElement` in place with native GPUI
/// accessory lines underneath and an optional tinted left accent bar.
pub fn wrap_with_accessories(
   sret: *mut RawAnyElement,
   lines: &[(String, Option<u32>)],
   tint: Option<u32>,
) -> bool {
   const MUTED: [f32; 4] = [0.60, 0.08, 0.62, 0.85];

   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   if sym.div_push_any.is_none() || sym.str_into_any.is_none() {
      return false;
   }
   let b = Builder::page(sym);

   // SAFETY: takes ownership of the original rendered `AnyElement` out of
   // `sret`; we write the replacement `AnyElement` back into `sret` before
   // returning.
   let mut orig = unsafe { ptr::read(sret) };

   let mut outer = b.v_stack(2.0);
   if let Some(argb) = tint {
      let hsla = argb_to_hsla(argb);
      // The text color is inherited by child text runs.
      b.style_div(&mut outer, |s| {
         s.hsla(s.at.text_color, hsla);
         s.length(s.at.border_widths[3], gpui_px(3.0));
         s.hsla(s.at.border_color, hsla);
         // A faint wash of the tint behind the whole row.
         let [h, sat, l, _] = hsla;
         s.solid_background([h, sat, l, 0.12]);
      });
   }
   if !b.push_any(&mut outer, &mut orig) {
      // SAFETY: restore original element if push failed.
      unsafe { ptr::write(sret, orig) };
      return false;
   }

   if !lines.is_empty() {
      let mut acc_box = b.v_stack(2.0);
      // Indent accessories under the message text column.
      b.style_div(&mut acc_box, |s| {
         let [_, right, bottom, left] = s.at.padding;
         s.length(right, gpui_px(16.0));
         s.length(bottom, gpui_px(4.0));
         s.length(left, gpui_px(72.0));
      });
      for (text, color) in lines {
         let hsla = color.map_or(MUTED, argb_to_hsla);
         for subline in text.lines() {
            if subline.is_empty() {
               continue;
            }
            if let Some(line_div) = b.text_line(subline, hsla, 12.0) {
               acc_box = b.push_div(acc_box, line_div);
            }
         }
      }
      outer = b.push_div(outer, acc_box);
   }

   b.write_any(outer, sret);
   true
}

/// Muted accessory text, for lines without a color of their own.
const ACCESSORY_MUTED: [f32; 4] = [0.60, 0.08, 0.62, 0.85];

/// One piece of an inline accessory: SVG icons when it has them and the build
/// can draw them, else its text.
#[derive(Clone, Debug)]
pub struct Piece {
   pub text:  String,
   pub color: Option<u32>,
   /// Asset paths from [`crate::icons::register`], each with its own color.
   pub icons: Vec<(&'static str, Option<u32>)>,
}

/// GPUI's `Svg` builders and layout, resolved by [`locate_svg`].
struct SvgSymbols {
   new:       SvgNewFn,
   path:      SvgPathFn,
   into_any:  SvgIntoAnyFn,
   style_box: usize,
}

type RawSvg = Scratch<{ layout::SVG_CAP }>;
type SvgNewFn = unsafe extern "C" fn(*mut RawSvg, *const c_void) -> *mut RawSvg;
type SvgPathFn = unsafe extern "C" fn(*mut RawSvg, *mut RawSvg, *const u8, usize) -> *mut RawSvg;
type SvgIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawSvg) -> *mut RawAnyElement;

static SVG: OnceLock<SvgSymbols> = OnceLock::new();

/// Resolves the `Svg` builders, after [`locate`].
pub fn locate_svg(image: &Image<'static>) {
   let located = if crate::X86_CODE {
      desktop::svg_builders(image)
   } else {
      arm64_svg_builders(image)
   };
   let found = match located {
      Ok(found) => found,
      Err(err) => {
         log::warn!("desktop gpui: {err}, icons fall back to text");
         return;
      },
   };
   if found.size > layout::SVG_CAP {
      log::warn!(
         "desktop gpui: Svg is {:#x} bytes, over {:#x}",
         found.size,
         layout::SVG_CAP
      );
      return;
   }
   // SAFETY: decoded from the code drawing a bundled icon, which calls them
   // with these Rust-ABI signatures.
   let symbols = unsafe {
      SvgSymbols {
         new:       std::mem::transmute::<usize, SvgNewFn>(found.new),
         path:      std::mem::transmute::<usize, SvgPathFn>(found.path),
         into_any:  std::mem::transmute::<usize, SvgIntoAnyFn>(found.into_any),
         style_box: found.style_box,
      }
   };
   let _ = SVG.set(symbols);
   log::info!("desktop gpui: resolved Svg builders {found:x?}");
}

#[cfg(target_os = "macos")]
fn arm64_svg_builders(image: &Image<'_>) -> Result<desktop::SvgBuilders, String> {
   let svg = phoenix_sigs::macos::svg_builders(image)?;
   let thunk = |addr: usize| {
      phoenix_hook::image::sret_caller(addr)
         .ok_or_else(|| "no memory for an Svg thunk".to_owned())
   };
   Ok(desktop::SvgBuilders {
      new: thunk(svg.new)?,
      path: thunk(svg.path)?,
      into_any: thunk(svg.into_any)?,
      ..svg
   })
}

#[cfg(not(target_os = "macos"))]
fn arm64_svg_builders(_image: &Image<'_>) -> Result<desktop::SvgBuilders, String> {
   Err("no arm64 Svg builders off macOS".to_owned())
}

/// Whether icons can be drawn.
pub fn svg_ready() -> bool {
   SVG.get().is_some() && SYMBOLS.get().is_some()
}

/// `len` bytes from Discord's own allocator, so Discord can free them.
pub fn discord_alloc(len: usize) -> Option<*mut u8> {
   let sym = SYMBOLS.get()?;
   // SAFETY: Discord's `alloc_or_oom(align, size)`, which aborts rather than
   // returning null.
   let ptr = unsafe { (sym.alloc_or_oom)(1, len.max(1)) };
   (!ptr.is_null()).then_some(ptr)
}

/// An `svg().path(path)` of `px` pixels filled with `color`, as an
/// `AnyElement`.
fn svg_icon(
   b: &Builder,
   svg: &SvgSymbols,
   path: &'static str,
   px: f32,
   color: [f32; 4],
) -> RawAnyElement {
   svg_icon_len(b, svg, path, gpui_px(px), color)
}

fn svg_icon_len(
   b: &Builder,
   svg: &SvgSymbols,
   path: &'static str,
   size_len: u64,
   color: [f32; 4],
) -> RawAnyElement {
   let mut fresh = RawSvg::zeroed();
   let mut icon = RawSvg::zeroed();
   // SAFETY: `svg()` builds into a buffer that fits an `Svg`, and `path` moves
   // it into the second with a `'static` path.
   unsafe {
      (svg.new)(&raw mut fresh, b.sym.caller_loc);
      (svg.path)(&raw mut icon, &raw mut fresh, path.as_ptr(), path.len());
   }
   // SAFETY: the `Svg`'s boxed `StyleRefinement`, at the decoded offset.
   let style = unsafe {
      icon
         .as_mut_ptr()
         .add(svg.style_box)
         .cast::<*mut u8>()
         .read_unaligned()
   };
   if !style.is_null() {
      let s = Style {
         ptr: style,
         at:  &b.sym.layout.style,
      };
      s.lengths(&s.at.size, size_len);
      s.float(s.at.flex_shrink, 0.0);
      s.hsla(s.at.text_color, color);
   }
   let mut any = MaybeUninit::<RawAnyElement>::uninit();
   // SAFETY: moves the `Svg` into a 32-byte `AnyElement`.
   unsafe {
      (svg.into_any)(any.as_mut_ptr(), &raw mut icon);
      any.assume_init()
   }
}

/// `pieces` side by side in a row: each piece's icons when it has any and
/// they can be drawn, else its text.
fn inline_row(b: &Builder, pieces: &[Piece], font_px: f32) -> RawDiv {
   let mut row = b.h_row(4.0, false);
   for piece in pieces {
      let hsla = piece.color.map_or(ACCESSORY_MUTED, argb_to_hsla);
      if let Some(svg) = SVG.get().filter(|_| !piece.icons.is_empty()) {
         let mut icons = b.h_row(2.0, false);
         for &(path, color) in &piece.icons {
            let mut icon = svg_icon(
               b,
               svg,
               path,
               font_px + 3.0,
               color.map_or(hsla, argb_to_hsla),
            );
            b.push_any(&mut icons, &mut icon);
         }
         row = b.push_div(row, icons);
      } else if !piece.text.is_empty()
         && let Some(line) = b.text_line(&piece.text, hsla, font_px)
      {
         row = b.push_div(row, line);
      }
   }
   row
}

/// Adds `pieces` as one more child of a `Div` Discord built by value, like the
/// member name's row right after the name, `lead` pixels after the child
/// before them.
pub fn append_pieces(div: *mut c_void, pieces: &[Piece], lead: f32) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let Some(push) = sym.div_push_any else {
      return false;
   };
   if div.is_null() || sym.str_into_any.is_none() {
      return false;
   }
   let b = Builder::page(sym);
   let mut row = inline_row(&b, pieces, 13.0);
   if lead > 0.0 {
      b.style_div(&mut row, |s| s.length(s.at.padding[3], gpui_px(lead)));
   }
   let mut any = MaybeUninit::<RawAnyElement>::uninit();
   b.write_any(row, any.as_mut_ptr());
   // SAFETY: `div` is a live `Div` that `Div::child` just returned, whose
   // children sit at the decoded offset; `push` moves the element into them.
   unsafe {
      let children = div
         .cast::<u8>()
         .add(sym.layout.div.children)
         .cast::<c_void>();
      push(children, any.as_mut_ptr());
   }
   true
}

// ---------------------------------------------------------------------------
// Sunbird Native GPUI Settings Page: Unified Discord Design Tokens
// ---------------------------------------------------------------------------

// Typography
const UI_TEXT: [f32; 4] = [0.60, 0.05, 0.95, 1.0];
const UI_MUTED: [f32; 4] = [0.60, 0.06, 0.68, 1.0];
const UI_DIM: [f32; 4] = [0.60, 0.06, 0.54, 1.0];

// Surfaces & cards
const UI_SURFACE: [f32; 4] = [0.62, 0.05, 0.55, 0.055];
const UI_SURFACE_HOVER: [f32; 4] = [0.62, 0.05, 0.55, 0.095];
const UI_BORDER: [f32; 4] = [0.62, 0.05, 0.55, 0.16];
const UI_BORDER_HOVER: [f32; 4] = [0.62, 0.05, 0.55, 0.28];
const UI_DIVIDER: [f32; 4] = [0.62, 0.05, 0.55, 0.12];

// Interactive controls (toolbar buttons, segmented tracks, floating popovers)
const UI_CONTROL_BG: [f32; 4] = [0.62, 0.04, 0.095, 1.0];
const UI_CONTROL_HOVER: [f32; 4] = [0.62, 0.05, 0.15, 1.0];
const UI_CONTROL_BORDER: [f32; 4] = [0.62, 0.05, 0.28, 0.50];
const UI_CONTROL_BORDER_HOVER: [f32; 4] = [0.62, 0.05, 0.38, 0.65];
const UI_SEG_ACTIVE: [f32; 4] = [0.62, 0.05, 0.20, 1.0];
const UI_SEG_ACTIVE_HOVER: [f32; 4] = [0.62, 0.05, 0.24, 1.0];
const UI_POPOVER_BG: [f32; 4] = [0.62, 0.04, 0.095, 1.0];
const UI_ROW_HOVER: [f32; 4] = [0.62, 0.05, 0.55, 0.12];
const UI_RADIO_RING: [f32; 4] = [0.60, 0.04, 0.48, 0.65];
const UI_WHITE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

// Semantic accents
const UI_BLURPLE: [f32; 4] = [0.65, 0.86, 0.65, 1.0];
const UI_BLURPLE_HOVER: [f32; 4] = [0.65, 0.86, 0.57, 1.0];
const UI_BLURPLE_SOFT: [f32; 4] = [0.65, 0.86, 0.65, 0.18];
const UI_BLURPLE_TEXT: [f32; 4] = [0.65, 0.95, 0.78, 1.0];
const UI_BLURPLE_BORDER: [f32; 4] = [0.65, 0.86, 0.65, 0.55];
const UI_SUCCESS: [f32; 4] = [0.40, 0.65, 0.50, 1.0];
const UI_SUCCESS_BG: [f32; 4] = [0.40, 0.65, 0.45, 0.16];
const UI_SUCCESS_BORDER: [f32; 4] = [0.40, 0.65, 0.50, 0.48];
const UI_DANGER: [f32; 4] = [0.99, 0.87, 0.60, 1.0];
const UI_DANGER_HOVER: [f32; 4] = [0.99, 0.87, 0.55, 0.18];
const UI_WARN: [f32; 4] = [0.11, 0.90, 0.70, 1.0];
const UI_WARN_BG: [f32; 4] = [0.10, 0.70, 0.45, 0.08];
const UI_WARN_BORDER: [f32; 4] = [0.10, 0.72, 0.50, 0.34];

// Shared geometry scale (in base px, encoded as `gpui_rem`)
const CTRL_HEIGHT: f32 = 34.0;
const CTRL_RADIUS: f32 = 8.0;
const COMPACT_HEIGHT: f32 = 28.0;
const COMPACT_RADIUS: f32 = 6.0;
const CARD_RADIUS: f32 = 10.0;
const POPOVER_RADIUS: f32 = 12.0;
const ROW_ICON_SIZE: f32 = 30.0;
const SEARCH_WIDTH: f32 = 180.0;

const FLEX_WRAP: u8 = 1;
const MONO_FONT: &str = "gg sans mono";

type IconSpec = (&'static str, &'static OnceLock<Option<&'static str>>, &'static str);

#[derive(Clone, Copy, PartialEq, Eq)]
enum IconBtnState {
   Idle,
   Open,
   Active,
   Done,
}

// Crisp 24x24 filled vector icons rendered via GPUI's native `Svg` element.
const SVG_GEAR: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M19.14 12.94c.04-.31.06-.63.06-.94s-.02-.63-.06-.94l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.1 7.1 0 0 0-1.63-.94l-.36-2.54a.48.48 0 0 0-.48-.41h-3.84a.48.48 0 0 0-.48.41l-.36 2.54c-.58.24-1.13.56-1.63.94l-2.39-.96a.5.5 0 0 0-.6.22L2.71 8.84a.5.5 0 0 0 .12.64l2.03 1.58c-.04.31-.06.63-.06.94s.02.63.06.94l-2.03 1.58a.5.5 0 0 0-.12.64l1.92 3.32a.5.5 0 0 0 .6.22l2.39-.96c.5.38 1.05.7 1.63.94l.36 2.54c.04.24.24.41.48.41h3.84c.24 0 .44-.17.48-.41l.36-2.54c.58-.24 1.13-.56 1.63-.94l2.39.96a.5.5 0 0 0 .6-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58ZM12 15.6A3.6 3.6 0 1 1 12 8.4a3.6 3.6 0 0 1 0 7.2Z"/></svg>"#;
const SVG_CODE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M8.7 16.3a1 1 0 0 1-1.4 0l-3.6-3.6a1 1 0 0 1 0-1.4l3.6-3.6a1 1 0 0 1 1.4 1.4L5.8 12l2.9 2.9a1 1 0 0 1 0 1.4Zm6.6 0a1 1 0 0 1 0-1.4l2.9-2.9-2.9-2.9a1 1 0 1 1 1.4-1.4l3.6 3.6a1 1 0 0 1 0 1.4l-3.6 3.6a1 1 0 0 1-1.4 0Z"/></svg>"#;
const SVG_RELOAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M17.65 6.35A7.95 7.95 0 0 0 12 4a8 8 0 1 0 7.75 10h-2.1A6 6 0 1 1 12 6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35Z"/></svg>"#;
const SVG_TRASH: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M9 3h6l1 2h4v2H4V5h4l1-2Zm1 6h2v9h-2V9Zm4 0h2v9h-2V9ZM6 8h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 8Z"/></svg>"#;
const SVG_SORT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M7.5 3.8a1 1 0 0 1 1.4 0l3.3 3.3a1 1 0 1 1-1.4 1.4L9.2 6.9V19a1 1 0 1 1-2 0V6.9L5.6 8.5a1 1 0 0 1-1.4-1.4l3.3-3.3Zm9.3 16.4a1 1 0 0 1-1.4 0l-3.3-3.3a1 1 0 1 1 1.4-1.4l1.6 1.6V5a1 1 0 1 1 2 0v12.1l1.6-1.6a1 1 0 1 1 1.4 1.4l-3.3 3.3Z"/></svg>"#;
const SVG_PLUS: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6V5Z"/></svg>"#;
const SVG_CHECK: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M9 16.2 4.8 12l-1.4 1.4L9 19 21 7l-1.4-1.4L9 16.2Z"/></svg>"#;
const SVG_FILTER: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M3 6a1 1 0 0 1 1-1h16a1 1 0 1 1 0 2H4a1 1 0 0 1-1-1Zm3 6a1 1 0 0 1 1-1h10a1 1 0 1 1 0 2H7a1 1 0 0 1-1-1Zm4 6a1 1 0 0 1 1-1h2a1 1 0 1 1 0 2h-2a1 1 0 0 1-1-1Z"/></svg>"#;
const SVG_WARN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M12 2.5a1.5 1.5 0 0 1 1.3.76l8.9 15.6A1.5 1.5 0 0 1 20.9 21H3.1a1.5 1.5 0 0 1-1.3-2.14l8.9-15.6A1.5 1.5 0 0 1 12 2.5ZM11 9v5h2V9h-2Zm0 7v2h2v-2h-2Z"/></svg>"#;
const SVG_CHEVRON_DOWN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M5.3 9.3a1 1 0 0 1 1.4 0l5.3 5.29 5.3-5.3a1 1 0 1 1 1.4 1.42l-6 6a1 1 0 0 1-1.4 0l-6-6a1 1 0 0 1 0-1.42Z"/></svg>"#;
const SVG_CHEVRON_UP: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M5.3 14.7a1 1 0 0 0 1.4 0l5.3-5.29 5.3 5.3a1 1 0 1 0 1.4-1.42l-6-6a1 1 0 0 0-1.4 0l-6 6a1 1 0 0 0 0 1.42Z"/></svg>"#;

static ICON_GEAR: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_CODE: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_RELOAD: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_TRASH: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_SORT: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_PLUS: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_CHECK: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_FILTER: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_WARN: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_CHEVRON_DOWN: OnceLock<Option<&'static str>> = OnceLock::new();
static ICON_CHEVRON_UP: OnceLock<Option<&'static str>> = OnceLock::new();

const DESKTOP_FULL: &[&str] = &[
   "silent-typing",
   "message-latency",
   "message-logger",
   "no-pending-count",
   "no-profile-themes",
   "no-reply-mention",
   "show-timeout-duration",
   "spoiler-mode",
   "text-replace",
   "view-raw",
   "global-badges",
   "dearrow",
   "translate",
   "clear-urls",
   "fix-embeds",
   "platform-indicators",
];

const DESKTOP_PARTIAL: &[&str] = &["fakenitro"];

const MOBILE_ONLY: &[&str] = &[
   "hide-composer-buttons",
   "no-haptics",
   "reviewdb",
   "taptap",
   "example",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum DesktopCompat {
   Full,
   Partial,
   MobileOnly,
   Unverified,
}

impl DesktopCompat {
   fn of(name: &str) -> Self {
      if DESKTOP_FULL.contains(&name) {
         Self::Full
      } else if DESKTOP_PARTIAL.contains(&name) {
         Self::Partial
      } else if MOBILE_ONLY.contains(&name) {
         Self::MobileOnly
      } else {
         Self::Unverified
      }
   }

   const fn tag(self) -> Option<&'static str> {
      match self {
         Self::Full => None,
         Self::Partial => Some("Partial"),
         Self::MobileOnly => Some("Mobile"),
         Self::Unverified => Some("Unverified"),
      }
   }

   /// `(background, text)` HSLA colors for non-full compat badges.
   const fn colors(self) -> Option<([f32; 4], [f32; 4])> {
      match self {
         Self::Full => None,
         // Warm amber
         Self::Partial => Some(([0.10, 0.85, 0.52, 0.16], [0.11, 0.92, 0.72, 1.0])),
         // Soft violet
         Self::MobileOnly => Some(([0.76, 0.72, 0.62, 0.18], [0.76, 0.86, 0.82, 1.0])),
         // Soft sky blue
         Self::Unverified => Some(([0.58, 0.78, 0.60, 0.16], [0.58, 0.88, 0.80, 1.0])),
      }
   }
}

fn compat_pill(b: &Builder, compat: DesktopCompat) -> Option<RawDiv> {
   let label = compat.tag()?;
   let (bg, text) = compat.colors()?;
   let mut badge = b.ui_text_line(label, text, 12.0)?;
   b.style_div(&mut badge, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.solid_background(bg);
      s.lengths(&s.at.corner_radii, gpui_rem(99.0));
      s.length(s.at.padding[0], gpui_rem(2.0));
      s.length(s.at.padding[1], gpui_rem(8.0));
      s.length(s.at.padding[2], gpui_rem(2.0));
      s.length(s.at.padding[3], gpui_rem(8.0));
   });
   Some(badge)
}

/// Builds the settings controls body for a single plugin, used inside the
/// native `PluginSettings` modal (and as an inline fallback when native modals
/// are unavailable).
pub fn build_plugin_settings_body(plugin: &phoenix_runtime::PluginInfo) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   MODAL_SETTINGS_ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();
   let b = Builder::modal(sym);
   let open_select = UI_STATE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .open_select
      .clone();
   Some(render_plugin_settings_surface(
      &b,
      plugin,
      open_select.as_ref(),
   ))
}

/// Builds a floating top-layer overlay for the currently open plugin-settings
/// `Control::Select` dropdown, anchored directly below its trigger with
/// Discord's `120ms ease-out` fade-and-slide transition.
pub fn build_open_select_overlay() -> Option<RawDiv> {
   const POSITION_ABSOLUTE: u8 = 1;
   let sym = SYMBOLS.get()?;
   let open = {
      let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
      let sel = ui.open_select.as_ref()?;
      if sel.progress().1 {
         ui.open_select = None;
         drop(ui);
         return None;
      }
      let out = sel.clone();
      drop(ui);
      out
   };
   let [ax, ay, aw, ah] = open.anchor?;
   let plugins = phoenix_runtime::plugins();
   let plugin = plugins.into_iter().find(|p| p.name == open.plugin)?;
   let setting = plugin.settings.into_iter().find(|s| s.key == open.key)?;
   let Control::Select { options, chosen } = setting.control else {
      return None;
   };
   if options.is_empty() {
      return None;
   }

   let b = Builder::modal(sym);
   let (eased, _) = open.progress();
   let slide_y = -6.0 * (1.0 - eased);

   let longest_chars = options.iter().map(|o| o.label.len()).max().unwrap_or(10);
   #[expect(clippy::cast_precision_loss, reason = "option label lengths are small")]
   let est_w = (longest_chars as f32).mul_add(9.5, 76.0).min(420.0);
   let left = (ax + aw - aw.max(est_w)).max(16.0);
   let top = ay + ah + 4.0 + slide_y;

   let mut menu = discord_select_listbox(&b, &open.plugin, &open.key, &options, &chosen);
   b.style_div(&mut menu, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.length(s.at.inset[0], gpui_px(top));
      s.length(s.at.inset[3], gpui_px(left));
      s.length(s.at.min_size[0], gpui_px(aw.max(est_w)));
      s.float(s.at.opacity, eased);
   });

   let mut layer = b.raw_div();
   b.style_div(&mut layer, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.lengths(&s.at.inset, gpui_px(0.0));
      s.lengths(&s.at.size, W_FULL);
   });
   // Only intercept outside clicks while open; while closing, pass clicks through.
   if open.closing.is_none() {
      let mut backdrop = b.raw_div();
      b.style_div(&mut backdrop, |s| {
         s.set(s.at.position, POSITION_ABSOLUTE);
         s.lengths(&s.at.inset, gpui_px(0.0));
         s.lengths(&s.at.size, W_FULL);
      });
      let mut backdrop = b.clickable(backdrop, "sb-select-backdrop", Action::CloseSelect);
      b.style_div(&mut backdrop, |s| s.set(s.at.mouse_cursor, 0));
      layer = b.push_div(layer, backdrop);
   }
   Some(b.push_div(layer, menu))
}

#[expect(
   clippy::too_many_lines,
   reason = "one match arm per plugin setting control variant"
)]
fn render_plugin_settings_surface(
   b: &Builder,
   plugin: &phoenix_runtime::PluginInfo,
   open_select: Option<&OpenSelect>,
) -> RawDiv {
   if plugin.settings.is_empty() {
      let mut empty = b.surface_card(4.0, 16.0, 10.0);
      if let Some(txt) = b.ui_text_line("This plugin has no configurable settings.", UI_MUTED, 14.0) {
         empty = b.push_div(empty, txt);
      }
      return empty;
   }
   let mut card = b.surface_card(0.0, 0.0, 10.0);
   for (idx, setting) in plugin.settings.iter().enumerate() {
      if idx > 0 {
         card = b.push_div(card, b.divider());
      }
      let sid = format!("sunbird-set-{}-{}", plugin.name, setting.key);
      let sdesc = setting.description.as_deref().unwrap_or("");
      let mut row_wrap = b.v_stack(8.0);
      b.style_div(&mut row_wrap, |s| {
         s.length(s.at.size[0], W_FULL);
         s.length(s.at.padding[0], gpui_rem(12.0));
         s.length(s.at.padding[1], gpui_rem(14.0));
         s.length(s.at.padding[2], gpui_rem(12.0));
         s.length(s.at.padding[3], gpui_rem(14.0));
      });
      let edit_row = |shown: &str, initial: String, kind: FieldKind| {
         let sub = if sdesc.is_empty() {
            shown.to_owned()
         } else {
            format!("{sdesc} · {shown}")
         };
         b.action_row(
            &sid,
            &setting.title,
            &sub,
            "Edit",
            BtnVariant::Secondary,
            BtnSize::Small,
            Action::EditSetting(SettingField {
               plugin: plugin.name.clone(),
               key: setting.key.clone(),
               title: setting.title.clone(),
               initial,
               kind,
            }),
         )
      };
      match &setting.control {
         Control::Switch(val) => {
            row_wrap = b.push_div(
               row_wrap,
               b.switch_row(
                  &sid,
                  &setting.title,
                  setting.description.as_deref(),
                  *val,
                  Action::ToggleSetting {
                     plugin: plugin.name.clone(),
                     key:    setting.key.clone(),
                  },
               ),
            );
         },
         Control::Select { options, chosen } => {
            let cur_label = options
               .iter()
               .find(|o| o.value == *chosen)
               .map_or(chosen.as_str(), |o| o.label.as_str());
            let dropdown_open =
               open_select.is_some_and(|s| s.is_open_for(&plugin.name, &setting.key));

            let mut left = b.v_stack(3.0);
            b.style_div(&mut left, |s| {
               s.length(s.at.min_size[0], gpui_px(0.0));
               s.float(s.at.flex_shrink, 1.0);
            });
            if let Some(title) = b.ui_text_line(&setting.title, UI_TEXT, 15.0) {
               left = b.push_div(left, title);
            }
            if !sdesc.is_empty()
               && let Some(desc) = b.ui_text_line(sdesc, UI_MUTED, 13.5)
            {
               left = b.push_div(left, desc);
            }

            let trigger = discord_select_trigger(
               b,
               &sid,
               cur_label,
               dropdown_open,
               Action::ToggleSelectDropdown {
                  plugin: plugin.name.clone(),
                  key:    setting.key.clone(),
               },
            );
            let mut head = b.h_row(12.0, true);
            b.style_div(&mut head, |s| s.length(s.at.size[0], W_FULL));
            head = b.push_div(head, left);
            head = b.push_div(head, trigger);
            row_wrap = b.push_div(row_wrap, head);

            // In modal mode the listbox renders as a floating top-layer popup
            // via `build_open_select_overlay` so it never pushes settings down.
            let has_floating = b.in_modal && open_select.is_some_and(|s| s.anchor.is_some());
            if dropdown_open && !options.is_empty() && !has_floating {
               row_wrap = b.push_div(
                  row_wrap,
                  discord_select_listbox(b, &plugin.name, &setting.key, options, chosen),
               );
            }
         },
         Control::Button => {
            row_wrap = b.push_div(
               row_wrap,
               b.action_row(
                  &sid,
                  &setting.title,
                  sdesc,
                  &setting.title,
                  BtnVariant::Secondary,
                  BtnSize::Small,
                  Action::PressButton {
                     plugin: plugin.name.clone(),
                     key:    setting.key.clone(),
                  },
               ),
            );
         },
         Control::Text(val) => {
            let count = val.lines().filter(|line| !line.trim().is_empty()).count();
            let shown = match (setting.multiline, count) {
               (_, 0) => "empty".to_owned(),
               (true, 1) => "1 line".to_owned(),
               (true, n) => format!("{n} lines"),
               (false, _) => val.clone(),
            };
            let kind = if setting.multiline {
               FieldKind::Lines
            } else {
               FieldKind::Text
            };
            row_wrap = b.push_div(row_wrap, edit_row(&shown, val.clone(), kind));
         },
         Control::Secret(val) => {
            let shown = if val.is_empty() { "not set" } else { "set" };
            row_wrap = b.push_div(row_wrap, edit_row(shown, String::new(), FieldKind::Secret));
         },
         Control::Number(val) => {
            let shown = val.to_string();
            row_wrap = b.push_div(row_wrap, edit_row(&shown, shown.clone(), FieldKind::Number));
         },
      }
      card = b.push_div(card, row_wrap);
   }
   card
}

/// Dark bordered Discord-style select trigger (`36rem` tall, `8rem` radius,
/// label on left, chevron on right, bright blurple focus ring when open).
fn discord_select_trigger(
   b: &Builder,
   id: &str,
   label: &str,
   open: bool,
   action: Action,
) -> RawDiv {
   let mut trigger = b.h_row(12.0, true);
   b.style_div(&mut trigger, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.min_size[0], gpui_rem(180.0));
      s.length(s.at.size[1], gpui_rem(36.0));
      s.solid_background(UI_CONTROL_BG);
      s.lengths(&s.at.border_widths, gpui_px(if open { 1.5 } else { 1.0 }));
      s.hsla(
         s.at.border_color,
         if open {
            UI_BLURPLE
         } else {
            UI_CONTROL_BORDER
         },
      );
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.length(s.at.padding[1], gpui_rem(10.0));
      s.length(s.at.padding[3], gpui_rem(12.0));
   });
   if let Some(txt) = b.ui_text_line(label, UI_TEXT, 14.5) {
      trigger = b.push_div(trigger, txt);
   }
   let (chev_svg, chev_slot, chev_fb, chev_col) = if open {
      (SVG_CHEVRON_UP, &ICON_CHEVRON_UP, "▴", UI_TEXT)
   } else {
      (SVG_CHEVRON_DOWN, &ICON_CHEVRON_DOWN, "▾", UI_MUTED)
   };
   if let Some(chev) = b.icon_or_text(chev_svg, chev_slot, chev_fb, 14.5, chev_col) {
      trigger = b.push_div(trigger, chev);
   }
   b.hover_div(&mut trigger, |s| {
      s.solid_background(UI_CONTROL_HOVER);
      if !open {
         s.hsla(s.at.border_color, UI_CONTROL_BORDER_HOVER);
      }
   });
   b.clickable(trigger, id, action)
}

/// Dark bordered Discord-style dropdown listbox for `Control::Select` options,
/// matching Discord's Select menu: bright white rows, hover highlight only,
/// and a plain white checkmark on the active option.
fn discord_select_listbox(
   b: &Builder,
   plugin: &str,
   key: &str,
   options: &[phoenix_runtime::Choice],
   chosen: &str,
) -> RawDiv {
   let mut menu = b.v_stack(2.0);
   b.style_div(&mut menu, |s| {
      s.solid_background(UI_POPOVER_BG);
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(s.at.border_color, UI_CONTROL_BORDER);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.lengths(&s.at.padding, gpui_rem(4.0));
   });
   for opt in options {
      let selected = opt.value == chosen;
      let opt_id = format!("sb-opt-{plugin}-{key}-{}", opt.value);
      let mut row = b.h_row(12.0, true);
      b.style_div(&mut row, |s| {
         s.length(s.at.size[0], W_FULL);
         s.lengths(&s.at.corner_radii, gpui_rem(COMPACT_RADIUS));
         s.length(s.at.padding[0], gpui_rem(10.0));
         s.length(s.at.padding[1], gpui_rem(12.0));
         s.length(s.at.padding[2], gpui_rem(10.0));
         s.length(s.at.padding[3], gpui_rem(12.0));
      });
      if let Some(mut txt) = b.ui_text_line(&opt.label, UI_TEXT, 14.5) {
         b.style_div(&mut txt, |s| {
            s.set(s.at.white_space, 1);
            s.float(s.at.flex_shrink, 0.0);
         });
         row = b.push_div(row, txt);
      }
      if selected
         && let Some(ic) = b.icon_or_text(SVG_CHECK, &ICON_CHECK, "✓", 15.0, UI_TEXT)
      {
         row = b.push_div(row, ic);
      }
      b.hover_div(&mut row, |s| {
         s.solid_background(UI_ROW_HOVER);
      });
      menu = b.push_div(
         menu,
         b.clickable(
            row,
            &opt_id,
            Action::PickSelectOption {
               plugin: plugin.to_owned(),
               key:    key.to_owned(),
               value:  opt.value.clone(),
            },
         ),
      );
   }
   menu
}

/// Renders the native Sunbird Settings page into `sret`.
#[expect(
   clippy::too_many_lines,
   reason = "snapshots UI state and dispatches the active settings page"
)]
pub fn render_sunbird_page(
   sret: *mut RawAnyElement,
   window: *mut c_void,
   cx: *mut c_void,
) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let b = Builder::page(sym);

   ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();

   let (
      tab,
      store_kind,
      filter,
      sort,
      cat_filter,
      cat_sort,
      expanded_plugins,
      open_select,
      compat_mask,
      popover,
   ) = {
      let ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
      (
         ui.tab,
         ui.store_kind,
         ui.filter,
         ui.sort,
         ui.cat_filter,
         ui.cat_sort,
         ui.expanded_plugins.clone(),
         ui.open_select.clone(),
         ui.compat_mask,
         ui.popover,
      )
   };

   // SAFETY: `Context<SettingsHost>` stores `app: *mut App` at `+0x0` and
   // `entity_id: (u32, u32)` at `+0x8`.
   record_settings_entity(cx);
   let app = if cx.is_null() {
      ptr::null_mut()
   } else {
      unsafe { *(cx.cast::<*mut c_void>()) }
   };

   let plugins = phoenix_runtime::plugins();
   let enabled_count = plugins.iter().filter(|p| p.state.is_on()).count();
   let disabled_count = plugins.len().saturating_sub(enabled_count);

   let mut root = b.v_stack(16.0);
   b.style_div(&mut root, |s| s.length(s.at.size[0], W_FULL));

   // 1. Clean Discord-native per-page header (or pill tab bar fallback).
   let catalog = update::catalog();
   if matches!(catalog, Catalog::Loaded(_))
      && MANUAL_CATALOG_REFRESH.swap(false, Ordering::AcqRel)
   {
      flash_done("sb-store-refresh");
   } else if matches!(catalog, Catalog::Failed(_)) {
      MANUAL_CATALOG_REFRESH.store(false, Ordering::Release);
   }
   let catalog_loading = matches!(catalog, Catalog::Loading);
   let catalog_count = match &catalog {
      Catalog::Loaded(items) => Some(items.len()),
      _ => None,
   };
   let header = if crate::settings::has_sidebar_category() {
      render_page_header(
         &b,
         tab,
         store_kind,
         plugins.len(),
         enabled_count,
         catalog_count,
         catalog_loading,
      )
   } else {
      render_tab_header(&b, tab, plugins.len(), enabled_count, catalog_count)
   };
   root = b.push_div(root, header);

   // 2. Active page body
   match tab {
      Tab::Plugins => {
         // SAFETY: called on GPUI main thread with live `window` and `app`.
         let search = unsafe { crate::modal::search_input(window, app, false) };
         root = render_plugins_tab(
            &b,
            root,
            &plugins,
            enabled_count,
            disabled_count,
            filter,
            sort,
            popover == OpenPopover::PluginsSort,
            search,
            &expanded_plugins,
            open_select.as_ref(),
         );
      },
      Tab::Catalog => {
         let installed: HashSet<String> = plugins.iter().map(|p| p.name.clone()).collect();
         // SAFETY: called on GPUI main thread with live `window` and `app`.
         let search = if store_kind == StoreKind::Plugins {
            unsafe { crate::modal::search_input(window, app, true) }
         } else {
            None
         };
         root = render_store_tab(
            &b,
            root,
            store_kind,
            catalog,
            &installed,
            cat_filter,
            cat_sort,
            search,
            compat_mask,
            popover,
         );
      },
      Tab::General => {
         root = render_general_tab(&b, root);
      },
   }

   b.write_any(root, sret);
   true
}

fn short_version() -> &'static str {
   update::VERSION
      .split('-')
      .next()
      .unwrap_or(update::VERSION)
}

fn store_refresh_state(catalog_loading: bool) -> IconBtnState {
   if catalog_loading {
      IconBtnState::Active
   } else if is_flashed("sb-store-refresh") {
      IconBtnState::Done
   } else {
      IconBtnState::Idle
   }
}

/// Discord-style segmented `Plugins | Themes` toggle for the Store header.
fn render_store_kind_switch(b: &Builder, active: StoreKind) -> RawDiv {
   discord_segmented(
      b,
      &[
         (
            "sb-store-kind-plugins",
            "Plugins",
            active == StoreKind::Plugins,
            Action::SetStoreKind(StoreKind::Plugins),
         ),
         (
            "sb-store-kind-themes",
            "Themes",
            active == StoreKind::Themes,
            Action::SetStoreKind(StoreKind::Themes),
         ),
      ],
   )
}

/// Clean Discord-native header for `Plugins`, `Store`, and `Settings`.
fn render_page_header(
   b: &Builder,
   tab: Tab,
   store_kind: StoreKind,
   plugin_count: usize,
   enabled_count: usize,
   catalog_count: Option<usize>,
   catalog_loading: bool,
) -> RawDiv {
   let (title, pill_count) = match tab {
      Tab::Plugins => ("Plugins", Some(plugin_count)),
      Tab::Catalog => (
         "Store",
         if store_kind == StoreKind::Plugins {
            catalog_count
         } else {
            None
         },
      ),
      Tab::General => ("Settings", None),
   };

   let mut title_row = b.h_row(8.0, false);
   if let Some(t) = b.ui_text_line(title, UI_TEXT, 20.0) {
      title_row = b.push_div(title_row, t);
   }
   if let Some(n) = pill_count
      && let Some(mut pill) = b.ui_text_line(&n.to_string(), UI_BLURPLE_TEXT, 12.0)
   {
      b.style_div(&mut pill, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.solid_background(UI_BLURPLE_SOFT);
         s.lengths(&s.at.corner_radii, gpui_rem(99.0));
         s.length(s.at.padding[0], gpui_rem(2.0));
         s.length(s.at.padding[1], gpui_rem(8.0));
         s.length(s.at.padding[2], gpui_rem(2.0));
         s.length(s.at.padding[3], gpui_rem(8.0));
      });
      title_row = b.push_div(title_row, pill);
   }

   let mut right = b.h_row(8.0, false);
   b.style_div(&mut right, |s| s.float(s.at.flex_shrink, 0.0));
   match tab {
      Tab::Plugins => {
         let status = format!("{enabled_count}/{plugin_count} active · v{}", short_version());
         if let Some(txt) = b.ui_text_line(&status, UI_DIM, 13.0) {
            right = b.push_div(right, txt);
         }
      },
      Tab::Catalog => {
         right = b.push_div(right, render_store_kind_switch(b, store_kind));
         if store_kind == StoreKind::Plugins {
            right = b.push_div(
               right,
               discord_toolbar_icon_btn(
                  b,
                  "sb-store-refresh",
                  (SVG_RELOAD, &ICON_RELOAD, "↻"),
                  store_refresh_state(catalog_loading),
                  Action::RefreshCatalog,
               ),
            );
         }
      },
      Tab::General => {
         let ver = format!("v{}", short_version());
         if let Some(txt) = b.ui_text_line(&ver, UI_DIM, 13.0) {
            right = b.push_div(right, txt);
         }
      },
   }

   let mut header = b.h_row(12.0, true);
   b.style_div(&mut header, |s| {
      s.length(s.at.size[0], W_FULL);
      s.length(s.at.padding[2], gpui_rem(12.0));
      s.length(s.at.border_widths[2], gpui_px(1.0));
      s.hsla(s.at.border_color, UI_DIVIDER);
   });
   header = b.push_div(header, title_row);
   b.push_div(header, right)
}

/// Clean rounded pill tab bar fallback when the native sidebar category hook is
/// unavailable and Sunbird appears as a single sidebar entry.
fn render_tab_header(
   b: &Builder,
   active: Tab,
   plugin_count: usize,
   enabled_count: usize,
   catalog_count: Option<usize>,
) -> RawDiv {
   let tab_btn = |id: &str, t: Tab, label: &str, count: Option<usize>| {
      let is_active = active == t;
      let mut item = b.h_row(7.0, false);
      b.style_div(&mut item, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.lengths(&s.at.corner_radii, gpui_rem(8.0));
         s.length(s.at.padding[0], gpui_rem(6.0));
         s.length(s.at.padding[1], gpui_rem(12.0));
         s.length(s.at.padding[2], gpui_rem(6.0));
         s.length(s.at.padding[3], gpui_rem(12.0));
         if is_active {
            s.solid_background(UI_SEG_ACTIVE);
         }
      });
      if let Some(txt) = b.ui_text_line(label, if is_active { UI_TEXT } else { UI_MUTED }, 15.0) {
         item = b.push_div(item, txt);
      }
      if let Some(n) = count
         && let Some(mut pill) = b.ui_text_line(
            &n.to_string(),
            if is_active { UI_BLURPLE_TEXT } else { UI_MUTED },
            12.0,
         )
      {
         b.style_div(&mut pill, |s| {
            s.solid_background(if is_active {
               UI_BLURPLE_SOFT
            } else {
               UI_ROW_HOVER
            });
            s.lengths(&s.at.corner_radii, gpui_rem(99.0));
            s.length(s.at.padding[0], gpui_rem(1.5));
            s.length(s.at.padding[1], gpui_rem(7.0));
            s.length(s.at.padding[2], gpui_rem(1.5));
            s.length(s.at.padding[3], gpui_rem(7.0));
         });
         item = b.push_div(item, pill);
      }
      b.hover_div(&mut item, |s| {
         s.solid_background(if is_active {
            UI_SEG_ACTIVE_HOVER
         } else {
            UI_ROW_HOVER
         });
      });
      b.clickable(item, id, Action::SetTab(t))
   };

   let mut tabs = b.h_row(6.0, false);
   tabs = b.push_div(
      tabs,
      tab_btn("sb-tab-plugins", Tab::Plugins, "Plugins", Some(plugin_count)),
   );
   tabs = b.push_div(
      tabs,
      tab_btn("sb-tab-store", Tab::Catalog, "Store", catalog_count),
   );
   tabs = b.push_div(
      tabs,
      tab_btn("sb-tab-settings", Tab::General, "Settings", None),
   );

   let mut meta = b.h_row(6.0, false);
   b.style_div(&mut meta, |s| s.float(s.at.flex_shrink, 0.0));
   let status_line = format!("{enabled_count}/{plugin_count} active · v{}", short_version());
   if let Some(txt) = b.ui_text_line(&status_line, UI_DIM, 13.0) {
      meta = b.push_div(meta, txt);
   }

   let mut header = b.h_row(12.0, true);
   b.style_div(&mut header, |s| {
      s.length(s.at.size[0], W_FULL);
      s.length(s.at.padding[2], gpui_rem(10.0));
      s.length(s.at.border_widths[2], gpui_px(1.0));
      s.hsla(s.at.border_color, UI_DIVIDER);
   });
   header = b.push_div(header, tabs);
   b.push_div(header, meta)
}

#[expect(
   clippy::too_many_arguments,
   clippy::too_many_lines,
   reason = "builds the full Option B single-column Plugins tab"
)]
fn render_plugins_tab(
   b: &Builder,
   root: RawDiv,
   plugins: &[phoenix_runtime::PluginInfo],
   enabled_count: usize,
   disabled_count: usize,
   filter: PluginFilter,
   sort: PluginSort,
   sort_open: bool,
   search: Option<(RawAnyElement, String)>,
   expanded: &HashSet<String>,
   open_select: Option<&OpenSelect>,
) -> RawDiv {
   let (search_el, query) = match search {
      Some((el, q)) => (Some(el), q.trim().to_ascii_lowercase()),
      None => (None, String::new()),
   };

   // Toolbar: search input + segmented filter on left, Sort + URL + File + Reload on right
   let mut left_controls = b.h_row(8.0, false);
   b.style_div(&mut left_controls, |s| s.set(s.at.flex_wrap, FLEX_WRAP));
   if let Some(mut el) = search_el {
      let mut search_wrap = b.v_stack(0.0);
      b.style_div(&mut search_wrap, |s| {
         s.length(s.at.size[0], gpui_rem(SEARCH_WIDTH));
         s.float(s.at.flex_shrink, 0.0);
      });
      if b.push_any(&mut search_wrap, &mut el) {
         left_controls = b.push_div(left_controls, search_wrap);
      }
   }

   let enabled_lbl = format!("Enabled ({enabled_count})");
   let disabled_lbl = format!("Disabled ({disabled_count})");
   let seg = discord_segmented(
      b,
      &[
         (
            "sb-filter-all",
            "All",
            filter == PluginFilter::All,
            Action::SetFilter(PluginFilter::All),
         ),
         (
            "sb-filter-enabled",
            &enabled_lbl,
            filter == PluginFilter::Enabled,
            Action::SetFilter(PluginFilter::Enabled),
         ),
         (
            "sb-filter-disabled",
            &disabled_lbl,
            filter == PluginFilter::Disabled,
            Action::SetFilter(PluginFilter::Disabled),
         ),
      ],
   );
   left_controls = b.push_div(left_controls, seg);

   let sort_btn = discord_toolbar_btn(
      b,
      "sb-plugins-sort",
      "Sort",
      Some((SVG_SORT, &ICON_SORT, "⇅")),
      sort_open,
      Action::TogglePopover(OpenPopover::PluginsSort),
   );
   let reload_all_state = if RELOADING_ALL.load(Ordering::Acquire) {
      IconBtnState::Active
   } else if is_flashed("sb-reload-all") {
      IconBtnState::Done
   } else {
      IconBtnState::Idle
   };

   let mut right_actions = b.h_row(8.0, false);
   b.style_div(&mut right_actions, |s| s.float(s.at.flex_shrink, 0.0));
   right_actions = b.push_div(right_actions, sort_btn);
   right_actions = b.push_div(
      right_actions,
      discord_primary_btn(
         b,
         "sb-install-url",
         "URL",
         Some((SVG_PLUS, &ICON_PLUS, "+")),
         Action::OpenInstallUrl,
      ),
   );
   right_actions = b.push_div(
      right_actions,
      discord_toolbar_btn(
         b,
         "sb-install-file",
         "File...",
         None,
         false,
         Action::InstallFromFile,
      ),
   );
   right_actions = b.push_div(
      right_actions,
      discord_toolbar_icon_btn(
         b,
         "sb-reload-all",
         (SVG_RELOAD, &ICON_RELOAD, "↻"),
         reload_all_state,
         Action::ReloadAll,
      ),
   );

   let mut toolbar = b.h_row(10.0, true);
   b.style_div(&mut toolbar, |s| {
      s.length(s.at.size[0], W_FULL);
      s.set(s.at.flex_wrap, FLEX_WRAP);
   });
   toolbar = b.push_div(toolbar, left_controls);
   toolbar = b.push_div(toolbar, right_actions);

   let mut content = b.v_stack(14.0);
   b.style_div(&mut content, |s| s.length(s.at.size[0], W_FULL));
   content = b.push_div(content, toolbar);

   // Filter + sort
   let mut filtered: Vec<&phoenix_runtime::PluginInfo> = plugins
      .iter()
      .filter(|p| {
         let matches_filter = match filter {
            PluginFilter::All => true,
            PluginFilter::Enabled => p.state.is_on(),
            PluginFilter::Disabled => !p.state.is_on(),
         };
         if !matches_filter {
            return false;
         }
         if query.is_empty() {
            return true;
         }
         p.name.to_ascii_lowercase().contains(&query)
            || p.header
               .description
               .as_deref()
               .is_some_and(|d| d.to_ascii_lowercase().contains(&query))
            || p.header
               .author
               .as_deref()
               .is_some_and(|a| a.to_ascii_lowercase().contains(&query))
      })
      .collect();

   match sort {
      PluginSort::AlphaAsc => {
         filtered.sort_by_key(|a| a.name.to_ascii_lowercase());
      },
      PluginSort::AlphaDesc => {
         filtered.sort_by_key(|a| std::cmp::Reverse(a.name.to_ascii_lowercase()));
      },
      PluginSort::EnabledFirst => {
         filtered.sort_by(|a, b| {
            b.state
               .is_on()
               .cmp(&a.state.is_on())
               .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
         });
      },
   }

   if filtered.is_empty() {
      let msg = if plugins.is_empty() {
         "Open Store to browse and install plugins."
      } else if !query.is_empty() {
         "No plugins match your search."
      } else {
         "No plugins match the current filter."
      };
      let mut empty = b.surface_card(10.0, 18.0, CARD_RADIUS);
      if let Some(txt) = b.ui_text_line(msg, UI_MUTED, 14.0) {
         empty = b.push_div(empty, txt);
      }
      if !plugins.is_empty() && filter != PluginFilter::All && query.is_empty() {
         let mut row = b.h_row(0.0, false);
         row = b.push_div(
            row,
            discord_toolbar_btn(
               b,
               "sb-plugins-reset-filter",
               "Show all plugins",
               None,
               false,
               Action::SetFilter(PluginFilter::All),
            ),
         );
         empty = b.push_div(empty, row);
      }
      content = b.push_div(content, empty);
      if sort_open {
         content = b.push_div(content, discord_popover_backdrop(b));
         content = b.push_div(content, render_plugins_sort_popover(b, sort));
      }
      return b.push_div(root, content);
   }

   // Single grouped surface card with crisp hairline dividers (Option B)
   let mut card = b.surface_card(0.0, 0.0, CARD_RADIUS);
   for (idx, plugin) in filtered.iter().enumerate() {
      if idx > 0 {
         card = b.push_div(card, b.divider());
      }
      let compat = DesktopCompat::of(&plugin.name);
      let enabled = plugin.state.is_on();
      let has_settings = enabled && !plugin.settings.is_empty();
      let inline_open = !crate::modal::is_ready() && has_settings && expanded.contains(&plugin.name);

      // Left column: title + version + author + compat pill on line 1, desc on line 2
      let mut title_row = b.h_row(8.0, false);
      b.style_div(&mut title_row, |s| s.set(s.at.flex_wrap, FLEX_WRAP));
      if let Some(name_div) = b.ui_text_line(
         &plugin.name,
         if enabled { UI_TEXT } else { UI_MUTED },
         15.5,
      ) {
         title_row = b.push_div(title_row, name_div);
      }
      if let Some(ver) = &plugin.header.version
         && let Some(ver_div) = b.ui_text_line(&format!("v{ver}"), UI_DIM, 13.0)
      {
         title_row = b.push_div(title_row, ver_div);
      }
      if let Some(author) = &plugin.header.author
         && let Some(auth_div) = b.ui_text_line(&format!("· {author}"), UI_DIM, 13.0)
      {
         title_row = b.push_div(title_row, auth_div);
      }
      if let Some(badge) = compat_pill(b, compat) {
         title_row = b.push_div(title_row, badge);
      }

      let mut left = b.v_stack(3.0);
      b.style_div(&mut left, |s| {
         s.length(s.at.min_size[0], gpui_px(0.0));
         s.float(s.at.flex_shrink, 1.0);
      });
      left = b.push_div(left, title_row);
      if let Some(err) = plugin.state.error() {
         if let Some(err_div) = b.ui_text_line(&format!("Error: {err}"), UI_DANGER, 13.5) {
            left = b.push_div(left, err_div);
         }
      } else if let Some(desc) = &plugin.header.description
         && !desc.is_empty()
         && let Some(desc_div) = b.ui_text_line(desc, UI_MUTED, 13.5)
      {
         left = b.push_div(left, desc_div);
      }
      let perms_line = if plugin.permissions.is_empty() {
         "No permissions".to_owned()
      } else {
         let grants = plugin.permissions.iter().map(ToString::to_string);
         format!("Reaches {}", grants.collect::<Vec<_>>().join(", "))
      };
      if let Some(perms_div) = b.ui_text_line(&perms_line, UI_DIM, 12.0) {
         left = b.push_div(left, perms_div);
      }

      let mut right = b.h_row(4.0, false);
      b.style_div(&mut right, |s| s.float(s.at.flex_shrink, 0.0));
      if has_settings {
         right = b.push_div(
            right,
            b.row_icon_btn(
               &format!("sb-gear-{}", plugin.name),
               SVG_GEAR,
               &ICON_GEAR,
               "⚙",
               if inline_open { UI_BLURPLE_TEXT } else { UI_MUTED },
               inline_open.then_some(UI_BLURPLE_SOFT),
               Action::OpenPluginSettings(plugin.name.clone()),
            ),
         );
      }
      right = b.push_div(
         right,
         b.row_icon_btn(
            &format!("sb-code-{}", plugin.name),
            SVG_CODE,
            &ICON_CODE,
            "</>",
            UI_MUTED,
            None,
            Action::EditPluginCode(plugin.name.clone()),
         ),
      );
      if enabled && plugin.installed {
         let reload_id = format!("sb-reload-{}", plugin.name);
         let done = is_flashed(&reload_id);
         let busy = is_plugin_reloading(&plugin.name);
         let (svg, slot, fb, col, hl) = if done {
            (SVG_CHECK, &ICON_CHECK, "✓", UI_SUCCESS, Some(UI_SUCCESS_BG))
         } else if busy {
            (
               SVG_RELOAD,
               &ICON_RELOAD,
               "↻",
               UI_BLURPLE_TEXT,
               Some(UI_BLURPLE_SOFT),
            )
         } else {
            (SVG_RELOAD, &ICON_RELOAD, "↻", UI_MUTED, None)
         };
         right = b.push_div(
            right,
            b.row_icon_btn(
               &reload_id,
               svg,
               slot,
               fb,
               col,
               hl,
               Action::ReloadPlugin(plugin.name.clone()),
            ),
         );
      } else if !enabled && plugin.installed {
         right = b.push_div(
            right,
            b.row_icon_btn(
               &format!("sb-trash-{}", plugin.name),
               SVG_TRASH,
               &ICON_TRASH,
               "×",
               UI_DANGER,
               None,
               Action::UninstallPlugin(plugin.name.clone()),
            ),
         );
      }
      let mut sw_wrap = b.h_row(0.0, false);
      b.style_div(&mut sw_wrap, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.padding[3], gpui_rem(8.0));
      });
      sw_wrap = b.push_div(
         sw_wrap,
         b.toggle_switch(
            &format!("sunbird-plugin-{}", plugin.name),
            enabled,
            Action::TogglePlugin(plugin.name.clone()),
         ),
      );
      right = b.push_div(right, sw_wrap);

      let mut row = b.h_row(14.0, true);
      b.style_div(&mut row, |s| {
         s.length(s.at.size[0], W_FULL);
         s.length(s.at.padding[0], gpui_rem(13.0));
         s.length(s.at.padding[1], gpui_rem(16.0));
         s.length(s.at.padding[2], gpui_rem(13.0));
         s.length(s.at.padding[3], gpui_rem(16.0));
      });
      row = b.push_div(row, left);
      row = b.push_div(row, right);
      card = b.push_div(card, row);

      // Fallback inline settings drawer only when native modals are unavailable
      if inline_open {
         let mut drawer_wrap = b.v_stack(0.0);
         b.style_div(&mut drawer_wrap, |s| {
            s.length(s.at.size[0], W_FULL);
            s.length(s.at.padding[1], gpui_rem(14.0));
            s.length(s.at.padding[2], gpui_rem(12.0));
            s.length(s.at.padding[3], gpui_rem(14.0));
         });
         drawer_wrap = b.push_div(
            drawer_wrap,
            render_plugin_settings_surface(b, plugin, open_select),
         );
         card = b.push_div(card, drawer_wrap);
      }
   }

   content = b.push_div(content, card);
   if sort_open {
      content = b.push_div(content, discord_popover_backdrop(b));
      content = b.push_div(content, render_plugins_sort_popover(b, sort));
   }

   b.push_div(root, content)
}

fn render_plugins_sort_popover(b: &Builder, sort: PluginSort) -> RawDiv {
   let mut menu = discord_popover_shell(b, 210.0, 188.0);
   menu = b.push_div(
      menu,
      discord_radio_row(
         b,
         "sb-psort-az",
         "Name (A-Z)",
         sort == PluginSort::AlphaAsc,
         Action::SetPluginSort(PluginSort::AlphaAsc),
      ),
   );
   menu = b.push_div(
      menu,
      discord_radio_row(
         b,
         "sb-psort-za",
         "Name (Z-A)",
         sort == PluginSort::AlphaDesc,
         Action::SetPluginSort(PluginSort::AlphaDesc),
      ),
   );
   b.push_div(
      menu,
      discord_radio_row(
         b,
         "sb-psort-active",
         "Enabled First",
         sort == PluginSort::EnabledFirst,
         Action::SetPluginSort(PluginSort::EnabledFirst),
      ),
   )
}

/// Short compatibility caption for a Store plugin, used in the detail modal.
pub fn store_compat_label(name: &str) -> &'static str {
   match DesktopCompat::of(name) {
      DesktopCompat::Full => "Desktop Ready",
      DesktopCompat::Partial => "Partial Desktop Support",
      DesktopCompat::MobileOnly => "Mobile Only",
      DesktopCompat::Unverified => "Unverified",
   }
}

const fn compat_bit(compat: DesktopCompat) -> u8 {
   match compat {
      DesktopCompat::Full => COMPAT_FULL,
      DesktopCompat::Partial => COMPAT_PARTIAL,
      DesktopCompat::MobileOnly => COMPAT_MOBILE,
      DesktopCompat::Unverified => COMPAT_UNVERIFIED,
   }
}

/// Discord-native metadata body for the GPUI `StoreDetail` modal opened when
/// clicking a Store card.
pub fn build_store_detail_body(
   item: &Listed,
   installed: Option<&phoenix_runtime::PluginInfo>,
   installing: bool,
) -> Option<RawDiv> {
   let sym = SYMBOLS.get()?;
   let b = Builder::modal(sym);

   let mut body = b.v_stack(12.0);
   b.style_div(&mut body, |s| s.length(s.at.size[0], W_FULL));

   if !item.description.is_empty()
      && let Some(desc) = b.ui_text_line(&item.description, UI_TEXT, 14.5)
   {
      body = b.push_div(body, desc);
   }

   if let Some(warn) = &item.warning
      && !warn.is_empty()
      && let Some(mut warn_txt) = b.ui_text_line(warn, UI_TEXT, 13.5)
   {
      b.style_div(&mut warn_txt, |s| {
         s.length(s.at.min_size[0], gpui_px(0.0));
         s.float(s.at.flex_shrink, 1.0);
      });
      let mut banner = b.h_row(10.0, false);
      b.style_div(&mut banner, |s| {
         s.length(s.at.size[0], W_FULL);
         s.solid_background(UI_WARN_BG);
         s.lengths(&s.at.border_widths, gpui_px(1.0));
         s.hsla(s.at.border_color, UI_WARN_BORDER);
         s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
         s.length(s.at.padding[0], gpui_rem(10.0));
         s.length(s.at.padding[1], gpui_rem(12.0));
         s.length(s.at.padding[2], gpui_rem(10.0));
         s.length(s.at.padding[3], gpui_rem(12.0));
      });
      if let Some(ic) = b.icon_or_text(SVG_WARN, &ICON_WARN, "!", 16.5, UI_WARN) {
         banner = b.push_div(banner, ic);
      }
      banner = b.push_div(banner, warn_txt);
      body = b.push_div(body, banner);
   }

   let status_val = if installing {
      ("Installing...", UI_MUTED)
   } else if let Some(plugin) = installed {
      if plugin.state.is_on() {
         ("Installed · Enabled", UI_SUCCESS)
      } else {
         ("Installed · Disabled", UI_MUTED)
      }
   } else {
      ("Not installed", UI_MUTED)
   };

   let compat_val = match DesktopCompat::of(&item.name) {
      DesktopCompat::Full => "Desktop Ready",
      DesktopCompat::Partial => "Partial Desktop Support",
      DesktopCompat::MobileOnly => "Mobile Only (may not work on desktop)",
      DesktopCompat::Unverified => "Unverified on desktop",
   };

   let meta_row = |label: &str, value: &str, color: [f32; 4], mono: bool| {
      let mut r = b.h_row(12.0, true);
      b.style_div(&mut r, |s| {
         s.length(s.at.size[0], W_FULL);
         s.length(s.at.padding[0], gpui_rem(10.0));
         s.length(s.at.padding[1], gpui_rem(14.0));
         s.length(s.at.padding[2], gpui_rem(10.0));
         s.length(s.at.padding[3], gpui_rem(14.0));
      });
      if let Some(k) = b.ui_text_line(label, UI_MUTED, 13.0) {
         r = b.push_div(r, k);
      }
      if let Some(mut v) = b.ui_text_line(value, color, if mono { 12.5 } else { 13.5 }) {
         if mono {
            b.style_div(&mut v, |s| {
               s.shared_string(s.at.font_family, SharedString::from_static(MONO_FONT));
            });
         }
         r = b.push_div(r, v);
      }
      r
   };

   let mut card = b.surface_card(0.0, 0.0, 10.0);
   card = b.push_div(card, meta_row("Status", status_val.0, status_val.1, false));
   card = b.push_div(card, b.divider());
   card = b.push_div(card, meta_row("Compatibility", compat_val, UI_TEXT, false));
   if let Some(plugin) = installed {
      if let Some(ver) = &plugin.header.version {
         card = b.push_div(card, b.divider());
         card = b.push_div(card, meta_row("Version", &format!("v{ver}"), UI_TEXT, true));
      }
      if let Some(author) = &plugin.header.author {
         card = b.push_div(card, b.divider());
         card = b.push_div(card, meta_row("Author", author, UI_TEXT, false));
      }
   }
   card = b.push_div(card, b.divider());
   card = b.push_div(
      card,
      meta_row(
         "Source",
         &format!("assets.sunbird.tools/plugins/{}.js", item.name),
         UI_MUTED,
         true,
      ),
   );
   body = b.push_div(body, card);
   Some(body)
}

fn render_store_card(b: &Builder, item: &Listed, is_installed: bool, is_installing: bool) -> RawDiv {
   let compat = DesktopCompat::of(&item.name);

   let mut title_row = b.h_row(8.0, false);
   b.style_div(&mut title_row, |s| {
      s.length(s.at.min_size[0], gpui_px(0.0));
      s.float(s.at.flex_shrink, 1.0);
      s.set(s.at.flex_wrap, FLEX_WRAP);
   });
   if let Some(name_div) = b.ui_text_line(&item.name, UI_TEXT, 15.5) {
      title_row = b.push_div(title_row, name_div);
   }
   if let Some(pill) = compat_pill(b, compat) {
      title_row = b.push_div(title_row, pill);
   }

   let mut top_row = b.h_row(10.0, true);
   b.style_div(&mut top_row, |s| s.length(s.at.size[0], W_FULL));
   top_row = b.push_div(top_row, title_row);

   if is_installing {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_CENTER: u8 = 4;
      let mut st = b.h_row(0.0, false);
      b.style_div(&mut st, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.size[1], gpui_rem(COMPACT_HEIGHT));
         s.set(s.at.align_items, ALIGN_CENTER);
         s.set(s.at.justify_content, JUSTIFY_CENTER);
         s.solid_background(UI_CONTROL_BG);
         s.lengths(&s.at.border_widths, gpui_px(1.0));
         s.hsla(s.at.border_color, UI_CONTROL_BORDER);
         s.lengths(&s.at.corner_radii, gpui_rem(COMPACT_RADIUS));
         s.length(s.at.padding[1], gpui_rem(10.0));
         s.length(s.at.padding[3], gpui_rem(10.0));
      });
      if let Some(txt) = b.ui_text_line("Installing...", UI_MUTED, 12.5) {
         st = b.push_div(st, txt);
      }
      top_row = b.push_div(top_row, st);
   } else if is_installed {
      const ALIGN_CENTER: u8 = 4;
      const JUSTIFY_CENTER: u8 = 4;
      let mut badge = b.h_row(5.0, false);
      b.style_div(&mut badge, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.size[1], gpui_rem(COMPACT_HEIGHT));
         s.set(s.at.align_items, ALIGN_CENTER);
         s.set(s.at.justify_content, JUSTIFY_CENTER);
         s.solid_background(UI_SUCCESS_BG);
         s.lengths(&s.at.corner_radii, gpui_rem(COMPACT_RADIUS));
         s.length(s.at.padding[1], gpui_rem(9.0));
         s.length(s.at.padding[3], gpui_rem(9.0));
      });
      if let Some(ic) = b.icon_or_text(SVG_CHECK, &ICON_CHECK, "✓", 13.5, UI_SUCCESS) {
         badge = b.push_div(badge, ic);
      }
      if let Some(txt) = b.ui_text_line("Installed", UI_SUCCESS, 12.5) {
         badge = b.push_div(badge, txt);
      }
      top_row = b.push_div(top_row, badge);
   } else {
      top_row = b.push_div(
         top_row,
         b.chip(
            &format!("sb-store-inst-{}", item.name),
            "Install",
            None,
            UI_TEXT,
            Some(UI_BLURPLE),
            None,
            Action::InstallCatalog(item.name.clone()),
         ),
      );
   }

   let mut card = b.surface_card(8.0, 16.0, CARD_RADIUS);
   b.style_div(&mut card, |s| {
      s.length(s.at.min_size[0], gpui_px(0.0));
      s.float(s.at.flex_shrink, 1.0);
   });
   card = b.push_div(card, top_row);
   if !item.description.is_empty()
      && let Some(desc) = b.ui_text_line(&item.description, UI_MUTED, 13.5)
   {
      card = b.push_div(card, desc);
   }
   if let Some(warn) = &item.warning
      && !warn.is_empty()
      && let Some(w_div) = b.ui_text_line(warn, UI_WARN, 12.5)
   {
      card = b.push_div(card, w_div);
   }
   b.hover_div(&mut card, |s| {
      s.solid_background(UI_SURFACE_HOVER);
      s.hsla(s.at.border_color, UI_BORDER_HOVER);
   });
   b.clickable(
      card,
      &format!("sb-store-card-{}", item.name),
      Action::OpenStoreDetail(item.clone()),
   )
}

/// Uniform dark bordered segmented control (`34rem` tall, `8rem` radius) used
/// across Plugins, Store, and the Store header switcher.
fn discord_segmented(b: &Builder, items: &[(&str, &str, bool, Action)]) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let mut track = b.h_row(2.0, false);
   b.style_div(&mut track, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[1], gpui_rem(CTRL_HEIGHT));
      s.solid_background(UI_CONTROL_BG);
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(s.at.border_color, UI_CONTROL_BORDER);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.lengths(&s.at.padding, gpui_rem(3.0));
   });
   for (id, label, active, action) in items {
      let mut pill = b.h_row(0.0, false);
      b.style_div(&mut pill, |s| {
         s.float(s.at.flex_shrink, 0.0);
         s.length(s.at.size[1], gpui_rem(CTRL_HEIGHT - 8.0));
         s.set(s.at.align_items, ALIGN_CENTER);
         s.set(s.at.justify_content, JUSTIFY_CENTER);
         s.lengths(&s.at.corner_radii, gpui_rem(5.5));
         s.length(s.at.padding[1], gpui_rem(10.0));
         s.length(s.at.padding[3], gpui_rem(10.0));
         if *active {
            s.solid_background(UI_SEG_ACTIVE);
         }
      });
      if let Some(txt) = b.ui_text_line(label, if *active { UI_TEXT } else { UI_MUTED }, 13.5) {
         pill = b.push_div(pill, txt);
      }
      b.hover_div(&mut pill, |s| {
         s.solid_background(if *active {
            UI_SEG_ACTIVE_HOVER
         } else {
            UI_ROW_HOVER
         });
      });
      track = b.push_div(track, b.clickable(pill, id, action.clone()));
   }
   track
}

/// Uniform dark bordered toolbar button (`34rem` tall, `8rem` radius) matching
/// Discord's search/filter bar controls (`↑↓ Sort`, `File...`, `Check for Updates`).
fn discord_toolbar_btn(
   b: &Builder,
   id: &str,
   label: &str,
   icon: Option<IconSpec>,
   open: bool,
   action: Action,
) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let mut btn = b.h_row(6.0, false);
   b.style_div(&mut btn, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[1], gpui_rem(CTRL_HEIGHT));
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, JUSTIFY_CENTER);
      s.solid_background(if open {
         UI_CONTROL_HOVER
      } else {
         UI_CONTROL_BG
      });
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(
         s.at.border_color,
         if open {
            UI_CONTROL_BORDER_HOVER
         } else {
            UI_CONTROL_BORDER
         },
      );
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.length(s.at.padding[1], gpui_rem(12.0));
      s.length(s.at.padding[3], gpui_rem(if icon.is_some() { 11.0 } else { 12.0 }));
   });
   if let Some(ic_spec) = icon
      && let Some(ic) = b.icon_or_text(ic_spec.0, ic_spec.1, ic_spec.2, 15.5, UI_TEXT)
   {
      btn = b.push_div(btn, ic);
   }
   if let Some(txt) = b.ui_text_line(label, UI_TEXT, 13.5) {
      btn = b.push_div(btn, txt);
   }
   b.hover_div(&mut btn, |s| {
      s.solid_background(UI_CONTROL_HOVER);
      s.hsla(s.at.border_color, UI_CONTROL_BORDER_HOVER);
   });
   b.clickable(btn, id, action)
}

fn discord_primary_btn(
   b: &Builder,
   id: &str,
   label: &str,
   icon: Option<IconSpec>,
   action: Action,
) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let mut btn = b.h_row(6.0, false);
   b.style_div(&mut btn, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[1], gpui_rem(CTRL_HEIGHT));
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, JUSTIFY_CENTER);
      s.solid_background(UI_BLURPLE);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.length(s.at.padding[1], gpui_rem(12.0));
      s.length(s.at.padding[3], gpui_rem(if icon.is_some() { 10.0 } else { 12.0 }));
   });
   if let Some(ic_spec) = icon
      && let Some(ic) = b.icon_or_text(ic_spec.0, ic_spec.1, ic_spec.2, 15.5, UI_TEXT)
   {
      btn = b.push_div(btn, ic);
   }
   if let Some(txt) = b.ui_text_line(label, UI_TEXT, 13.5) {
      btn = b.push_div(btn, txt);
   }
   b.hover_div(&mut btn, |s| {
      s.solid_background(UI_BLURPLE_HOVER);
   });
   b.clickable(btn, id, action)
}

fn discord_toolbar_icon_btn(
   b: &Builder,
   id: &str,
   icon: IconSpec,
   state: IconBtnState,
   action: Action,
) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let (bg, border, icon_spec, icon_color) = match state {
      IconBtnState::Idle => (UI_CONTROL_BG, UI_CONTROL_BORDER, icon, UI_TEXT),
      IconBtnState::Open => (UI_CONTROL_HOVER, UI_CONTROL_BORDER_HOVER, icon, UI_TEXT),
      IconBtnState::Active => (UI_CONTROL_HOVER, UI_BLURPLE_BORDER, icon, UI_BLURPLE_TEXT),
      IconBtnState::Done => (
         UI_SUCCESS_BG,
         UI_SUCCESS_BORDER,
         (SVG_CHECK, &ICON_CHECK, "✓"),
         UI_SUCCESS,
      ),
   };

   let mut btn = b.h_row(0.0, false);
   b.style_div(&mut btn, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[0], gpui_rem(CTRL_HEIGHT));
      s.length(s.at.size[1], gpui_rem(CTRL_HEIGHT));
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, JUSTIFY_CENTER);
      s.solid_background(bg);
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(s.at.border_color, border);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
   });
   if let Some(ic) = b.icon_or_text(icon_spec.0, icon_spec.1, icon_spec.2, 16.5, icon_color) {
      btn = b.push_div(btn, ic);
   }
   b.hover_div(&mut btn, |s| {
      s.solid_background(if state == IconBtnState::Done {
         UI_SUCCESS_BG
      } else {
         UI_CONTROL_HOVER
      });
      s.hsla(
         s.at.border_color,
         if state == IconBtnState::Done {
            UI_SUCCESS_BORDER
         } else {
            UI_CONTROL_BORDER_HOVER
         },
      );
   });
   b.clickable(btn, id, action)
}

fn discord_popover_backdrop(b: &Builder) -> RawDiv {
   const POSITION_ABSOLUTE: u8 = 1;
   let mut backdrop = b.raw_div();
   b.style_div(&mut backdrop, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.lengths(&s.at.inset, gpui_px(0.0));
      s.lengths(&s.at.size, W_FULL);
   });
   let mut backdrop = b.clickable(backdrop, "sb-popover-backdrop", Action::ClosePopover);
   b.style_div(&mut backdrop, |s| s.set(s.at.mouse_cursor, 0));
   backdrop
}

fn discord_popover_shell(b: &Builder, width_rem: f32, right_rem: f32) -> RawDiv {
   const POSITION_ABSOLUTE: u8 = 1;
   let mut menu = b.v_stack(2.0);
   b.style_div(&mut menu, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.length(s.at.inset[0], gpui_rem(40.0));
      s.length(s.at.inset[1], gpui_rem(right_rem));
      s.length(s.at.size[0], gpui_rem(width_rem));
      s.solid_background(UI_POPOVER_BG);
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(s.at.border_color, UI_CONTROL_BORDER);
      s.lengths(&s.at.corner_radii, gpui_rem(POPOVER_RADIUS));
      s.lengths(&s.at.padding, gpui_rem(8.0));
   });
   menu
}

fn discord_radio_row(
   b: &Builder,
   id: &str,
   label: &str,
   selected: bool,
   action: Action,
) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let mut radio = b.h_row(0.0, false);
   b.style_div(&mut radio, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[0], gpui_rem(22.0));
      s.length(s.at.size[1], gpui_rem(22.0));
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, JUSTIFY_CENTER);
      s.lengths(&s.at.corner_radii, gpui_rem(99.0));
      if selected {
         s.solid_background(UI_BLURPLE);
      } else {
         s.lengths(&s.at.border_widths, gpui_px(1.5));
         s.hsla(s.at.border_color, UI_RADIO_RING);
      }
   });
   if selected {
      let mut inner = b.raw_div();
      b.style_div(&mut inner, |s| {
         s.length(s.at.size[0], gpui_rem(8.0));
         s.length(s.at.size[1], gpui_rem(8.0));
         s.solid_background(UI_WHITE);
         s.lengths(&s.at.corner_radii, gpui_rem(99.0));
      });
      radio = b.push_div(radio, inner);
   }

   let mut row = b.h_row(12.0, true);
   b.style_div(&mut row, |s| {
      s.length(s.at.size[0], W_FULL);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.length(s.at.padding[0], gpui_rem(9.0));
      s.length(s.at.padding[1], gpui_rem(12.0));
      s.length(s.at.padding[2], gpui_rem(9.0));
      s.length(s.at.padding[3], gpui_rem(12.0));
   });
   if let Some(txt) = b.ui_text_line(label, UI_TEXT, 14.5) {
      row = b.push_div(row, txt);
   }
   row = b.push_div(row, radio);
   b.hover_div(&mut row, |s| {
      s.solid_background(UI_ROW_HOVER);
   });
   b.clickable(row, id, action)
}

fn discord_checkbox_row(
   b: &Builder,
   id: &str,
   label: &str,
   checked: bool,
   action: Action,
) -> RawDiv {
   const ALIGN_CENTER: u8 = 4;
   const JUSTIFY_CENTER: u8 = 4;

   let mut checkbox = b.h_row(0.0, false);
   b.style_div(&mut checkbox, |s| {
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.size[0], gpui_rem(20.0));
      s.length(s.at.size[1], gpui_rem(20.0));
      s.set(s.at.align_items, ALIGN_CENTER);
      s.set(s.at.justify_content, JUSTIFY_CENTER);
      s.lengths(&s.at.corner_radii, gpui_rem(5.0));
      if checked {
         s.solid_background(UI_BLURPLE);
      } else {
         s.lengths(&s.at.border_widths, gpui_px(1.5));
         s.hsla(s.at.border_color, UI_RADIO_RING);
      }
   });
   if checked
      && let Some(ic) = b.icon_or_text(SVG_CHECK, &ICON_CHECK, "✓", 14.0, UI_WHITE)
   {
      checkbox = b.push_div(checkbox, ic);
   }

   let mut row = b.h_row(12.0, true);
   b.style_div(&mut row, |s| {
      s.length(s.at.size[0], W_FULL);
      s.lengths(&s.at.corner_radii, gpui_rem(CTRL_RADIUS));
      s.length(s.at.padding[0], gpui_rem(9.0));
      s.length(s.at.padding[1], gpui_rem(12.0));
      s.length(s.at.padding[2], gpui_rem(9.0));
      s.length(s.at.padding[3], gpui_rem(12.0));
   });
   if let Some(txt) = b.ui_text_line(label, UI_TEXT, 14.5) {
      row = b.push_div(row, txt);
   }
   row = b.push_div(row, checkbox);
   b.hover_div(&mut row, |s| {
      s.solid_background(UI_ROW_HOVER);
   });
   b.clickable(row, id, action)
}

fn render_store_filter_popover(b: &Builder, compat_mask: u8) -> RawDiv {
   let mut menu = discord_popover_shell(b, 232.0, 0.0);
   let mut hdr = b.h_row(0.0, false);
   b.style_div(&mut hdr, |s| {
      s.length(s.at.padding[0], gpui_rem(6.0));
      s.length(s.at.padding[1], gpui_rem(12.0));
      s.length(s.at.padding[2], gpui_rem(4.0));
      s.length(s.at.padding[3], gpui_rem(12.0));
   });
   if let Some(txt) = b.ui_text_line("Compatibility", UI_MUTED, 13.5) {
      hdr = b.push_div(hdr, txt);
   }
   menu = b.push_div(menu, hdr);

   let options: [(&str, &str, u8); 4] = [
      ("sb-compat-full", "Desktop Ready", COMPAT_FULL),
      ("sb-compat-partial", "Partial", COMPAT_PARTIAL),
      ("sb-compat-mobile", "Mobile Only", COMPAT_MOBILE),
      ("sb-compat-unverified", "Unverified", COMPAT_UNVERIFIED),
   ];
   for (id, label, bit) in options {
      let checked = (compat_mask & bit) != 0;
      menu = b.push_div(
         menu,
         discord_checkbox_row(b, id, label, checked, Action::ToggleCompatBit(bit)),
      );
   }
   menu
}

fn render_store_sort_popover(b: &Builder, sort: CatalogSort) -> RawDiv {
   let mut menu = discord_popover_shell(b, 210.0, 42.0);
   menu = b.push_div(
      menu,
      discord_radio_row(
         b,
         "sb-ssort-az",
         "Name (A-Z)",
         sort == CatalogSort::AlphaAsc,
         Action::SetCatalogSort(CatalogSort::AlphaAsc),
      ),
   );
   b.push_div(
      menu,
      discord_radio_row(
         b,
         "sb-ssort-za",
         "Name (Z-A)",
         sort == CatalogSort::AlphaDesc,
         Action::SetCatalogSort(CatalogSort::AlphaDesc),
      ),
   )
}

fn render_themes_placeholder(b: &Builder, root: RawDiv) -> RawDiv {
   let mut card = b.surface_card(8.0, 18.0, CARD_RADIUS);
   if let Some(title) = b.ui_text_line("Themes", UI_TEXT, 16.0) {
      card = b.push_div(card, title);
   }
   if let Some(desc) = b.ui_text_line("Desktop theme support isn't available yet.", UI_MUTED, 14.0) {
      card = b.push_div(card, desc);
   }
   let mut btn_row = b.h_row(0.0, false);
   b.style_div(&mut btn_row, |s| s.length(s.at.padding[0], gpui_rem(4.0)));
   btn_row = b.push_div(
      btn_row,
      discord_primary_btn(
         b,
         "sb-store-back-plugins",
         "Browse Plugins",
         None,
         Action::SetStoreKind(StoreKind::Plugins),
      ),
   );
   card = b.push_div(card, btn_row);
   b.push_div(root, card)
}

#[expect(
   clippy::too_many_arguments,
   clippy::too_many_lines,
   reason = "builds the Discord-native Store page with multiselect compat filter and clickable grid"
)]
fn render_store_tab(
   b: &Builder,
   mut root: RawDiv,
   store_kind: StoreKind,
   catalog: Catalog,
   installed: &HashSet<String>,
   filter: CatalogFilter,
   sort: CatalogSort,
   search: Option<(RawAnyElement, String)>,
   compat_mask: u8,
   popover: OpenPopover,
) -> RawDiv {
   let catalog_loading = matches!(catalog, Catalog::Loading);

   // In single-tab fallback mode, show Plugins / Themes switcher and refresh here.
   if !crate::settings::has_sidebar_category() {
      let mut sub_row = b.h_row(10.0, true);
      b.style_div(&mut sub_row, |s| s.length(s.at.size[0], W_FULL));
      sub_row = b.push_div(sub_row, render_store_kind_switch(b, store_kind));
      if store_kind == StoreKind::Plugins {
         sub_row = b.push_div(
            sub_row,
            discord_toolbar_icon_btn(
               b,
               "sb-store-refresh",
               (SVG_RELOAD, &ICON_RELOAD, "↻"),
               store_refresh_state(catalog_loading),
               Action::RefreshCatalog,
            ),
         );
      }
      root = b.push_div(root, sub_row);
   }

   if store_kind == StoreKind::Themes {
      return render_themes_placeholder(b, root);
   }

   let (search_el, query) = match search {
      Some((el, q)) => (Some(el), q.trim().to_ascii_lowercase()),
      None => (None, String::new()),
   };

   let mut left_controls = b.h_row(8.0, false);
   b.style_div(&mut left_controls, |s| s.set(s.at.flex_wrap, FLEX_WRAP));
   if let Some(mut el) = search_el {
      let mut search_wrap = b.v_stack(0.0);
      b.style_div(&mut search_wrap, |s| {
         s.length(s.at.size[0], gpui_rem(SEARCH_WIDTH));
         s.float(s.at.flex_shrink, 0.0);
      });
      if b.push_any(&mut search_wrap, &mut el) {
         left_controls = b.push_div(left_controls, search_wrap);
      }
   }

   let seg = discord_segmented(
      b,
      &[
         (
            "sb-store-all",
            "All",
            filter == CatalogFilter::All,
            Action::SetCatalogFilter(CatalogFilter::All),
         ),
         (
            "sb-store-avail",
            "Available",
            filter == CatalogFilter::Available,
            Action::SetCatalogFilter(CatalogFilter::Available),
         ),
         (
            "sb-store-inst",
            "Installed",
            filter == CatalogFilter::Installed,
            Action::SetCatalogFilter(CatalogFilter::Installed),
         ),
      ],
   );
   left_controls = b.push_div(left_controls, seg);

   let sort_open = popover == OpenPopover::StoreSort;
   let filter_open = popover == OpenPopover::StoreFilter;
   let is_filtered = (compat_mask & COMPAT_ALL) != COMPAT_ALL;
   let filter_btn_state = if filter_open {
      IconBtnState::Open
   } else if is_filtered {
      IconBtnState::Active
   } else {
      IconBtnState::Idle
   };

   let sort_btn = discord_toolbar_btn(
      b,
      "sb-store-sort",
      "Sort",
      Some((SVG_SORT, &ICON_SORT, "⇅")),
      sort_open,
      Action::TogglePopover(OpenPopover::StoreSort),
   );
   let filter_btn = discord_toolbar_icon_btn(
      b,
      "sb-store-filter-btn",
      (SVG_FILTER, &ICON_FILTER, "≡"),
      filter_btn_state,
      Action::TogglePopover(OpenPopover::StoreFilter),
   );

   // Match Discord's toolbar order: `[↑↓ Sort]` then `[≡]` square icon button on the right.
   let mut right_controls = b.h_row(8.0, false);
   b.style_div(&mut right_controls, |s| s.float(s.at.flex_shrink, 0.0));
   right_controls = b.push_div(right_controls, sort_btn);
   right_controls = b.push_div(right_controls, filter_btn);

   let mut toolbar = b.h_row(10.0, true);
   b.style_div(&mut toolbar, |s| {
      s.length(s.at.size[0], W_FULL);
      s.set(s.at.flex_wrap, FLEX_WRAP);
   });
   toolbar = b.push_div(toolbar, left_controls);
   toolbar = b.push_div(toolbar, right_controls);

   // Relative container holding toolbar + grid + floating Sort/Filter popover on top.
   let mut content = b.v_stack(14.0);
   b.style_div(&mut content, |s| s.length(s.at.size[0], W_FULL));
   content = b.push_div(content, toolbar);

   match catalog {
      Catalog::Unfetched => {
         update::fetch_catalog();
         let mut box_div = b.surface_card(4.0, 16.0, CARD_RADIUS);
         if let Some(txt) = b.ui_text_line("Loading plugins...", UI_MUTED, 14.0) {
            box_div = b.push_div(box_div, txt);
         }
         content = b.push_div(content, box_div);
      },
      Catalog::Loading => {
         let msg = if MANUAL_CATALOG_REFRESH.load(Ordering::Acquire) {
            "Refreshing store..."
         } else {
            "Loading plugins..."
         };
         let mut box_div = b.surface_card(4.0, 16.0, CARD_RADIUS);
         if let Some(txt) = b.ui_text_line(msg, UI_MUTED, 14.0) {
            box_div = b.push_div(box_div, txt);
         }
         content = b.push_div(content, box_div);
      },
      Catalog::Failed(err) => {
         let mut box_div = b.surface_card(6.0, 16.0, CARD_RADIUS);
         box_div = b.push_div(
            box_div,
            b.action_row(
               "sunbird-catalog-retry",
               "Could not reach Store",
               &err,
               "Retry",
               BtnVariant::Primary,
               BtnSize::Small,
               Action::RefreshCatalog,
            ),
         );
         content = b.push_div(content, box_div);
      },
      Catalog::Loaded(items) => {
         let installing = INSTALLING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
         let mut visible: Vec<&Listed> = items
            .iter()
            .filter(|item| {
               let bit = compat_bit(DesktopCompat::of(&item.name));
               if (compat_mask & bit) == 0 {
                  return false;
               }
               let is_inst = installed.contains(&item.name);
               let matches_filter = match filter {
                  CatalogFilter::All => true,
                  CatalogFilter::Available => !is_inst,
                  CatalogFilter::Installed => is_inst,
               };
               if !matches_filter {
                  return false;
               }
               if query.is_empty() {
                  return true;
               }
               item.name.to_ascii_lowercase().contains(&query)
                  || item.description.to_ascii_lowercase().contains(&query)
            })
            .collect();

         match sort {
            CatalogSort::AlphaAsc => {
               visible.sort_by_key(|a| a.name.to_ascii_lowercase());
            },
            CatalogSort::AlphaDesc => {
               visible.sort_by_key(|a| std::cmp::Reverse(a.name.to_ascii_lowercase()));
            },
         }

         if visible.is_empty() {
            let mut empty = b.surface_card(10.0, 18.0, CARD_RADIUS);
            let msg = if compat_mask == 0 {
               "No compatibility filters selected."
            } else if !query.is_empty() {
               "No plugins match your search."
            } else {
               "No plugins match your filters."
            };
            if let Some(txt) = b.ui_text_line(msg, UI_MUTED, 14.0) {
               empty = b.push_div(empty, txt);
            }
            if (compat_mask != COMPAT_ALL || filter != CatalogFilter::All) && query.is_empty() {
               let mut row = b.h_row(0.0, false);
               row = b.push_div(
                  row,
                  discord_toolbar_btn(
                     b,
                     "sb-compat-reset-empty",
                     "Reset filters",
                     None,
                     false,
                     Action::ResetStoreFilters,
                  ),
               );
               empty = b.push_div(empty, row);
            }
            content = b.push_div(content, empty);
         } else {
            let mut grid = b.v_stack(10.0);
            b.style_div(&mut grid, |s| s.length(s.at.size[0], W_FULL));
            for pair in visible.chunks(2) {
               // Plain flex row (default align-items: stretch) so paired cards match height.
               let mut row = chain(b.sym.div_flex, b.raw_div());
               b.style_div(&mut row, |s| {
                  s.length(s.at.size[0], W_FULL);
                  s.lengths(&s.at.gap, gpui_rem(10.0));
               });
               for item in pair {
                  let is_installed = installed.contains(&item.name);
                  let is_installing = installing.contains(&item.name);
                  row = b.push_div(row, render_store_card(b, item, is_installed, is_installing));
               }
               if pair.len() == 1 {
                  let mut spacer = b.raw_div();
                  b.style_div(&mut spacer, |s| {
                     s.length(s.at.size[0], W_FULL);
                     s.length(s.at.min_size[0], gpui_px(0.0));
                     s.float(s.at.flex_shrink, 1.0);
                  });
                  row = b.push_div(row, spacer);
               }
               grid = b.push_div(grid, row);
            }
            content = b.push_div(content, grid);
         }
      },
   }

   match popover {
      OpenPopover::StoreFilter => {
         content = b.push_div(content, discord_popover_backdrop(b));
         content = b.push_div(content, render_store_filter_popover(b, compat_mask));
      },
      OpenPopover::StoreSort => {
         content = b.push_div(content, discord_popover_backdrop(b));
         content = b.push_div(content, render_store_sort_popover(b, sort));
      },
      OpenPopover::None | OpenPopover::PluginsSort => {},
   }

   b.push_div(root, content)
}

#[expect(
   clippy::too_many_lines,
   reason = "builds the Developer and Updates grouped cards in one pass"
)]
fn render_general_tab(b: &Builder, mut root: RawDiv) -> RawDiv {
   // Section 1: Developer
   let mut dev_sec = b.v_stack(6.0);
   b.style_div(&mut dev_sec, |s| s.length(s.at.size[0], W_FULL));
   dev_sec = b.push_div(dev_sec, b.section_label("DEVELOPER"));

   let mut dev_card = b.surface_card(0.0, 0.0, CARD_RADIUS);
   let pad_row = |inner: RawDiv| {
      let mut wrap = b.v_stack(0.0);
      b.style_div(&mut wrap, |s| {
         s.length(s.at.size[0], W_FULL);
         s.length(s.at.padding[0], gpui_rem(12.0));
         s.length(s.at.padding[1], gpui_rem(16.0));
         s.length(s.at.padding[2], gpui_rem(12.0));
         s.length(s.at.padding[3], gpui_rem(16.0));
      });
      b.push_div(wrap, inner)
   };

   dev_card = b.push_div(
      dev_card,
      pad_row(b.switch_row(
         "sunbird-staff-unlock",
         "Staff Unlock",
         Some("Unlock Discord staff-only settings, experiments, and internal developer menus."),
         phoenix_runtime::staff_enabled(),
         Action::ToggleStaff,
      )),
   );
   dev_card = b.push_div(dev_card, b.divider());
   dev_card = b.push_div(
      dev_card,
      pad_row(b.action_row(
         "sunbird-open-devtools",
         "Runtime Console & DevTools",
         &format!(
            "Evaluate JavaScript in QuickJS and view live runtime logs ({}).",
            phoenix_ui_desktop::DEVTOOLS_SHORTCUT
         ),
         "Open DevTools",
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::OpenDevTools,
      )),
   );
   dev_card = b.push_div(dev_card, b.divider());
   let reload_lbl = if RELOADING_ALL.load(Ordering::Acquire) {
      "Reloading..."
   } else if is_flashed("sb-reload-all") {
      "Reloaded"
   } else {
      "Reload All"
   };
   dev_card = b.push_div(
      dev_card,
      pad_row(b.action_row(
         "sunbird-reload-all",
         "Reload All Plugins",
         "Re-evaluate every currently enabled plugin without restarting Discord.",
         reload_lbl,
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::ReloadAll,
      )),
   );
   dev_sec = b.push_div(dev_sec, dev_card);
   root = b.push_div(root, dev_sec);

   // Section 2: Updates
   let (upd_desc, upd_badge, upd_btn, upd_var, upd_act) = updates_card_state();
   let mut upd_sec = b.v_stack(6.0);
   b.style_div(&mut upd_sec, |s| s.length(s.at.size[0], W_FULL));
   upd_sec = b.push_div(upd_sec, b.section_label("UPDATES"));

   let mut upd_title = b.h_row(8.0, false);
   if let Some(t) = b.ui_text_line("Sunbird", UI_TEXT, 15.5) {
      upd_title = b.push_div(upd_title, t);
   }
   if let Some((badge_txt, badge_bg, badge_fg)) = upd_badge
      && let Some(mut pill) = b.ui_text_line(badge_txt, badge_fg, 12.0)
   {
      b.style_div(&mut pill, |s| {
         s.solid_background(badge_bg);
         s.lengths(&s.at.corner_radii, gpui_rem(99.0));
         s.length(s.at.padding[0], gpui_rem(2.0));
         s.length(s.at.padding[1], gpui_rem(8.0));
         s.length(s.at.padding[2], gpui_rem(2.0));
         s.length(s.at.padding[3], gpui_rem(8.0));
      });
      upd_title = b.push_div(upd_title, pill);
   }

   let mut upd_left = b.v_stack(3.0);
   b.style_div(&mut upd_left, |s| {
      s.length(s.at.min_size[0], gpui_px(0.0));
      s.float(s.at.flex_shrink, 1.0);
   });
   upd_left = b.push_div(upd_left, upd_title);
   if let Some(d) = b.ui_text_line(&upd_desc, UI_MUTED, 13.5) {
      upd_left = b.push_div(upd_left, d);
   }

   let upd_btn_div = if matches!(upd_var, BtnVariant::Primary) {
      discord_primary_btn(b, "sb-updates-action", upd_btn, None, upd_act)
   } else {
      discord_toolbar_btn(
         b,
         "sb-updates-action",
         upd_btn,
         None,
         update::busy(),
         upd_act,
      )
   };

   let mut upd_row = b.h_row(12.0, true);
   b.style_div(&mut upd_row, |s| {
      s.length(s.at.size[0], W_FULL);
      s.length(s.at.padding[0], gpui_rem(13.0));
      s.length(s.at.padding[1], gpui_rem(16.0));
      s.length(s.at.padding[2], gpui_rem(13.0));
      s.length(s.at.padding[3], gpui_rem(16.0));
   });
   upd_row = b.push_div(upd_row, upd_left);
   upd_row = b.push_div(upd_row, upd_btn_div);

   let mut upd_card = b.surface_card(0.0, 0.0, CARD_RADIUS);
   upd_card = b.push_div(upd_card, upd_row);
   upd_sec = b.push_div(upd_sec, upd_card);
   b.push_div(root, upd_sec)
}

type UpdateBadge = (&'static str, [f32; 4], [f32; 4]);

fn updates_card_state() -> (String, Option<UpdateBadge>, &'static str, BtnVariant, Action) {
   let status = update::status();
   if update::busy() {
      return (
         status.unwrap_or_else(|| "Checking for updates...".to_owned()),
         None,
         "Working...",
         BtnVariant::Secondary,
         Action::CheckUpdates,
      );
   }
   if let Some(found) = update::found() {
      if !found.plugins.is_empty() {
         let desc = match &found.loader {
            Some(version) => {
               format!(
                  "Plugin updates ready: {} · Sunbird {version} available (restart to update)",
                  found.plugins.join(", ")
               )
            },
            None => format!("Plugin updates ready: {}", found.plugins.join(", ")),
         };
         return (
            desc,
            Some(("Update ready", UI_BLURPLE_SOFT, UI_BLURPLE_TEXT)),
            "Apply Updates",
            BtnVariant::Primary,
            Action::ApplyUpdates,
         );
      }
      if let Some(version) = found.loader {
         return (
            format!("Sunbird {version} is available, restart Sunbird to update."),
            Some(("Restart required", UI_BLURPLE_SOFT, UI_BLURPLE_TEXT)),
            "Check for Updates",
            BtnVariant::Secondary,
            Action::CheckUpdates,
         );
      }
   }
   (
      status.unwrap_or_else(|| format!("v{}", update::VERSION)),
      Some(("Up to date", UI_SUCCESS_BG, UI_SUCCESS)),
      "Check for Updates",
      BtnVariant::Secondary,
      Action::CheckUpdates,
   )
}
