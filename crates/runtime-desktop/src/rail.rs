//! A column beside the server rail with the servers of the open folders, which
//! the rail then draws closed. On while a plugin holds a
//! `phoenix.ui.folderColumn` handle.

use std::{
   cell::Cell,
   mem::MaybeUninit,
   ptr,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU32,
         Ordering,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_hook::{
   image::Image,
   x86::Abi,
};
use phoenix_sigs::{
   desktop,
   index::Index,
};

use crate::gpui::{
   self,
   RawAnyElement,
   RawDiv,
};

type RenderFn =
   unsafe extern "C" fn(sret: *mut u8, rail: *mut u8, window: usize, cx: usize) -> *mut u8;
type ItemFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   rail: *mut u8,
   index: usize,
   a3: u32,
   a4: u8,
   window: usize,
   cx: usize,
) -> *mut RawAnyElement;

type RequestLayoutFn = unsafe extern "C" fn(
   sret: *mut u8,
   view: *mut u8,
   id: usize,
   inspector_id: usize,
   window: usize,
   cx: usize,
) -> *mut u8;

struct Hooks {
   layout:         desktop::Rail,
   render:         RenderFn,
   item:           ItemFn,
   view:           desktop::AnyViewLayout,
   request_layout: RequestLayoutFn,
   list:           Option<(desktop::RailList, RemeasureFn)>,
}

type RemeasureFn = unsafe extern "C" fn(list: *mut u8);
type PanelFn = unsafe extern "C" fn(
   sret: usize,
   panel: usize,
   a2: usize,
   a3: usize,
   a4: usize,
   a5: usize,
) -> usize;
type SidebarWidthFn = unsafe extern "C" fn(window: usize, cx: usize) -> f32;

/// The type name of the cached view that holds the rail.
const RAIL_VIEW: &[u8] = b"discord_desktop::ui::rail::view::RailView";
const COLUMN_PX: f32 = 72.0;
const SLIDE: Duration = Duration::from_millis(180);

static HOOKS: OnceLock<Hooks> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
/// How far the column has slid open at the last rail render, from 0 to 1, as
/// `f32` bits. The rail's view and the user panel lay out that much wider.
static SHOWN: AtomicU32 = AtomicU32::new(0);
/// The uneased slide, its direction, and when it last moved.
static SLIDING: Mutex<(f32, bool, Option<Instant>)> = Mutex::new((0.0, false, None));
static PANEL: OnceLock<PanelFn> = OnceLock::new();
static SIDEBAR_WIDTH: OnceLock<SidebarWidthFn> = OnceLock::new();
/// Set when the column turns on or off. The rail's list keeps the height it
/// measured for each entry until that entry changes, so the folders open
/// at the time keep their old height until the list forgets them all.
static REMEASURE: AtomicBool = AtomicBool::new(false);

thread_local! {
   /// Set while the column draws a folder, which then renders open.
   static IN_COLUMN: Cell<bool> = const { Cell::new(false) };
   /// Set while the user panel renders, whose width spans the column too.
   static IN_PANEL: Cell<bool> = const { Cell::new(false) };
}

