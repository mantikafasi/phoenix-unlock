use std::{
   cell::Cell,
   collections::BTreeMap,
   ffi::c_void,
   panic::{
      self,
      AssertUnwindSafe,
   },
   slice,
   str,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::Image;
use phoenix_sigs::{
   Abi,
   EventId,
   GATEWAY_HANDLER_FIELD,
   GatewayEvent,
   GatewayHandler,
   GatewayRegistry,
   HandlerResult,
   RawStr,
};
use serde_json::{
   Map,
   Value,
   value::RawValue,
};

use crate::{
   Error,
   STATE,
   State,
   js::Before,
};

type HandlerFn = <GatewayHandler as Abi>::Fn;

/// The registry's original handlers, in [`GatewayRegistry::iter`] order, and
/// which events a plugin listens to.
struct Gateway {
   registry:  GatewayRegistry,
   originals: Vec<HandlerFn>,
   hooked:    Vec<AtomicBool>,
}

static GATEWAY: OnceLock<Gateway> = OnceLock::new();
/// Events plugins fed Phoenix from outside a hook. They run on the store
/// thread ahead of the next event Phoenix handles, since only a handler call
/// carries the store.
static QUEUE: Mutex<Vec<(EventId, String)>> = Mutex::new(Vec::new());
static QUEUED: AtomicBool = AtomicBool::new(false);

thread_local! {
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static IN_JS: Cell<bool> = const { Cell::new(false) };
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static HANDLING: Cell<Option<Handling>> = const { Cell::new(None) };
}

/// The host's handler arguments for the event whose hooks are running, so a
/// hook can feed Phoenix another event on the same store thread.
#[derive(Clone, Copy)]
struct Handling {
   store: *mut c_void,
   ctx:   *mut c_void,
   rest:  [u64; 4],
}

/// Points every registry entry at [`detour`]. Phoenix looks handlers up in the
/// registry on each dispatch, so a pointer swap catches every event without
/// patching code, and events no plugin listens to cost one atomic load.
pub fn swap(
   image: &Image<'static>,
   registry: GatewayRegistry,
) -> Result<&'static GatewayRegistry, Error> {
   let writes = registry
      .iter()
      .map(|(_, entry)| {
         (
            entry.slot + GATEWAY_HANDLER_FIELD,
            detour as *const () as usize,
         )
      })
      .collect::<Vec<(usize, usize)>>();
   let originals = registry
      .iter()
      // SAFETY: the registry walk only accepts entries whose handler starts a
      // function, and every registry handler has this ABI.
      .map(|(_, entry)| unsafe { std::mem::transmute::<usize, HandlerFn>(entry.handler) })
      .collect();
   let hooked = registry.iter().map(|_| AtomicBool::new(false)).collect();

   GATEWAY
      .set(Gateway {
         registry,
         originals,
         hooked,
      })
      .map_err(|_| Error::AlreadyStarted)?;
   let gateway = GATEWAY.get().expect("GATEWAY was set above");

   // SAFETY: the slots are the handler fields of registry entries in `image`,
   // and the host has not started dispatching while its constructors are
   // pending.
   let previous = unsafe { image.swap_words(&writes) }?;
   for ((_, entry), found) in gateway.registry.iter().zip(previous) {
      if found != entry.handler {
         return Err(
            phoenix_sigs::Error::Conflict {
               sig: entry.name.clone(),
               anchor: "registry swap".into(),
               expected: entry.handler,
               found,
            }
            .into(),
         );
      }
   }
   Ok(&gateway.registry)
}

/// Checks the build table's rules against the registry walk. Code patterns
/// scan all of `.text`, about 0.4 s each on Phoenix, so this runs off the
/// host's startup path. Table anchors read the pointers [`swap`] already
/// replaced and stop matching, so only code anchors count here.
pub fn verify(state: &State) {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before verifying");
   for rule in state.build.map_or(&[][..], |build| build.gateway) {
      let Some(entry) = gateway
         .registry
         .id(&rule.sig.name)
         .map(|id| gateway.registry.entry(id))
      else {
         log::warn!("{}: no such event in the registry", rule.sig.name);
         continue;
      };

      match rule.sig.resolve(&state.image) {
         Ok(located) if located.addr == entry.handler => {
            log::info!(
               "{} at {:#x}, {}/{} anchors agree with the registry",
               entry.name,
               located.addr,
               located.agreeing,
               rule.sig.anchors.len(),
            );
         },
         Ok(located) => {
            log::error!(
               "{}: the table rule found {:#x}, the registry holds {:#x}",
               entry.name,
               located.addr,
               entry.handler,
            );
         },
         Err(err) => log::error!("{err}"),
      }
   }
}

