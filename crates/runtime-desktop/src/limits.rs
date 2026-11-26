//! Phoenix's two message length gates, which while a plugin holds
//! `phoenix.messages.allowLongMessages` stop getting in the way of long text.
//!
//! A paste of more than 4000 characters no longer becomes a `message.txt`
//! upload, and the send check, its "too long" modal and the counter's
//! negative remainder all read a limit raised out of reach. The text then goes
//! out through `onSend`, where a plugin can split it.

use std::sync::{
   OnceLock,
   atomic::{
      AtomicBool,
      Ordering,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

/// The character count the paste check compares, `(text) -> usize`, called
/// with whatever arguments the check got.
type CountFn = unsafe extern "C" fn(usize, usize, usize, usize) -> usize;

/// `count >= 4001`, where Phoenix's check decides a paste is a file.
const LONG: usize = 0xFA1;
/// `jmp [rip + 0]` and the address, 14 bytes.
const JUMP_LEN: usize = 14;
/// The limit while held, far past any message and clear of `u32` overflow when
/// Phoenix subtracts a length from it.
const LIFTED: u32 = 1_000_000;
/// Nitro's and everyone else's limit, as Phoenix builds them.
const NITRO: u32 = 4000;
const BASE: u32 = 2000;

static ALLOWED: AtomicBool = AtomicBool::new(false);
static COUNT: OnceLock<CountFn> = OnceLock::new();
/// Where the two limits are immediates in the perks builder.
static LIMIT: OnceLock<desktop::MessageLimit> = OnceLock::new();

/// Hooks both gates from [`crate::prepare`], before Discord's threads run.
pub fn install(image: &Image<'static>) {
   install_paste(image);
   match desktop::message_limit(image) {
      Ok(found) => {
         let _ = LIMIT.set(found);
         log::info!("message limit: found @ {:#x}", found.nitro);
      },
      Err(err) => log::warn!("message limit: {err}, the send limit stays"),
   }
}

fn install_paste(image: &Image<'static>) {
   let found = match desktop::paste_limit(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("paste limit: {err}, long pastes stay files");
         return;
      },
   };
   if found.len < JUMP_LEN {
      log::warn!("paste limit: the check is too short to replace");
      return;
   }
   // SAFETY: the count function is the one the check calls, with the same
   // arguments.
   let count = unsafe { std::mem::transmute::<usize, CountFn>(found.count_fn) };
   let _ = COUNT.set(count);
   let mut jump = Vec::with_capacity(JUMP_LEN);
   jump.extend([0xFF, 0x25, 0, 0, 0, 0]);
   jump.extend((long_paste as *const () as usize).to_le_bytes());
   // SAFETY: runs before Discord's threads, and the whole check is replaced:
   // nothing returns into its old body.
   if let Err(err) = unsafe { image.write_code(found.func, &jump) } {
      log::warn!("paste limit: {err}");
      return;
   }
   log::info!("paste limit: replaced @ {:#x}", found.func);
}

/// While `on`, a paste is never taken for a file because of its length, and
/// Phoenix's message limit is out of reach. Off puts both back.
pub fn allow(on: bool) {
   if ALLOWED.swap(on, Ordering::AcqRel) == on {
      return;
   }
   let Some(limit) = LIMIT.get() else {
      return;
   };
   let Ok(image) = phoenix_runtime::target() else {
      return;
   };
   let (nitro, base) = if on { (LIFTED, LIFTED) } else { (NITRO, BASE) };
   for (at, value) in [(limit.nitro, nitro), (limit.base, base)] {
      // SAFETY: a four-byte immediate that Phoenix only loads, so a reader sees
      // the old limit or the new one, and either is a limit.
      if let Err(err) = unsafe { image.write_code(at, &value.to_le_bytes()) } {
         log::warn!("message limit: {err}");
      }
   }
}

extern "C" fn long_paste(a: usize, b: usize, c: usize, d: usize) -> bool {
   if ALLOWED.load(Ordering::Acquire) {
      return false;
   }
   // The jump is written just before `COUNT` is set, all before Discord runs.
   let count = COUNT.wait();
   // SAFETY: the same call the replaced check made, with its arguments.
   unsafe { count(a, b, c, d) >= LONG }
}
