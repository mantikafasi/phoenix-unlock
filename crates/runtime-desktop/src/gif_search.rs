//! The GIF picker's Favourites search (`phoenix.ui.setGifFavoritesSearch`).
//!
//! Every part of the picker reads its tiles through one accessor that returns
//! `(ptr, len)` of the favourites `Vec` on the Favourites tab. The hook keeps
//! a copy of Discord's tiles while the search is empty, so it holds every
//! favourite. For any other search text it asks the plugin, off the render
//! thread, which favourites to show, then returns tiles of its own built from
//! that copy and redraws.

use std::{
   collections::HashMap,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
      mpsc,
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop::{
   self,
   GIF_RESULT_SIZE,
   GIF_RESULTS_PROLOGUE,
   GIF_RESULTS_PROLOGUE_LEN,
   GifResults,
};

/// Rust's `String` as Discord lays it out: `cap, ptr, len`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawString {
   cap: usize,
   ptr: *const u8,
   len: usize,
}

impl RawString {
   const EMPTY: Self = Self {
      cap: 0,
      ptr: std::ptr::dangling(),
      len: 0,
   };

   const fn of(text: &str) -> Self {
      if text.is_empty() {
         return Self::EMPTY;
      }
      Self {
         cap: text.len(),
         ptr: text.as_ptr(),
         len: text.len(),
      }
   }

   /// # Safety
   ///
   /// `self` is a live `String` of Discord's.
   unsafe fn to_owned(self) -> Box<str> {
      if self.len == 0 {
         return Box::default();
      }
      // SAFETY: forwarded from the caller.
      let bytes = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
      String::from_utf8_lossy(bytes).into()
   }
}

/// A Favourites tile, as read from Discord's live list: two strings always
/// empty there, the favourite's key URL, its media URL, a short name the
/// built-in search matches, the size, and whether it is a video (a `bool`
/// whose padding holds leftovers).
#[repr(C)]
#[derive(Clone, Copy)]
struct Tile {
   a:      RawString,
   url:    RawString,
   src:    RawString,
   b:      RawString,
   name:   RawString,
   width:  u32,
   height: u32,
   video:  u8,
   _pad:   [u8; 7],
}

const _: () = assert!(size_of::<Tile>() == GIF_RESULT_SIZE);

/// A favourite copied out of Discord's list.
struct Favorite {
   src:    Box<str>,
   name:   Box<str>,
   width:  u32,
   height: u32,
   video:  u8,
}

/// Every favourite, copied while the search was empty.
struct Snapshot {
   /// The list's `(ptr, len)` when copied, to copy again once it changes.
   key:    [usize; 2],
   by_url: HashMap<Box<str>, Favorite>,
}

/// Tiles built for one search, pointing into `text`.
struct Results {
   tiles: Box<[Tile]>,
   _text: Vec<Box<str>>,
}

// SAFETY: the tiles' pointers point into `_text`, owned alongside.
#[expect(
   clippy::non_send_fields_in_send_ty,
   reason = "the tiles only point into `_text`, which moves with them"
)]
unsafe impl Send for Results {}

enum Answer {
   Pending,
   /// The plugin's tiles, or `None` for Discord's own results.
   Ready(Option<Results>),
}

struct State {
   snapshot: Option<Snapshot>,
   /// The last list that failed [`bad_tile`], logged once.
   rejected: [usize; 2],
   query:    String,
   answer:   Option<Answer>,
   /// Results the picker may still hold a borrow of, freed a while later.
   retired:  Vec<(Instant, Results)>,
}

static STATE: Mutex<State> = Mutex::new(State {
   snapshot: None,
   rejected: [0; 2],
   query:    String::new(),
   answer:   None,
   retired:  Vec::new(),
});
static FOUND: OnceLock<GifResults> = OnceLock::new();
/// Whether a plugin searches, so the hook stays out of the way otherwise.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// The picker borrows the slice only while it draws one frame.
const RETIRE_AFTER: Duration = Duration::from_secs(30);

pub fn supported() -> bool {
   FOUND.get().is_some()
}

