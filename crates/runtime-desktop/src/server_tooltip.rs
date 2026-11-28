//! Rows plugins add at the bottom of a server's tooltip in the server list
//! (`phoenix.ui.addServerTooltip`).
//!
//! The detour on what draws the bottom of the tooltip, the voice channels and
//! streams that [`server_tooltip::tooltip`] finds, puts the plugins' rows
//! under it, or alone where the server has none. Each row is its pieces side
//! by side: icons or colored text. Plugins render on a worker thread and what
//! they return is kept per server, so the tooltip never waits on them.

use std::{
   collections::HashMap,
   ffi::c_void,
   sync::{
      LazyLock,
      OnceLock,
      RwLock,
      atomic::{
         AtomicIsize,
         AtomicU64,
         AtomicUsize,
         Ordering,
      },
      mpsc,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::{
   index::Index,
   locate::server_tooltip,
};

use crate::{
   accessories::invalidate_discord_windows,
   gpui::{
      self,
      Piece,
      RawAnyElement,
   },
   members::MIN_SNOWFLAKE,
};

/// `fn(out: &mut Option<AnyElement>, &self, window, cx) -> out`.
type RenderFn = unsafe extern "C" fn(
   *mut RawAnyElement,
   *const u8,
   *mut c_void,
   *mut c_void,
) -> *mut RawAnyElement;

/// The rows' text, a little larger and bolder than the tooltip's own lines.
const FONT_PX: f32 = 16.0;
const FONT_WEIGHT: f32 = 600.0;
const MAX_CACHE_ENTRIES: usize = 4096;

/// What each plugin adds to one server's tooltip: a row of pieces each.
type Rows = Vec<Vec<Piece>>;

enum Entry {
   Pending,
   Ready(Rows),
}

static ORIGINAL: OnceLock<RenderFn> = OnceLock::new();
/// Where the tooltip's `self` keeps the server's id.
static GUILD_FIELD: AtomicIsize = AtomicIsize::new(0);
/// How many plugins add rows.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// The server whose tooltip was drawn last, the only one a change redraws.
static SHOWN: AtomicU64 = AtomicU64::new(0);
/// What the plugins returned per server.
static CACHE: LazyLock<RwLock<HashMap<u64, Entry>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static WORKER: OnceLock<mpsc::Sender<u64>> = OnceLock::new();

/// Hooks the tooltip's render, before Discord's threads run.
pub fn install(image: &Image<'static>, ix: &Index<'_>) {
   if !gpui::supports_accessories() {
      log::warn!("server tooltip: GPUI text/push primitives unavailable, skipping");
      return;
   }
   // Without it, plugins' icons show as their text.
   gpui::locate_svg(image);
   let found = match server_tooltip::tooltip(ix) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("server tooltip: {err}");
         return;
      },
   };
   GUILD_FIELD.store(found.guild, Ordering::Release);
   // SAFETY: runs before Discord's threads, and the detour has the render's
   // ABI.
   if let Some(original) = unsafe {
      crate::hook_sret::<RenderFn>(
         image,
         found.render,
         render as *const () as usize,
         "server tooltip",
      )
   } {
      let _ = ORIGINAL.set(original);
      log::info!(
         "server tooltip: hooked @ {:#x}, server id at self+{:#x}",
         found.render,
         found.guild
      );
   }
}

pub fn set_active_count(count: usize) {
   ACTIVE.store(count, Ordering::Release);
   if count > 0 {
      worker();
   }
}

pub fn clear_cache() {
   if let Ok(mut cache) = CACHE.write() {
      cache.clear();
   }
}

/// Keeps what the plugins returned for `guild`, and repaints when it changed
/// the tooltip on screen. Redrawing for every server would stall Discord when
/// a plugin updates them all at once.
pub fn set_rendered(guild: u64, rows: Rows) {
   let changed = CACHE.write().is_ok_and(|mut cache| {
      if cache.len() > MAX_CACHE_ENTRIES {
         cache.clear();
      }
      let same = matches!(cache.get(&guild), Some(Entry::Ready(old)) if same_rows(old, &rows));
      cache.insert(guild, Entry::Ready(rows));
      !same
   });
   if changed && SHOWN.load(Ordering::Acquire) == guild {
      invalidate_discord_windows();
   }
}

fn same_rows(a: &Rows, b: &Rows) -> bool {
   a.len() == b.len()
      && a.iter().zip(b).all(|(a, b)| {
         a.len() == b.len()
            && a
               .iter()
               .zip(b)
               .all(|(a, b)| a.text == b.text && a.color == b.color && a.icons == b.icons)
      })
}

fn worker() -> &'static mpsc::Sender<u64> {
   WORKER.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<u64>();
      let _ = std::thread::Builder::new()
         .name("sunbird-server-tooltips".to_owned())
         .spawn(move || {
            while let Ok(guild) = rx.recv() {
               let mut batch = vec![guild];
               while let Ok(more) = rx.try_recv() {
                  if !batch.contains(&more) {
                     batch.push(more);
                  }
               }
               for guild in batch {
                  if ACTIVE.load(Ordering::Acquire) == 0 {
                     break;
                  }
                  let rows = phoenix_runtime::engine()
                     .map_or_default(|engine| crate::js::ui::render_server_tooltip(&engine, guild));
                  set_rendered(guild, rows);
               }
            }
         });
      tx
   })
}

/// What the plugins returned for `guild`, asking them when unknown.
fn lookup(guild: u64) -> Option<Rows> {
   if let Ok(cache) = CACHE.read()
      && let Some(entry) = cache.get(&guild)
   {
      return match entry {
         Entry::Ready(rows) if !rows.is_empty() => Some(rows.clone()),
         _ => None,
      };
   }
   let ask = CACHE.write().is_ok_and(|mut cache| {
      if cache.contains_key(&guild) {
         return false;
      }
      cache.insert(guild, Entry::Pending);
      true
   });
   if ask {
      let _ = worker().send(guild);
   }
   None
}

extern "C" fn render(
   out: *mut RawAnyElement,
   this: *const u8,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   // The jump is written just before `ORIGINAL` is set, all before Discord
   // runs.
   let original = *ORIGINAL.wait();
   // SAFETY: the trampoline runs the original with Phoenix's arguments.
   let returned = unsafe { original(out, this, window, cx) };
   if ACTIVE.load(Ordering::Acquire) == 0 || this.is_null() {
      return returned;
   }
   // SAFETY: the tooltip's `self` keeps the server's id at the decoded field.
   let guild = unsafe {
      this
         .offset(GUILD_FIELD.load(Ordering::Acquire))
         .cast::<u64>()
         .read_unaligned()
   };
   if guild < MIN_SNOWFLAKE {
      return returned;
   }
   SHOWN.store(guild, Ordering::Release);
   if let Some(rows) = lookup(guild) {
      // The render draws the voice channels and streams under the line, and
      // nothing for a server without any: an element's first word is a
      // pointer, so 0 is no element.
      // SAFETY: the original filled what `out` points to.
      let drawn = unsafe { out.cast::<usize>().read_unaligned() } != 0;
      gpui::wrap_with_rows(out, drawn, &rows, FONT_PX, Some(FONT_WEIGHT));
   }
   returned
}