/// Runs `data` through Phoenix's own handler for `event`, skipping the
/// plugins. Returns whether the handler accepted it.
pub fn dispatch(event: EventId, data: &str) -> Option<bool> {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   let handling = HANDLING.get()?;
   let name = &gateway.registry.entry(event).name;
   let synthetic = GatewayEvent {
      name: RawStr {
         ptr: name.as_ptr(),
         len: name.len(),
      },
      data: RawStr {
         ptr: data.as_ptr(),
         len: data.len(),
      },
      rest: handling.rest,
   };

   // SAFETY: `handling` holds the arguments of a handler call still on this
   // thread's stack, and the event borrows strings that outlive the call.
   let original = gateway.originals[event.index()];
   let result = unsafe { original(handling.store, &raw const synthetic, handling.ctx) };
   Some(result.is_ok())
}

/// The live registry entry named `name`, like `MESSAGE_CREATE`.
pub fn event(name: &str) -> Option<EventId> {
   GATEWAY.get()?.registry.id(name)
}

/// Runs `data` through Phoenix's own handler for `event` ahead of the next
/// event Phoenix handles.
pub fn enqueue(event: EventId, data: String) {
   QUEUE
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push((event, data));
   QUEUED.store(true, Ordering::Release);
}

fn flush(gateway: &Gateway, store: *mut c_void, ctx: *mut c_void, rest: [u64; 4]) {
   let queued = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(PoisonError::into_inner));
   for (event, data) in queued {
      let name = &gateway.registry.entry(event).name;
      let synthetic = GatewayEvent {
         name: RawStr {
            ptr: name.as_ptr(),
            len: name.len(),
         },
         data: RawStr {
            ptr: data.as_ptr(),
            len: data.len(),
         },
         rest,
      };
      // SAFETY: `store` and `ctx` are the arguments of the handler call this
      // runs inside, and the event borrows strings that outlive the call.
      let original = gateway.originals[event.index()];
      if unsafe { original(store, &raw const synthetic, ctx) }.is_ok() {
         continue;
      }
      log::warn!("Phoenix rejected a queued {name}");
   }
}

/// Routes `event` through the plugins.
pub fn install(event: EventId) {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   if !gateway.hooked[event.index()].swap(true, Ordering::AcqRel) {
      log::info!("hooked {}", gateway.registry.entry(event).name);
   }
}

/// Sends `event` straight to Phoenix again once no plugin listens to it.
pub fn uninstall(event: EventId) {
   let gateway = GATEWAY
      .get()
      .expect("start swaps the registry before plugins load");
   if gateway.hooked[event.index()].swap(false, Ordering::AcqRel) {
      log::info!("unhooked {}", gateway.registry.entry(event).name);
   }
}

unsafe extern "C" fn detour(
   store: *mut c_void,
   event: *const GatewayEvent,
   ctx: *mut c_void,
) -> HandlerResult {
   #[cfg(target_os = "android")]
   crate::bridge::attach();
   let gateway = GATEWAY
      .get()
      .expect("slots point here only after GATEWAY is set");
   // SAFETY: the host passes a live event for the duration of the call.
   let event = unsafe { &*event };
   if QUEUED.swap(false, Ordering::AcqRel) {
      flush(gateway, store, ctx, event.rest);
   }
   // SAFETY: name and data are borrowed strings that outlive the call.
   let (name, data) = unsafe { (text(event.name), text(event.data)) };
   let Some((id, name)) = name.and_then(|name| Some((gateway.registry.id(name)?, name))) else {
      log::error!("gateway event without a registry entry, dropped");
      return HandlerResult::OK;
   };
   let promoted = data.and_then(|data| promote(name, data));
   let promoted_event = promoted.as_deref().map(|json| {
      GatewayEvent {
         data: RawStr {
            ptr: json.as_ptr(),
            len: json.len(),
         },
         ..*event
      }
   });
   let event = promoted_event.as_ref().unwrap_or(event);
   let data = promoted.as_deref().or(data);
   let original = gateway.originals[id.index()];
   // SAFETY: forwarded to the registry's own handler unchanged or with
   // replaced data.
   let call = |event: &GatewayEvent| unsafe { original(store, event, ctx) };

   let listened = gateway.hooked[id.index()].load(Ordering::Acquire);
   let (true, Some(state), Some(data), false) = (listened, STATE.get(), data, IN_JS.get()) else {
      return call(event);
   };
   HANDLING.set(Some(Handling {
      store,
      ctx,
      rest: event.rest,
   }));
   #[cfg(target_os = "android")]
   let original_data = data;
   let before = run_js(state, |engine| engine.before(id, name, data)).unwrap_or(Before::Keep);
   let (result, data) = match &before {
      Before::Drop => {
         HANDLING.set(None);
         return HandlerResult::OK;
      },
      Before::Keep => (call(event), data),
      Before::Replace(json) => {
         let replaced = GatewayEvent {
            data: RawStr {
               ptr: json.as_ptr(),
               len: json.len(),
            },
            ..*event
         };
         (call(&replaced), json.as_str())
      },
   };
   #[cfg(target_os = "android")]
   if let (Before::Replace(json), "MESSAGE_CREATE" | "MESSAGE_UPDATE") = (&before, name) {
      remember_original(original_data, json);
   }
   run_js(state, |engine| engine.after(id, name, data, result.is_ok()));
   HANDLING.set(None);
   result
}

