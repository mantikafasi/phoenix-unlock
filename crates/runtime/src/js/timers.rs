//! `setTimeout`, `setInterval` and their `clear` functions, which every plugin
//! gets as names of its own. One thread waits for the next deadline and runs
//! the due callbacks through the engine, so a timer costs no thread of its
//! own and none of them runs on the thread that set it.

use std::{
   cmp::Reverse,
   collections::BinaryHeap,
   sync::{
      Condvar,
      Mutex,
      Once,
      PoisonError,
      atomic::{
         AtomicU64,
         Ordering,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::{
      Opt,
      Rest,
   },
};

use crate::js::{
   Engine,
   Registry,
};

/// The longest delay browsers keep (2^31 - 1 ms). Longer ones fire at once
/// there, and wait this long here.
const LONGEST: f64 = 2_147_483_647.0;

/// The shortest interval, so a `setInterval(f, 0)` cannot keep the engine
/// lock and the app's threads waiting on it.
const SHORTEST_INTERVAL: Duration = Duration::from_millis(4);

/// Live timers one plugin may hold, so a loop of `setTimeout` calls fails in
/// the plugin before it fills the heap every plugin shares.
const PER_PLUGIN: usize = 10_000;

/// A timer's callback, the arguments it gets and, for an interval, its
/// period.
pub struct Timer<'js> {
   plugin:   String,
   callback: Function<'js>,
   args:     Vec<Value<'js>>,
   every:    Option<Duration>,
}

/// Deadlines by timer id, the earliest on top. A cleared timer's deadline
/// stays until it comes up and finds the timer gone.
static DUE: Mutex<BinaryHeap<Reverse<(Instant, u64)>>> = Mutex::new(BinaryHeap::new());
static WAKE: Condvar = Condvar::new();
static WAITER: Once = Once::new();

fn schedule(id: u64, at: Instant) {
   DUE.lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(Reverse((at, id)));
   WAKE.notify_one();
   // Without a started runtime no engine can run a callback, so the thread
   // would only drop deadlines. Tests take them with `take_due` instead.
   if crate::STATE.get().is_some() {
      WAITER.call_once(|| {
         std::thread::Builder::new()
            .name("phoenix-timers".to_owned())
            .spawn(wait)
            .map_err(|err| log::error!("starting the timer thread: {err}"))
            .ok();
      });
   }
}

/// Takes the ids whose deadline passed, earliest first.
fn take_due(now: Instant) -> Vec<u64> {
   let mut ids = Vec::new();
   let mut due = DUE.lock().unwrap_or_else(PoisonError::into_inner);
   while let Some(&Reverse((at, id))) = due.peek() {
      if at > now {
         break;
      }
      due.pop();
      ids.push(id);
   }
   drop(due);
   ids
}

fn wait() {
   loop {
      let ids = {
         let mut due = DUE.lock().unwrap_or_else(PoisonError::into_inner);
         loop {
            let now = Instant::now();
            match due.peek() {
               None => due = WAKE.wait(due).unwrap_or_else(PoisonError::into_inner),
               Some(&Reverse((at, _))) if at > now => {
                  due = WAKE
                     .wait_timeout(due, at - now)
                     .unwrap_or_else(PoisonError::into_inner)
                     .0;
               },
               Some(_) => break,
            }
         }
         drop(due);
         take_due(Instant::now())
      };
      for id in ids {
         if let Some(engine) = crate::engine() {
            engine.fire(id);
         }
      }
   }
}

fn delay(ms: Option<f64>) -> Duration {
   let ms = ms.filter(|ms| ms.is_finite() && *ms > 0.0).unwrap_or(0.0);
   Duration::from_secs_f64(ms.min(LONGEST) / 1000.0)
}

impl Engine {
   /// Runs timer `id` if it is still set, and sets an interval's next run.
   pub fn fire(&self, id: u64) {
      self.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return;
         };
         let timer = {
            let mut timers = registry.timers.borrow_mut();
            match timers.get(&id) {
               None => return,
               Some(timer) if timer.every.is_some() => {
                  Timer {
                     plugin:   timer.plugin.clone(),
                     callback: timer.callback.clone(),
                     args:     timer.args.clone(),
                     every:    timer.every,
                  }
               },
               Some(_) => {
                  let Some(timer) = timers.remove(&id) else {
                     return;
                  };
                  timer
               },
            }
         };
         if let Some(every) = timer.every {
            schedule(id, Instant::now() + every);
         }
         if let Err(err) = timer
            .callback
            .call::<_, ()>((Rest(timer.args),))
            .catch(&ctx)
         {
            log::error!("[{}] timer: {err}", timer.plugin);
         }
      });
   }
}