/// Hooks the rail from [`crate::prepare`], before Discord's threads run.
pub fn install(image: &Image<'static>, ix: &Index<'_>) {
   if !gpui::locate(image, ix) {
      log::warn!("rail: no GPUI builders, no folder column");
      return;
   }
   let layout = match desktop::rail(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("rail: {err}, no folder column");
         return;
      },
   };
   // The caller boxes the render's result with an `into_any_element` that is
   // `Div`'s copy from another codegen unit.
   let (Some(div_new), Some(div_into_any)) = (gpui::div_new_addr(), gpui::div_into_any_addr())
   else {
      return;
   };
   if !desktop::calls(image, layout.render, div_new)
      || !desktop::same_generic(image, layout.render_into, div_into_any)
   {
      log::warn!("rail: the rail render no longer returns a Div, no folder column");
      return;
   }
   let view = match desktop::any_view_layout(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("rail: {err}, no folder column");
         return;
      },
   };
   // Read before the hooks rewrite the render it decodes.
   let list = desktop::rail_list(image, &layout)
      .inspect_err(|err| log::warn!("rail: {err}, open folders keep their height when toggled"))
      .ok()
      // SAFETY: the locator checked the method takes only the list.
      .map(|list| {
         (list, unsafe {
            std::mem::transmute::<usize, RemeasureFn>(list.remeasure)
         })
      });
   let panel = desktop::user_panel(image)
      .inspect_err(|err| log::warn!("rail: {err}, the user panel keeps its width"))
      .ok();
   let prologues = desktop::Prologues::of(Abi::of(image));
   let hook_with = |prologue: desktop::Prologue, func: usize, detour: usize| {
      // SAFETY: runs before Discord's threads, and each detour has its
      // target's ABI.
      unsafe { image.hook_entry(func, prologue.pattern, prologue.len, detour) }
   };
   let hook = |func: usize, detour: usize| hook_with(prologues.frame, func, detour);
   // Without a wider view the column is clipped away, so it goes first.
   let request_layout = match hook_with(
      prologues.small_frame,
      view.request_layout,
      request_view_layout as *const () as usize,
   ) {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("rail: {err}");
         return;
      },
   };
   let item = match hook(layout.item, render_item as *const () as usize) {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("rail: {err}");
         return;
      },
   };
   let render = match hook(layout.render, render_rail as *const () as usize) {
      Ok(trampoline) => trampoline,
      Err(err) => {
         // The item hook alone only consults `ENABLED`, which stays off.
         log::warn!("rail: {err}");
         return;
      },
   };
   // SAFETY: the trampolines replay the displaced prologues.
   let hooks = unsafe {
      Hooks {
         layout,
         render: std::mem::transmute::<usize, RenderFn>(render),
         item: std::mem::transmute::<usize, ItemFn>(item),
         view,
         request_layout: std::mem::transmute::<usize, RequestLayoutFn>(request_layout),
         list,
      }
   };
   let _ = HOOKS.set(hooks);
   if let Some(panel) = panel {
      hook_panel(image, panel, prologues.frame);
   }
   log::info!(
      "rail: hooked render @ {:#x}, items @ {:#x}",
      layout.render,
      layout.item
   );
}

/// Widens the user panel with the column: its render reads the channel list's
/// width through a GOT slot that the other layouts share, so the slot's getter
/// adds the column only inside the panel's render.
fn hook_panel(image: &Image<'static>, panel: desktop::UserPanel, frame: desktop::Prologue) {
   let Some(original) = image.read_usize(panel.sidebar_slot) else {
      return;
   };
   // SAFETY: `sigs` checked the slot holds the `(window, cx) -> f32` getter.
   let _ = SIDEBAR_WIDTH.set(unsafe { std::mem::transmute::<usize, SidebarWidthFn>(original) });
   // SAFETY: runs before Discord's threads, and the detour has the render's
   // ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         panel.render,
         frame.pattern,
         frame.len,
         render_panel as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("rail: {err}, the user panel keeps its width");
         return;
      },
   };
   // SAFETY: the trampoline replays the displaced prologue.
   let _ = PANEL.set(unsafe { std::mem::transmute::<usize, PanelFn>(trampoline) });
   let detour = sidebar_width as *const () as usize;
   // SAFETY: a GOT slot of this image, and the detour forwards to the getter
   // it held.
   if let Err(err) = unsafe { image.swap_words(&[(panel.sidebar_slot, detour)]) } {
      log::warn!("rail: {err}, the user panel keeps its width");
   }
}

unsafe extern "C" fn render_panel(
   sret: usize,
   panel: usize,
   a2: usize,
   a3: usize,
   a4: usize,
   a5: usize,
) -> usize {
   let Some(render) = PANEL.get() else {
      return sret;
   };
   IN_PANEL.set(true);
   // SAFETY: forwards Discord's arguments unchanged.
   let out = unsafe { render(sret, panel, a2, a3, a4, a5) };
   IN_PANEL.set(false);
   out
}

unsafe extern "C" fn sidebar_width(window: usize, cx: usize) -> f32 {
   let Some(getter) = SIDEBAR_WIDTH.get() else {
      return 0.0;
   };
   // SAFETY: forwards Discord's arguments unchanged.
   let width = unsafe { getter(window, cx) };
   if IN_PANEL.get() {
      COLUMN_PX.mul_add(shown(), width)
   } else {
      width
   }
}

fn shown() -> f32 {
   f32::from_bits(SHOWN.load(Ordering::Acquire))
}

