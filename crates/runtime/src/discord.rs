//! Discord's HTTP API through Phoenix's own `Networking` host module, so a
//! plugin's request leaves on the app's OkHttp client with the app's session
//! headers.
//!
//! - Phoenix's Rust side sets every header, `authorization` included, and Java
//!   only sends them. [`observe_call`] keeps the latest set and the API base
//!   from Phoenix's own requests, in memory only.
//! - [`send`] issues `Networking.request` with ids from [`FIRST_ID`], and
//!   [`filter`] takes the `@http` replies for those ids out of the dispatch
//!   stream before Phoenix sees them.
//!
//! Module, method and field numbers index the host's name table `hh.jb`,
//! versionCode 356207.

use std::{
   collections::HashMap,
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
      atomic::{
         AtomicI32,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_wire::{
   DispatchEvent,
   HostCall,
   Key,
   Value,
   decode_dispatch,
   edit_frames,
};

use crate::{
   bridge::{
      self,
      Action,
   },
   fetch::Response,
};

const NETWORKING: u16 = 43;
const REQUEST: u16 = 168;
const HTTP_EVENT: u16 = 321;

const ERROR: Key = Key::new(403);
const ID: Key = Key::new(417);
const METHOD: Key = Key::new(421);
const URL: Key = Key::new(422);
const HEADERS: Key = Key::new(423);
const STATUS: Key = Key::new(430);

/// Far above the counter Phoenix numbers its own requests with.
const FIRST_ID: i32 = 0x5000_0000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Headers Phoenix sets per request, which a plugin's request must not
/// inherit.
const PER_REQUEST: [&str; 4] = [
   "content-type",
   "content-length",
   "x-context-properties",
   "x-audit-log-reason",
];

/// A plugin waiting for the user to authorize `client_id` in Phoenix's
/// authorize screen, and Phoenix's request id once it sends the authorization.
struct Authorizing {
   client_id: String,
   call:      Option<i32>,
   waiter:    Sender<String>,
   /// Tells this wait from a newer one.
   turn:      u64,
}

static AUTHORIZING: Mutex<Option<Authorizing>> = Mutex::new(None);
/// The user whose profile Phoenix fetched last, and when.
static LAST_PROFILE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// The user `url` fetches the profile of, `…/users/<id>/profile`.
fn profile_of(url: &str) -> Option<String> {
   let after = &url[url.find("/users/")? + "/users/".len()..];
   let (id, rest) = after.split_once('/')?;
   (rest.starts_with("profile") && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
      .then(|| id.to_owned())
}

/// The user whose profile Phoenix fetched in the last few seconds, for a
/// profile screen that shows no user id of its own.
pub fn last_profile() -> Option<String> {
   let last = LAST_PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
   let (id, at) = last.as_ref()?;
   (at.elapsed() < Duration::from_secs(5)).then(|| id.clone())
}
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, flub::Error)]
pub enum DiscordError {
   #[error("Phoenix has not made an API request yet, so there is no session")]
   NoSession,
   #[error("{path}: not a path under the API base")]
   Path { path: String },
   #[error("Phoenix's bridge is not up yet")]
   NoBridge,
   #[error("{path}: no answer in {} seconds", TIMEOUT.as_secs())]
   Timeout { path: String },
   #[error("{path}: {message}")]
   Network { path: String, message: String },
   /// Discord turned down an OAuth2 authorization. The message is Discord's.
   #[error("authorize: Discord answered {status}: {message}")]
   Refused { status: i32, message: String },
}

/// What Phoenix's own API requests carry.
struct Session {
   /// Up to and including the version, like `https://discord.com/api/v9`.
   base:    String,
   headers: Vec<(String, String)>,
}

/// `@http`'s fields and body.
struct Reply {
   status: i32,
   error:  Option<String>,
   body:   Vec<u8>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static PENDING: LazyLock<Mutex<HashMap<i32, Sender<Reply>>>> = LazyLock::new(Mutex::default);
static NEXT_ID: AtomicI32 = AtomicI32::new(FIRST_ID);
static REQUEST_HOOKS: LazyLock<Mutex<Vec<RequestHook>>> = LazyLock::new(Mutex::default);
static REQUEST_HOOK_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Rust-side filter for `phoenix.discord.onRequest` so non-matching API calls
/// never enter QuickJS.
#[derive(Clone, Default)]
pub struct RequestFilter {
   pub method:        Option<String>,
   pub path_prefix:   Option<String>,
   pub path_suffix:   Option<String>,
   pub path_includes: Option<String>,
}

impl RequestFilter {
   fn matches(&self, method: &str, path: &str) -> bool {
      if let Some(expected) = &self.method
         && !method.eq_ignore_ascii_case(expected)
      {
         return false;
      }
      let clean = path.split(['?', '#']).next().unwrap_or(path);
      if let Some(prefix) = &self.path_prefix
         && !clean.starts_with(prefix.as_str())
      {
         return false;
      }
      if let Some(suffix) = &self.path_suffix
         && !clean.ends_with(suffix.as_str())
      {
         return false;
      }
      if let Some(needle) = &self.path_includes
         && !path.contains(needle.as_str())
      {
         return false;
      }
      true
   }
}

struct RequestHook {
   id:     u64,
   plugin: String,
   filter: RequestFilter,
}

/// Outcome of running matching `phoenix.discord.onRequest` hooks.
pub enum RequestAction {
   Pass,
   Drop,
   Replace(Vec<u8>),
}

pub fn add_request_hook(id: u64, plugin: String, filter: RequestFilter) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.push(RequestHook { id, plugin, filter });
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn remove_request_hook(id: u64) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.id != id);
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

pub fn forget_request_hooks(plugin: &str) {
   let mut hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
   hooks.retain(|hook| hook.plugin != plugin);
   REQUEST_HOOK_COUNT.store(hooks.len(), Ordering::Relaxed);
}

/// Relative path under the API base (e.g. `https://discord.com/api/v9/foo` -> `/foo`).
fn api_path(url: &str) -> Option<&str> {
   let base = api_base(url)?;
   let rest = &url[base.len()..];
   rest.starts_with('/').then_some(rest)
}

/// `Networking.request`'s `headers`, an array of `[name, value]` pairs.
fn headers(args: &Value) -> Vec<(String, String)> {
   let Some(Value::Arr(pairs)) = args.field(HEADERS) else {
      return Vec::new();
   };
   pairs
      .iter()
      .filter_map(|pair| {
         let Value::Arr(parts) = pair else {
            return None;
         };
         match parts.as_slice() {
            [Value::Str(name), Value::Str(value)] => Some((name.clone(), value.clone())),
            _ => None,
         }
      })
      .collect()
}

/// `https://host/api/v9` out of an API URL.
fn api_base(url: &str) -> Option<&str> {
   let after = url.find("/api/v")? + "/api/v".len();
   let digits = url[after..]
      .find(|char: char| !char.is_ascii_digit())
      .unwrap_or(url.len() - after);
   (url.starts_with("https://") && digits > 0).then(|| &url[..after + digits])
}

/// Whether an `invoke` frame starting with `head` is one [`observe_call`]
/// reads.
pub fn wants(head: &[u8]) -> bool {
   HostCall::target(head) == Some((NETWORKING, REQUEST))
}

/// Observes one of Phoenix's own API requests and runs any matching
/// `phoenix.discord.onRequest` hooks (filtered in Rust first so non-matching
/// calls never enter QuickJS). Never exposes session headers to JS.
pub fn intercept_call(frame: &[u8]) -> Action {
   let Ok(mut call) = HostCall::decode(frame) else {
      return Action::Pass;
   };
   observe_decoded(&call);
   if REQUEST_HOOK_COUNT.load(Ordering::Relaxed) == 0 {
      return Action::Pass;
   }
   let Some(method) = call.args.field(METHOD).and_then(Value::as_str).map(str::to_owned) else {
      return Action::Pass;
   };
   let Some(path) = call
      .args
      .field(URL)
      .and_then(Value::as_str)
      .and_then(api_path)
      .map(str::to_owned)
   else {
      return Action::Pass;
   };
   let matched = {
      let hooks = REQUEST_HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
      hooks
         .iter()
         .filter(|hook| hook.filter.matches(&method, &path))
         .map(|hook| hook.id)
         .collect::<Vec<_>>()
   };
   if matched.is_empty() {
      return Action::Pass;
   }
   match crate::intercept_request(&matched, &method, &path, &call.body) {
      RequestAction::Pass => Action::Pass,
      RequestAction::Drop => Action::Drop,
      RequestAction::Replace(body) => {
         call.body = body;
         // Drop any stale content-length header so OkHttp uses the new body length.
         if let Value::Obj(ref mut fields) = call.args {
            for (key, value) in fields.iter_mut() {
               if *key == HEADERS
                  && let Value::Arr(pairs) = value
               {
                  pairs.retain(|pair| {
                     !matches!(pair, Value::Arr(parts) if matches!(parts.first(), Some(Value::Str(name)) if name.eq_ignore_ascii_case("content-length")))
                  });
               }
            }
         }
         Action::Replace(call.encode())
      },
   }
}

/// Keeps the session of one of Phoenix's own API requests.
fn observe_decoded(call: &HostCall) {
   if let Some(id) = call.args.field(URL).and_then(Value::as_str).and_then(profile_of) {
      *LAST_PROFILE.lock().unwrap_or_else(PoisonError::into_inner) = Some((id, Instant::now()));
   }
   if let Some(url) = call.args.field(URL).and_then(Value::as_str)
      && url.contains("/oauth2/authorize?")
      && call.args.field(METHOD).and_then(Value::as_str) == Some("POST")
      && let Some(authorizing) = AUTHORIZING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      && url
         .split(['?', '&'])
         .any(|pair| pair.strip_prefix("client_id=") == Some(authorizing.client_id.as_str()))
   {
      authorizing.call = call.args.field(ID).and_then(Value::as_i32);
   }
   let Some(base) = call
      .args
      .field(URL)
      .and_then(Value::as_str)
      .and_then(api_base)
   else {
      return;
   };
   let headers = headers(&call.args)
      .into_iter()
      .filter(|(name, _)| !PER_REQUEST.contains(&name.to_ascii_lowercase().as_str()))
      .collect::<Vec<_>>();
   if !headers
      .iter()
      .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
   {
      return;
   }

   let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
   if session.is_none() {
      log::info!("discord: API session found at {base}");
   }
   *session = Some(Session {
      base: base.to_owned(),
      headers,
   });
}

/// Takes the `@http` replies to phoenix's requests out of a `nativeDispatch`
/// call.
pub fn filter(bytes: &[u8]) -> Action {
   let watching = AUTHORIZING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .is_some_and(|authorizing| authorizing.call.is_some());
   if !watching
      && PENDING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .is_empty()
   {
      return Action::Pass;
   }
   let Ok(items) = decode_dispatch(bytes) else {
      return Action::Pass;
   };
   let count = items.len();
   let taken = items
      .into_iter()
      .filter_map(|item| {
         let DispatchEvent::Host {
            event: HTTP_EVENT,
            args,
            body,
         } = item.event
         else {
            return None;
         };
         let id = args.field(ID).and_then(Value::as_i32)?;
         if id < FIRST_ID {
            let frame = &bytes[item.offset..item.offset + item.len];
            return authorized(id, frame, &body).map(|new| (item.offset, Some(new)));
         }
         let waiter = PENDING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
         // With no waiter the request timed out, and the reply goes nowhere.
         if let Some(waiter) = waiter {
            let _ = waiter.send(Reply {
               status: args
                  .field(STATUS)
                  .and_then(Value::as_i32)
                  .unwrap_or_default(),
               error: args.field(ERROR).and_then(Value::as_str).map(str::to_owned),
               body,
            });
         }
         Some((item.offset, None))
      })
      .collect::<Vec<_>>();

   let dropped = taken.iter().filter(|(_, new)| new.is_none()).count();
   match taken.len() {
      0 => Action::Pass,
      _ if dropped == count => Action::Drop,
      _ => Action::Replace(edit_frames(bytes, &taken)),
   }
}

/// Sends `method path` with Phoenix's session headers, any of them replaced
/// by `headers`, and waits for the answer. Runs on a worker thread.
pub fn send(
   method: &str,
   path: &str,
   headers: Vec<(String, String)>,
   body: Vec<u8>,
) -> Result<Response, DiscordError> {
   if !path.starts_with('/') {
      return Err(DiscordError::Path {
         path: path.to_owned(),
      });
   }
   let (url, mut merged) = {
      let session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
      let session = session.as_ref().ok_or(DiscordError::NoSession)?;
      (format!("{}{path}", session.base), session.headers.clone())
   };
   merged.retain(|(name, _)| {
      !headers
         .iter()
         .any(|(own, _)| own.eq_ignore_ascii_case(name))
   });
   merged.extend(headers);

   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let pairs = merged
      .into_iter()
      .map(|(name, value)| Value::Arr(vec![Value::Str(name), Value::Str(value)]))
      .collect();
   let call = HostCall {
      module: NETWORKING,
      method: REQUEST,
      args: Value::Obj(vec![
         (ID, Value::I32(id)),
         (METHOD, Value::Str(method.to_owned())),
         (URL, Value::Str(url)),
         (HEADERS, Value::Arr(pairs)),
      ]),
      body,
   };

   let (sender, receiver) = mpsc::channel();
   PENDING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(id, sender);
   let forget = || {
      PENDING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .remove(&id);
   };
   if !bridge::invoke(&call.encode()) {
      forget();
      return Err(DiscordError::NoBridge);
   }
   let Ok(reply) = receiver.recv_timeout(TIMEOUT) else {
      forget();
      return Err(DiscordError::Timeout {
         path: path.to_owned(),
      });
   };
   if reply.status == 0 {
      return Err(DiscordError::Network {
         path:    path.to_owned(),
         message: reply.error.unwrap_or_else(|| "no response".to_owned()),
      });
   }
   Ok(Response {
      status: reply.status,
      body:   reply.body,
   })
}

/// What [`authorize`] asks Discord for.
pub struct Authorization {
   pub client_id:     String,
   pub redirect_uri:  String,
   pub scopes:        Vec<String>,
   pub response_type: String,
}

/// Opens Phoenix's own OAuth2 authorize screen for `request` and waits for
/// the user to authorize. Phoenix's authorize request is then answered with
/// Discord's own "authorized" page instead of the redirect, so the code stays
/// in the app, and the redirect URL with the code comes back here. The URL
/// holds a credential and is never logged.
pub fn authorize(request: &Authorization) -> Result<String, DiscordError> {
   let link = format!(
      "discord://action/oauth2/authorize?client_id={}&response_type={}&redirect_uri={}&scope={}",
      encode(&request.client_id),
      encode(&request.response_type),
      encode(&request.redirect_uri),
      encode(&request.scopes.join(" ")),
   );
   static TURN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
   let turn = TURN.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   // A newer authorize drops the older one's sender, which rejects it.
   *AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner) = Some(Authorizing {
      client_id: request.client_id.clone(),
      call:      None,
      waiter:    sender,
      turn,
   });
   if let Err(message) = crate::picker::open(&link) {
      AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner).take();
      return Err(DiscordError::Refused { status: 0, message });
   }
   let location = receiver.recv_timeout(AUTHORIZE_TIMEOUT);
   let mut authorizing = AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner);
   if authorizing.as_ref().is_some_and(|current| current.turn == turn) {
      authorizing.take();
   }
   drop(authorizing);
   location.map_err(|_| DiscordError::Refused {
      status:  0,
      message: "not authorized".to_owned(),
   })
}