/// Adds `setTimeout`, `setInterval`, `clearTimeout` and `clearInterval` for
/// `plugin` to `names`.
pub fn add_timers<'js>(ctx: &Ctx<'js>, plugin: &str, names: &Object<'js>) -> rquickjs::Result<()> {
   for (name, repeats) in [("setTimeout", false), ("setInterval", true)] {
      let owner = plugin.to_owned();
      names.set(
         name,
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, callback: Value<'js>, ms: Opt<f64>, args: Rest<Value<'js>>| {
               set(&ctx, &owner, callback, ms.0, args.0, repeats)
            },
         )?,
      )?;
   }
   for name in ["clearTimeout", "clearInterval"] {
      let owner = plugin.to_owned();
      names.set(
         name,
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: Opt<Value<'js>>| {
            clear(&ctx, &owner, id.0.and_then(|id| id.as_number()));
         })?,
      )?;
   }
   Ok(())
}

fn set<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   callback: Value<'js>,
   ms: Option<f64>,
   args: Vec<Value<'js>>,
   repeats: bool,
) -> rquickjs::Result<f64> {
   static NEXT: AtomicU64 = AtomicU64::new(1);

   let Some(callback) = callback.into_function() else {
      return Err(Exception::throw_type(
         ctx,
         "timers take a function, code strings are not evaluated",
      ));
   };
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   let mut timers = registry.timers.borrow_mut();
   if timers
      .values()
      .filter(|timer| timer.plugin == plugin)
      .count()
      >= PER_PLUGIN
   {
      return Err(Exception::throw_range(
         ctx,
         &format!("more than {PER_PLUGIN} timers are set"),
      ));
   }
   let wait = delay(ms);
   let every = repeats.then(|| wait.max(SHORTEST_INTERVAL));
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   timers.insert(id, Timer {
      plugin: plugin.to_owned(),
      callback,
      args,
      every,
   });
   drop(timers);
   schedule(id, Instant::now() + every.unwrap_or(wait));
   #[expect(clippy::cast_precision_loss, reason = "ids stay far below 2^53")]
   Ok(id as f64)
}

fn clear(ctx: &Ctx<'_>, plugin: &str, id: Option<f64>) {
   let Some(id) = id.filter(|id| id.fract() == 0.0 && *id >= 1.0) else {
      return;
   };
   #[expect(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      reason = "checked to be a positive whole number"
   )]
   let id = id as u64;
   if let Some(registry) = ctx.userdata::<Registry<'_>>() {
      let mut timers = registry.timers.borrow_mut();
      if timers.get(&id).is_some_and(|timer| timer.plugin == plugin) {
         timers.remove(&id);
      }
   }
}

/// Drops every timer `plugin` set.
pub fn forget(ctx: &Ctx<'_>, plugin: &str) {
   if let Some(registry) = ctx.userdata::<Registry<'_>>() {
      registry
         .timers
         .borrow_mut()
         .retain(|_, timer| timer.plugin != plugin);
   }
}

#[cfg(test)]
pub fn due_now() -> Vec<u64> {
   take_due(Instant::now())
}