/// Moves the slide toward `open` by the time since the last render, and
/// returns it eased with whether it still moves.
fn slide(open: bool) -> (f32, bool) {
   let now = Instant::now();
   let raw = {
      let mut sliding = SLIDING.lock().unwrap_or_else(PoisonError::into_inner);
      let (raw, was_open, last) = *sliding;
      // A cached view renders rarely at rest, so a slide that starts or turns
      // around starts its clock now.
      let at_rest = if was_open { raw >= 1.0 } else { raw <= 0.0 };
      let last = last.filter(|_| was_open == open && !at_rest).unwrap_or(now);
      let step = now.duration_since(last).as_secs_f32() / SLIDE.as_secs_f32();
      let raw = if open {
         (raw + step).min(1.0)
      } else {
         (raw - step).max(0.0)
      };
      *sliding = (raw, open, Some(now));
      raw
   };
   let moving = if open { raw < 1.0 } else { raw > 0.0 };
   (raw * raw * 2.0f32.mul_add(-raw, 3.0), moving)
}

/// Turns the folder column on or off and redraws Discord.
pub fn enable(on: bool) {
   if !on {
      SHOWN.store(0, Ordering::Release);
      *SLIDING.lock().unwrap_or_else(PoisonError::into_inner) = (0.0, false, None);
   }
   if ENABLED.swap(on, Ordering::AcqRel) != on {
      REMEASURE.store(true, Ordering::Release);
      crate::accessories::invalidate_discord_windows();
   }
}

/// The ids in the `HashSet<u64>` whose raw table starts at `table`.
///
/// # Safety
///
/// `table` must point at a live `hashbrown` table of `u64`s.
#[expect(clippy::cast_ptr_alignment, reason = "read unaligned")]
unsafe fn set_ids(table: *const u8) -> Vec<u64> {
   // SAFETY: the caller hands a live table, `[ctrl, bucket_mask, ..]`.
   unsafe {
      let ctrl = table.cast::<*const u8>().read_unaligned();
      let mask = table.add(8).cast::<usize>().read_unaligned();
      if ctrl.is_null() || !(mask + 1).is_power_of_two() || mask > 0xFFFF {
         return Vec::new();
      }
      (0..=mask)
         .filter(|&slot| *ctrl.add(slot) & 0x80 == 0)
         .map(|slot| ctrl.cast::<u64>().sub(slot + 1).read_unaligned())
         .collect()
   }
}

/// The tile of rail entry `index`, when it is a folder.
///
/// # Safety
///
/// `rail` must be the live `Rail` the hooks were handed.
unsafe fn tile_at(layout: &desktop::Rail, rail: *mut u8, index: usize) -> Option<*mut u8> {
   // SAFETY: as in `folder_at`.
   unsafe {
      folder_at(layout, rail, index)?;
      let field = |at: usize| rail.add(at).cast::<usize>().read_unaligned();
      let entry = (field(layout.items.0) as *const u8).add(index * 16);
      let tile_index = entry.add(8).cast::<usize>().read_unaligned();
      Some((field(layout.tiles.0) as *mut u8).add(tile_index * layout.tiles.2))
   }
}

/// The folder id of rail entry `index`, when it is a folder.
///
/// # Safety
///
/// `rail` must be the live `Rail` the hooks were handed.
unsafe fn folder_at(layout: &desktop::Rail, rail: *const u8, index: usize) -> Option<u64> {
   // SAFETY: the offsets were decoded from the item renderer, which reads them
   // the same way and bounds-checks the same lengths.
   unsafe {
      let field = |at: usize| rail.add(at).cast::<usize>().read_unaligned();
      let (items, items_len) = (field(layout.items.0), field(layout.items.1));
      if index >= items_len {
         return None;
      }
      let entry = (items as *const u8).add(index * 16);
      if entry.cast::<u32>().read_unaligned() != desktop::RAIL_TILE {
         return None;
      }
      let tile_index = entry.add(8).cast::<usize>().read_unaligned();
      let (tiles, tiles_len, size) = layout.tiles;
      if tile_index >= field(tiles_len) {
         return None;
      }
      let tile = (field(tiles) as *const u8).add(tile_index * size);
      // Servers hold `u64::MAX` here, and `u64::MAX - 1` is the `None` niche.
      if tile.cast::<u64>().read_unaligned() >= u64::MAX - 1 {
         return None;
      }
      Some(tile.add(layout.folder_id).cast::<u64>().read_unaligned())
   }
}

/// Entries of the open folders, in rail order.
///
/// # Safety
///
/// `rail` must be the live `Rail` the hooks were handed.
unsafe fn open_folders(layout: &desktop::Rail, rail: *const u8) -> Vec<usize> {
   // SAFETY: as in `folder_at`, from the same `Rail`.
   unsafe {
      let open = set_ids(rail.add(layout.open_folders));
      if open.is_empty() {
         return Vec::new();
      }
      let len = rail.add(layout.items.1).cast::<usize>().read_unaligned();
      (0..len)
         .filter(|&index| folder_at(layout, rail, index).is_some_and(|id| open.contains(&id)))
         .collect()
   }
}