/// Rewrites the reply to Phoenix's own authorize request while a plugin waits
/// for it. Returns the replacement frame.
fn authorized(id: i32, frame: &[u8], body: &[u8]) -> Option<Vec<u8>> {
   let mut authorizing = AUTHORIZING.lock().unwrap_or_else(PoisonError::into_inner);
   if authorizing.as_ref()?.call != Some(id) {
      return None;
   }
   let current = authorizing.take()?;
   drop(authorizing);
   let json = serde_json::from_slice::<serde_json::Value>(body).ok()?;
   let location = json.get("location")?.as_str()?.to_owned();
   let _ = current.waiter.send(location);
   let head = &frame[..frame.len() - body.len()];
   let mut out = head.to_vec();
   out.extend_from_slice(br#"{"location":"https://discord.com/oauth2/authorized"}"#);
   Some(out)
}

/// Percent-encodes `text` for a query value.
fn encode(text: &str) -> String {
   let mut out = String::with_capacity(text.len());
   for byte in text.bytes() {
      if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
         out.push(char::from(byte));
      } else {
         out.push_str(&format!("%{byte:02X}"));
      }
   }
   out
}

#[cfg(test)]
mod tests {
   #[test]
   fn encodes_query_values() {
      assert_eq!(
         super::encode("https://a.b/api/auth x"),
         "https%3A%2F%2Fa.b%2Fapi%2Fauth%20x"
      );
   }
}