/// Turns plugin searching on or off, dropping any answer already given.
pub fn set_active(active: bool) {
   ACTIVE.store(active, Ordering::Release);
   if active {
      worker();
   }
   refresh();
}

/// Asks the plugin again about the current search.
pub fn refresh() {
   {
      let mut state = lock();
      retire(&mut state);
   }
   crate::modal::force_redraw();
}

fn lock() -> std::sync::MutexGuard<'static, State> {
   STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn retire(state: &mut State) {
   if let Some(Answer::Ready(Some(old))) = state.answer.take() {
      state.retired.push((Instant::now(), old));
   }
   state.retired.retain(|(at, _)| at.elapsed() < RETIRE_AFTER);
}

fn worker() -> &'static mpsc::Sender<String> {
   WORKER.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<String>();
      let _ = std::thread::Builder::new()
         .name("sunbird-gif-search".to_owned())
         .spawn(move || {
            while let Ok(mut query) = rx.recv() {
               // Only the latest of a burst of keystrokes matters.
               while let Ok(newer) = rx.try_recv() {
                  query = newer;
               }
               let urls = phoenix_runtime::engine()
                  .and_then(|engine| crate::js::ui::search_gif_favorites(&engine, &query));
               let mut state = lock();
               if state.query != query || !matches!(state.answer, Some(Answer::Pending)) {
                  continue;
               }
               let results = urls.map(|urls| build(state.snapshot.as_ref(), &urls));
               state.answer = Some(Answer::Ready(results));
               drop(state);
               crate::modal::force_redraw();
            }
         });
      tx
   })
}

/// Tiles for `urls`, in order, from the favourites in `snapshot`.
fn build(snapshot: Option<&Snapshot>, urls: &[String]) -> Results {
   let mut tiles = Vec::new();
   let mut text = Vec::new();
   for url in urls {
      let Some(fav) = snapshot.and_then(|s| s.by_url.get(url.as_str())) else {
         continue;
      };
      let url: Box<str> = url.as_str().into();
      let src = fav.src.clone();
      let name = fav.name.clone();
      tiles.push(Tile {
         a:      RawString::EMPTY,
         url:    RawString::of(&url),
         src:    RawString::of(&src),
         b:      RawString::EMPTY,
         name:   RawString::of(&name),
         width:  fav.width,
         height: fav.height,
         video:  fav.video,
         _pad:   [0; 7],
      });
      text.extend([url, src, name]);
   }
   Results {
      tiles: tiles.into_boxed_slice(),
      _text: text,
   }
}

/// Locates and hooks the accessor from `DllMain`.
pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE || !cfg!(windows) {
      return;
   }
   let found = match desktop::gif_results(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("gif search: {err}, ");
         return;
      },
   };
   // SAFETY: from `prepare`, before the picker runs; the shim keeps the
   // accessor's `rcx -> (rax, rdx)` contract. The trampoline is never needed:
   // `original` reads the same fields.
   if let Err(err) = unsafe {
      image.hook_entry(
         found.accessor,
         GIF_RESULTS_PROLOGUE,
         GIF_RESULTS_PROLOGUE_LEN,
         accessor_shim as *const () as usize,
      )
   } {
      log::warn!("gif search: hook failed: {err}");
      return;
   }
   let _ = FOUND.set(found);
}

/// The accessor returns its slice in `rax:rdx`, which no stable Rust ABI
/// does, so the shim has [`pick`] write it to the stack.
#[cfg(windows)]
#[unsafe(naked)]
extern "C" fn accessor_shim() {
   core::arch::naked_asm!(
      "sub rsp, 56",
      "lea rdx, [rsp + 32]",
      "call {pick}",
      "mov rax, [rsp + 32]",
      "mov rdx, [rsp + 40]",
      "add rsp, 56",
      "ret",
      pick = sym pick,
   );
}

#[cfg(not(windows))]
extern "C" fn accessor_shim() {}