unsafe extern "C" fn render_item(
   sret: *mut RawAnyElement,
   rail: *mut u8,
   index: usize,
   a3: u32,
   a4: u8,
   window: usize,
   cx: usize,
) -> *mut RawAnyElement {
   let hooks = HOOKS.wait();
   let forward = || {
      // SAFETY: forwards Discord's arguments unchanged.
      unsafe { (hooks.item)(sret, rail, index, a3, a4, window, cx) }
   };
   if !ENABLED.load(Ordering::Acquire) || IN_COLUMN.get() {
      return forward();
   }
   // SAFETY: Discord hands its live `Rail`.
   let Some(len) = (unsafe { open_folder_len(&hooks.layout, rail, index) }) else {
      return forward();
   };
   // The rest of the rail's render still sees the folder's servers, since it
   // lists the shown servers from them and Discord ignores clicks on others.
   // SAFETY: empties the folder only for its own draw, and puts it back.
   unsafe {
      let saved = len.replace(0);
      let out = forward();
      len.write(saved);
      out
   }
}

/// The server count of rail entry `index`, when it is an open folder. Zero, it
/// draws the open folder's icon at a closed folder's height.
///
/// # Safety
///
/// `rail` must be the live `Rail` the hooks were handed.
#[expect(
   clippy::cast_ptr_alignment,
   reason = "tiles are `Vec` elements holding `u64`s"
)]
unsafe fn open_folder_len(
   layout: &desktop::Rail,
   rail: *mut u8,
   index: usize,
) -> Option<*mut usize> {
   // SAFETY: as in `folder_at`.
   unsafe {
      let id = folder_at(layout, rail, index)?;
      if !set_ids(rail.add(layout.open_folders)).contains(&id) {
         return None;
      }
      let tile = tile_at(layout, rail, index)?;
      Some(tile.add(layout.folder_len).cast::<usize>())
   }
}

/// Lays the rail's cached view out as wide as the column has slid open, so the
/// rest of the window makes room for it.
#[expect(clippy::cast_ptr_alignment, reason = "read and written unaligned")]
unsafe extern "C" fn request_view_layout(
   sret: *mut u8,
   view: *mut u8,
   id: usize,
   inspector_id: usize,
   window: usize,
   cx: usize,
) -> *mut u8 {
   let hooks = HOOKS.wait();
   // SAFETY: forwards Discord's arguments unchanged.
   let forward = || unsafe { (hooks.request_layout)(sret, view, id, inspector_id, window, cx) };
   let Some(width_at) = gpui::layout().map(|layout| layout.style.size[0]) else {
      return forward();
   };
   let extra = COLUMN_PX * shown();
   if extra <= 0.0 {
      return forward();
   }
   // SAFETY: `view` is the live `AnyView` being laid out, its fields at the
   // offsets `sigs` decoded from this function, and its type name a
   // `&'static str`.
   unsafe {
      let style = view.add(hooks.view.cached_style);
      if style.cast::<u32>().read_unaligned() == hooks.view.uncached {
         return forward();
      }
      let name = view
         .add(hooks.view.type_name)
         .cast::<(*const u8, usize)>()
         .read_unaligned();
      if name.1 != RAIL_VIEW.len() || std::slice::from_raw_parts(name.0, name.1) != RAIL_VIEW {
         return forward();
      }
      let width = style.add(width_at).cast::<u64>();
      let saved = width.read_unaligned();
      // Only `Some(Pixels(_))`, whose tag is the low word's zero.
      if saved & 0xFFFF_FFFF != 0 {
         static ONCE: std::sync::Once = std::sync::Once::new();
         ONCE.call_once(|| log::warn!("rail: the rail's view width is not in pixels"));
         return forward();
      }
      let px = f32::from_bits((saved >> 32) as u32);
      width.write_unaligned(gpui::gpui_px(px + extra));
      let out = forward();
      width.write_unaligned(saved);
      out
   }
}