/// Whether READY and USER_UPDATE carry the staff flag.
static STAFF_UNLOCK: AtomicBool = AtomicBool::new(true);

#[must_use]
pub fn staff_enabled() -> bool {
   STAFF_UNLOCK.load(Ordering::Relaxed)
}

pub fn set_staff_enabled(enabled: bool) {
   STAFF_UNLOCK.store(enabled, Ordering::Relaxed);
}

/// Gives the signed-in user the staff flag, which unlocks Phoenix's
/// experiments. `None` leaves the event as it was.
fn promote(name: &str, data: &str) -> Option<String> {
   if !staff_enabled() {
      return None;
   }
   let promoted = match name {
      "READY" => ready_with_staff(data),
      "USER_UPDATE" => with_staff(data),
      _ => return None,
   };
   if promoted.is_none() {
      log::warn!("{name} kept its user flags, the staff flag is off");
   }
   promoted
}

#[cfg(target_os = "android")]
#[derive(serde::Deserialize)]
struct Posted<'a> {
   #[serde(borrow)]
   content: Option<std::borrow::Cow<'a, str>>,
}

/// Hands the patches a message's text from before and after the plugin
/// hooks, when they changed it.
#[cfg(target_os = "android")]
fn remember_original(before: &str, after: &str) {
   let (Ok(before), Ok(after)) = (
      serde_json::from_str::<Posted<'_>>(before),
      serde_json::from_str::<Posted<'_>>(after),
   ) else {
      return;
   };
   if let (Some(original), Some(shown)) = (before.content, after.content)
      && original != shown
   {
      crate::patches::remember_original(shown.into_owned(), original.into_owned());
   }
}

/// READY with its `user` swapped for one with the staff flag. The other
/// fields pass through as they came.
fn ready_with_staff(data: &str) -> Option<String> {
   let mut fields = serde_json::from_str::<BTreeMap<String, &RawValue>>(data).ok()?;
   let user = RawValue::from_string(with_staff(fields.get("user")?.get())?).ok()?;
   fields.insert("user".to_owned(), &user);
   serde_json::to_string(&fields).ok()
}

fn with_staff(user: &str) -> Option<String> {
   const STAFF: u64 = 1;

   let mut user = serde_json::from_str::<Map<String, Value>>(user).ok()?;
   let flags = user
      .get("flags")
      .and_then(Value::as_u64)
      .unwrap_or_default();
   user.insert("flags".to_owned(), (flags | STAFF).into());
   serde_json::to_string(&user).ok()
}

/// # Safety
///
/// `raw` describes live bytes.
unsafe fn text<'a>(raw: RawStr) -> Option<&'a str> {
   if raw.ptr.is_null() {
      return None;
   }
   // SAFETY: forwarded from the caller.
   str::from_utf8(unsafe { slice::from_raw_parts(raw.ptr, raw.len) }).ok()
}

/// Runs `work` on the engine, logging JS errors and panics instead of unwinding
/// into the host. Nested gateway calls made while JS runs go straight to the
/// original.
fn run_js<T>(
   state: &State,
   work: impl FnOnce(&mut crate::js::Engine) -> Result<T, Error>,
) -> Option<T> {
   IN_JS.set(true);
   let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
      let mut engine = state
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner);
      work(&mut engine)
   }));
   IN_JS.set(false);
   match outcome {
      Ok(Ok(value)) => Some(value),
      Ok(Err(err)) => {
         log::error!("{err}");
         None
      },
      Err(_) => {
         log::error!("a gateway hook panicked, the event went through unmodified");
         None
      },
   }
}