/// The slice the accessor would return, read the same way.
const unsafe fn original(found: &GifResults, picker: *const u8) -> [usize; 2] {
   // SAFETY: `picker` is the picker state the accessor was called with.
   unsafe {
      let (p, l) = if *picker.add(found.mode) == found.fav_mode {
         (found.fav_ptr, found.fav_len)
      } else {
         (found.search_ptr, found.search_len)
      };
      [
         picker.add(p).cast::<usize>().read_unaligned(),
         picker.add(l).cast::<usize>().read_unaligned(),
      ]
   }
}

unsafe extern "C" fn pick(picker: *const u8, out: *mut [usize; 2]) {
   let Some(found) = FOUND.get() else {
      return;
   };
   // SAFETY: as in `original`; `out` is the shim's stack slot.
   unsafe {
      let base = original(found, picker);
      *out = base;
      if !ACTIVE.load(Ordering::Acquire) || *picker.add(found.mode) != found.fav_mode {
         return;
      }
      if let Ok(Some(ours)) = std::panic::catch_unwind(|| favourites(found, picker, base)) {
         *out = ours;
      }
   }
}

/// The first of Discord's tiles not in [`Tile`]'s shape, if any.
fn bad_tile(tiles: &[Tile]) -> Option<usize> {
   let sane = |s: &RawString| s.len <= s.cap && s.len < 1 << 16 && !s.ptr.is_null();
   tiles.iter().position(|t| {
      !(sane(&t.a)
         && sane(&t.url)
         && sane(&t.src)
         && sane(&t.b)
         && sane(&t.name)
         && t.url.len > 0
         && t.video <= 1)
   })
}

/// The tiles to show on the Favourites tab, or `None` for Discord's.
unsafe fn favourites(
   found: &GifResults,
   picker: *const u8,
   base: [usize; 2],
) -> Option<[usize; 2]> {
   // SAFETY: the search text's `String` fields, decoded by the locator.
   let query = unsafe {
      let text = picker
         .add(found.query_ptr)
         .cast::<*const u8>()
         .read_unaligned();
      let len = picker.add(found.query_len).cast::<usize>().read_unaligned();
      if text.is_null() || len > 1024 {
         return None;
      }
      std::str::from_utf8(std::slice::from_raw_parts(text, len)).ok()?
   };
   let mut state = lock();
   if query.is_empty() {
      // SAFETY: Discord's list, every favourite while the search is empty.
      unsafe { take_snapshot(&mut state, base) };
      return None;
   }
   if state.query != query || state.answer.is_none() {
      retire(&mut state);
      query.clone_into(&mut state.query);
      state.answer = Some(Answer::Pending);
      let _ = worker().send(query.to_owned());
      return None;
   }
   match &state.answer {
      Some(Answer::Ready(Some(results))) => {
         Some([results.tiles.as_ptr() as usize, results.tiles.len()])
      },
      _ => None,
   }
}

/// Copies Discord's favourites unless the list is the one already copied.
unsafe fn take_snapshot(state: &mut State, base: [usize; 2]) {
   if base[1] == 0 || state.snapshot.as_ref().is_some_and(|s| s.key == base) {
      return;
   }
   // SAFETY: forwarded from the caller.
   let tiles = unsafe { std::slice::from_raw_parts(base[0] as *const Tile, base[1]) };
   // Copies only lists that look like favourites, so the tiles built later
   // hold nothing but strings read from one.
   if let Some(i) = bad_tile(tiles) {
      if state.rejected != base {
         state.rejected = base;
         let t = &tiles[i];
         let shape = |s: &RawString| (s.cap, s.len, s.ptr.is_null());
         log::warn!(
            "gif search: skipped a favourites list, tile {i} of {} has {:?} {:?} {:?} {:?} {:?}",
            base[1],
            shape(&t.a),
            shape(&t.url),
            shape(&t.src),
            shape(&t.b),
            shape(&t.name),
         );
      }
      return;
   }
   let by_url = tiles
      .iter()
      .map(|t| {
         // SAFETY: live tiles of Discord's.
         unsafe {
            (t.url.to_owned(), Favorite {
               src:    t.src.to_owned(),
               name:   t.name.to_owned(),
               width:  t.width,
               height: t.height,
               video:  t.video,
            })
         }
      })
      .collect();
   state.snapshot = Some(Snapshot { key: base, by_url });
}