unsafe extern "C" fn render_rail(
   sret: *mut u8,
   rail: *mut u8,
   window: usize,
   cx: usize,
) -> *mut u8 {
   let hooks = HOOKS.wait();
   // SAFETY: forwards Discord's arguments unchanged.
   let forward = || unsafe { (hooks.render)(sret, rail, window, cx) };
   if let Some((list, remeasure)) = hooks.list
      && REMEASURE.swap(false, Ordering::AcqRel)
   {
      // SAFETY: Discord hands its live `Rail`, whose list field points at its
      // `RefCell`d state. The method panics on a held borrow, so a borrowed
      // list waits for the next render.
      unsafe {
         let field = rail.add(list.field);
         let state = field.cast::<*const u8>().read_unaligned();
         if !state.is_null() && state.add(list.borrow).cast::<usize>().read_unaligned() == 0 {
            remeasure(field);
         } else {
            REMEASURE.store(true, Ordering::Release);
         }
      }
   }
   let Some(size) = gpui::layout().map(|layout| layout.div.size) else {
      return forward();
   };
   if !ENABLED.load(Ordering::Acquire) {
      return forward();
   }
   let out = forward();
   // SAFETY: Discord hands its live `Rail`.
   let open = unsafe { open_folders(&hooks.layout, rail) };
   let (shown, moving) = slide(!open.is_empty());
   // The rail's view and the user panel were laid out at the old width before
   // this render.
   if SHOWN.swap(shown.to_bits(), Ordering::AcqRel) != shown.to_bits() || moving {
      crate::accessories::invalidate_discord_windows();
   }
   if shown <= 0.0 {
      return out;
   }
   let (Some(mut column), Some(mut clip), Some(mut row)) = (
      gpui::v_stack(8.0),
      gpui::h_row(0.0, false),
      gpui::h_row(0.0, false),
   ) else {
      return out;
   };
   gpui::style_div(&mut column, |s| {
      let [top, _, bottom, _] = s.at.padding;
      s.length(top, gpui::gpui_px(12.0));
      s.length(bottom, gpui::gpui_px(12.0));
      s.length(s.at.size[0], gpui::gpui_px(COLUMN_PX));
      s.length(s.at.size[1], gpui::W_FULL);
      s.float(s.at.flex_shrink, 0.0);
      s.length(s.at.border_widths[3], gpui::gpui_px(1.0));
      s.hsla(s.at.border_color, [0.0, 0.0, 1.0, 0.06]);
   });
   for index in open {
      let mut element = MaybeUninit::<RawAnyElement>::uninit();
      IN_COLUMN.set(true);
      // SAFETY: the same call the rail's list makes, with this render's window
      // and context.
      unsafe { (hooks.item)(element.as_mut_ptr(), rail, index, 0, 0, window, cx) };
      IN_COLUMN.set(false);
      // SAFETY: the renderer wrote an `AnyElement`.
      let mut element = unsafe { element.assume_init() };
      gpui::push_any(&mut column, &mut element);
   }
   // An id keeps the column's own scroll offset, apart from the rail's.
   if crate::modal::set_id(&mut column, "sunbird-folder-column") {
      const OVERFLOW_SCROLL: u8 = 3;
      gpui::style_div(&mut column, |s| s.set(s.at.overflow_y, OVERFLOW_SCROLL));
   }
   // The column keeps its width and slides out from under the clip.
   gpui::style_div(&mut clip, |s| {
      const OVERFLOW_HIDDEN: u8 = 2;
      s.set(s.at.overflow_x, OVERFLOW_HIDDEN);
      s.length(s.at.size[0], gpui::gpui_px(COLUMN_PX * shown));
      s.length(s.at.size[1], gpui::W_FULL);
      s.float(s.at.flex_shrink, 0.0);
   });
   gpui::style_div(&mut row, |s| {
      const ALIGN_STRETCH: u8 = 6;
      s.set(s.at.align_items, ALIGN_STRETCH);
      s.length(s.at.size[1], gpui::W_FULL);
      s.float(s.at.flex_shrink, 0.0);
   });
   // SAFETY: the original wrote the rail's `size`-byte `Div` into `sret`,
   // which takes the row in its place. `sigs` checks `size` fits a `RawDiv`.
   unsafe {
      let mut rail_div = RawDiv::zeroed();
      ptr::copy_nonoverlapping(sret, rail_div.as_mut_ptr(), size);
      gpui::style_div(&mut rail_div, |s| s.float(s.at.flex_shrink, 0.0));
      let mut row = gpui::push_div(gpui::push_div(row, rail_div), gpui::push_div(clip, column));
      ptr::copy_nonoverlapping(row.as_mut_ptr(), sret, size);
   }
   out
}
