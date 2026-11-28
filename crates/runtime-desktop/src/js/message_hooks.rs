mod body;

use std::{
   collections::{HashMap, VecDeque},
   ffi::c_int,
   ops::Range,
   sync::{
      atomic::{AtomicU8, AtomicU64, Ordering},
      mpsc::{self, RecvTimeoutError, Sender},
   },
   thread,
   time::{Duration, Instant},
};

use phoenix_runtime::{
   discord::RequestAction,
   js::{Engine, restart_budget},
};
use rquickjs::{CatchResultExt, Ctx, Exception, Function, Object, Promise, Value, function::This};

use super::Desktop;

const TIMEOUT: Duration = Duration::from_secs(60);
static ACTIVE: AtomicU8 = AtomicU8::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);

#[link(name = "user32")]
unsafe extern "system" {
   fn IsGUIThread(convert: c_int) -> c_int;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
   Send,
   Edit,
}

impl Kind {
   const fn name(self) -> &'static str {
      match self {
         Self::Send => "onPreSend",
         Self::Edit => "onPreEdit",
      }
   }

   const fn flag(self) -> u8 {
      1 << self as u8
   }
}

pub(super) struct Hook<'js> {
   id: u64,
   pub(super) plugin: String,
   kind: Kind,
   handler: Function<'js>,
}

#[derive(Default)]
pub(super) struct Hooks<'js> {
   handlers: Vec<Hook<'js>>,
   waiting: HashMap<u64, Deferred<'js>>,
}

impl Hooks<'_> {
   fn sync_active(&self) {
      let flags = self
         .handlers
         .iter()
         .fold(0, |flags, hook| flags | hook.kind.flag());
      ACTIVE.store(flags, Ordering::Release);
   }

   pub(super) fn remove(&mut self, remove: impl Fn(&Hook<'_>) -> bool) {
      self.handlers.retain(|hook| !remove(hook));
      self
         .waiting
         .retain(|_, wait| self.handlers.iter().any(|hook| hook.id == wait.hook_id));
      self.sync_active();
   }
}

pub(super) fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   messages: &Object<'js>,
) -> rquickjs::Result<()> {
   for kind in [Kind::Send, Kind::Edit] {
      let plugin = plugin.to_owned();
      messages.set(
         kind.name(),
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, handler: Function<'js>| {
            let registry = ctx
               .userdata::<Desktop<'js>>()
               .ok_or_else(|| Exception::throw_internal(&ctx, "no desktop registry"))?;
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
               if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
                  registry
                     .message_hooks
                     .borrow_mut()
                     .remove(|hook| hook.id == id);
               }
            })?;
            let mut hooks = registry.message_hooks.borrow_mut();
            hooks.handlers.push(Hook {
               id,
               plugin: plugin.clone(),
               kind,
               handler,
            });
            hooks.sync_active();
            Ok::<_, rquickjs::Error>(remove)
         })?,
      )?;
   }
   Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Route<'a> {
   channel: &'a str,
   message: Option<&'a str>,
}

impl Route<'_> {
   const fn kind(&self) -> Kind {
      if self.message.is_some() {
         Kind::Edit
      } else {
         Kind::Send
      }
   }
}

fn route<'a>(method: &str, path: &'a str) -> Option<Route<'a>> {
   let clean = path.split(['?', '#']).next()?;
   let (channel, path) = clean.strip_prefix("/channels/")?.split_once('/')?;
   let is_id = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
   if !is_id(channel) {
      return None;
   }
   let message = if method.eq_ignore_ascii_case("POST") && path == "messages" {
      None
   } else if method.eq_ignore_ascii_case("PATCH") {
      Some(path.strip_prefix("messages/").filter(|id| is_id(id))?)
   } else {
      return None;
   };
   Some(Route { channel, message })
}

pub fn intercept(
   method: &str,
   path: &str,
   headers: &[(String, String)],
   body: &[u8],
) -> RequestAction {
   let Some(route) = route(method, path) else {
      return RequestAction::Pass;
   };
   if ACTIVE.load(Ordering::Acquire) & route.kind().flag() == 0 {
      return RequestAction::Pass;
   }
   let content_type = headers
      .iter()
      .find(|(name, _)| name.eq_ignore_ascii_case("content-type"));
   let Some(range) = body::payload(body, content_type.map(|(_, value)| value.as_str())) else {
      log::warn!("message hook: canceling a request with an unsupported message body");
      return RequestAction::Drop;
   };
   let (done, result) = mpsc::channel();
   let pending = Pending::new(route, &body[range.clone()], done, Instant::now() + TIMEOUT);
   let request_id = pending.request_id;
   let deadline = pending.deadline;
   // SAFETY: false only queries whether this thread is a Windows GUI thread.
   let action = if unsafe { IsGUIThread(0) } != 0 {
      let Some(engine) = phoenix_runtime::try_engine() else {
         return RequestAction::Drop;
      };
      start(&engine, pending);
      match result.try_recv() {
         Ok(action) => action,
         Err(mpsc::TryRecvError::Disconnected) => RequestAction::Drop,
         Err(mpsc::TryRecvError::Empty) => {
            engine.with(|ctx| {
               take_waiting(&ctx, request_id);
            });
            log::warn!("message hook: canceling a promise that would block a GUI thread");
            RequestAction::Drop
         },
      }
   } else {
      if thread::Builder::new()
         .spawn(move || {
            if let Some(engine) = phoenix_runtime::engine() {
               start(&engine, pending);
            }
         })
         .is_err()
      {
         return RequestAction::Drop;
      }
      match result.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
         Ok(action) => action,
         Err(RecvTimeoutError::Disconnected) => RequestAction::Drop,
         Err(RecvTimeoutError::Timeout) => {
            let _ = thread::Builder::new().spawn(move || {
               if let Some(engine) = phoenix_runtime::engine() {
                  engine.with(|ctx| {
                     take_waiting(&ctx, request_id);
                  });
               }
            });
            log::warn!("message hook: decision timed out, canceling the request");
            RequestAction::Drop
         },
      }
   };
   replace_payload(action, body, range, headers)
}

fn replace_payload(
   action: RequestAction,
   body: &[u8],
   range: Range<usize>,
   headers: &[(String, String)],
) -> RequestAction {
   let RequestAction::Replace(payload) = action else {
      return action;
   };
   if payload.len() != range.len()
      && headers
         .iter()
         .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
   {
      log::warn!("message hook: cannot resize a body with an explicit content length");
      return RequestAction::Drop;
   }
   if range.len() == body.len() {
      return RequestAction::Replace(payload);
   }
   let merged = [&body[..range.start], &payload, &body[range.end..]].concat();
   RequestAction::Replace(merged)
}

struct Pending {
   request_id: u64,
   hooks: VecDeque<u64>,
   channel: String,
   message: Option<String>,
   body: Vec<u8>,
   changed: bool,
   done: Sender<RequestAction>,
   deadline: Instant,
}

impl Pending {
   fn new(route: Route<'_>, body: &[u8], done: Sender<RequestAction>, deadline: Instant) -> Self {
      Self {
         request_id: NEXT.fetch_add(1, Ordering::Relaxed),
         hooks: VecDeque::new(),
         channel: route.channel.to_owned(),
         message: route.message.map(str::to_owned),
         body: body.to_vec(),
         changed: false,
         done,
         deadline,
      }
   }
}

struct Deferred<'js> {
   hook_id: u64,
   message: Object<'js>,
   before: String,
   pending: Pending,
}

fn start(engine: &Engine, mut pending: Pending) {
   let kind = if pending.message.is_some() {
      Kind::Edit
   } else {
      Kind::Send
   };
   engine.with(|ctx| {
      pending.hooks = ctx
         .userdata::<Desktop<'_>>()
         .map_or_else(VecDeque::new, |registry| {
            registry
               .message_hooks
               .borrow()
               .handlers
               .iter()
               .filter(|hook| hook.kind == kind)
               .map(|hook| hook.id)
               .collect()
         });
      run(&ctx, pending);
   });
}

fn run<'js>(ctx: &Ctx<'js>, mut pending: Pending) {
   while let Some(id) = pending.hooks.pop_front() {
      if Instant::now() >= pending.deadline {
         return;
      }
      let hook = ctx.userdata::<Desktop<'js>>().and_then(|registry| {
         let hooks = registry.message_hooks.borrow();
         let hook = hooks.handlers.iter().find(|hook| hook.id == id)?;
         Some((hook.plugin.clone(), hook.kind, hook.handler.clone()))
      });
      let Some((plugin, kind, handler)) = hook else {
         continue;
      };
      restart_budget(ctx);
      let called = (|| {
         let message = ctx
            .json_parse(pending.body.clone())?
            .into_object()
            .ok_or_else(|| Exception::throw_type(ctx, "expected a message object"))?;
         let before = message_json(ctx, &message)?;
         let argument = Object::new(ctx.clone())?;
         argument.set("channelId", pending.channel.as_str())?;
         if let Some(id) = &pending.message {
            argument.set("messageId", id.as_str())?;
         }
         let result = handler.call::<_, Value<'js>>((message.clone(), argument))?;
         Ok((result, message, before))
      })();
      let Ok((result, message, before)) = called.catch(ctx) else {
         log::warn!("[{plugin}] {} failed, canceling the request", kind.name());
         return;
      };
      if let Some(promise) = result.as_promise() {
         if let Some(result) = promise.result::<Value<'js>>() {
            match result.catch(ctx) {
               Ok(value) if apply(ctx, &mut pending, id, &message, &before, &value) => continue,
               _ => return,
            }
         }
         let deferred = Deferred {
            hook_id: id,
            message,
            before,
            pending,
         };
         if defer(ctx, promise, deferred).catch(ctx).is_err() {
            log::warn!(
               "[{plugin}] {} promise failed, canceling the request",
               kind.name()
            );
         }
         return;
      }
      if !apply(ctx, &mut pending, id, &message, &before, &result) {
         return;
      }
   }
   if Instant::now() < pending.deadline {
      let action = if pending.changed {
         RequestAction::Replace(pending.body)
      } else {
         RequestAction::Pass
      };
      let _ = pending.done.send(action);
   }
}

fn take_waiting<'js>(ctx: &Ctx<'js>, request_id: u64) -> Option<Deferred<'js>> {
   ctx.userdata::<Desktop<'js>>()?
      .message_hooks
      .borrow_mut()
      .waiting
      .remove(&request_id)
}

fn defer<'js>(
   ctx: &Ctx<'js>,
   promise: &Promise<'js>,
   deferred: Deferred<'js>,
) -> rquickjs::Result<()> {
   let request_id = deferred.pending.request_id;
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return Ok(());
   };
   {
      let mut hooks = registry.message_hooks.borrow_mut();
      if !hooks
         .handlers
         .iter()
         .any(|hook| hook.id == deferred.hook_id)
      {
         return Ok(());
      }
      hooks.waiting.insert(request_id, deferred);
   }
   drop(registry);
   let attached = (|| {
      let ok = Function::new(ctx.clone(), move |ctx: Ctx<'js>, value: Value<'js>| {
         let Some(Deferred {
            hook_id,
            message,
            before,
            mut pending,
         }) = take_waiting(&ctx, request_id)
         else {
            return;
         };
         if apply(&ctx, &mut pending, hook_id, &message, &before, &value) {
            run(&ctx, pending);
         }
      })?;
      let fail = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         take_waiting(&ctx, request_id);
      })?;
      let constructor: Object<'js> = ctx.globals().get("Promise")?;
      let prototype: Object<'js> = constructor.get("prototype")?;
      let then: Function<'js> = prototype.get("then")?;
      then.call::<_, ()>((This(promise.clone()), ok, fail))
   })();
   if attached.is_err() {
      take_waiting(ctx, request_id);
   }
   attached
}

fn message_json<'js>(ctx: &Ctx<'js>, message: &Object<'js>) -> rquickjs::Result<String> {
   ctx.json_stringify(message.clone())?
      .ok_or_else(|| Exception::throw_type(ctx, "expected a JSON message"))?
      .to_string()
}

fn apply<'js>(
   ctx: &Ctx<'js>,
   pending: &mut Pending,
   id: u64,
   message: &Object<'js>,
   before: &str,
   result: &Value<'js>,
) -> bool {
   if result.as_bool() == Some(false)
      || Instant::now() >= pending.deadline
      || !ctx.userdata::<Desktop<'js>>().is_some_and(|registry| {
         registry
            .message_hooks
            .borrow()
            .handlers
            .iter()
            .any(|hook| hook.id == id)
      })
   {
      return false;
   }
   restart_budget(ctx);
   let Ok(after) = message_json(ctx, message).catch(ctx) else {
      return false;
   };
   if !after.starts_with('{') {
      return false;
   }
   if after != before {
      let merged = (|| {
         let exact = serde_json::from_slice(&pending.body)?;
         let before = serde_json::from_str(before)?;
         let mut after = serde_json::from_str(&after)?;
         if restore_unchanged(&exact, &before, &mut after) {
            serde_json::to_vec(&after).map(Some)
         } else {
            Ok(None)
         }
      })();
      let Ok(Some(merged)) = merged else {
         return false;
      };
      pending.body = merged;
      pending.changed = true;
   }
   true
}

fn restore_unchanged(
   exact: &serde_json::Value,
   before: &serde_json::Value,
   after: &mut serde_json::Value,
) -> bool {
   use serde_json::Value::{Array, Object};
   if exact == before {
      return true;
   }
   if before == after {
      after.clone_from(exact);
      return true;
   }
   match (exact, before, after) {
      (Object(exact), Object(before), Object(after)) => {
         for (key, after) in after {
            if let (Some(exact), Some(before)) = (exact.get(key), before.get(key))
               && !restore_unchanged(exact, before, after)
            {
               return false;
            }
         }
      },
      (Array(_), Array(_), Array(_)) => return false,
      _ => {},
   }
   true
}

#[cfg(test)]
mod tests {
   use std::{cell::Cell, rc::Rc};

   use phoenix_runtime::js::compile;
   use phoenix_sigs::registry::GatewayRegistry;
   use rquickjs::Persistent;
   use serde_json::json;

   use super::*;

   fn setup() -> Engine {
      let engine = Engine::new(&GatewayRegistry::default()).unwrap();
      engine.with(|ctx| assert!(ctx.store_userdata(Desktop::default()).is_ok()));
      engine
   }

   fn install<'js>(ctx: &Ctx<'js>, api: &Object<'js>, code: &str) {
      register(ctx, "test", api).unwrap();
      compile(ctx, code)
         .unwrap()
         .call::<_, ()>((api.clone(),))
         .unwrap();
   }

   fn request(
      engine: &Engine,
      method: &str,
      path: &str,
      body: &[u8],
   ) -> mpsc::Receiver<RequestAction> {
      let (done, result) = mpsc::channel();
      start(
         engine,
         Pending::new(
            route(method, path).unwrap(),
            body,
            done,
            Instant::now() + TIMEOUT,
         ),
      );
      result
   }

   #[test]
   fn matches_channel_messages_only() {
      assert_eq!(
         route("POST", "/channels/42/messages?x=1"),
         Some(Route {
            channel: "42",
            message: None
         })
      );
      assert_eq!(
         route("PATCH", "/channels/42/messages/9007199254740993?x=1"),
         Some(Route {
            channel: "42",
            message: Some("9007199254740993")
         })
      );
      for (method, path) in [
         ("POST", "/channels/42/messages/15"),
         ("PATCH", "/channels/42/messages"),
         ("PATCH", "/channels/42/messages/15/reactions"),
         ("PATCH", "/channels/42/messages/"),
         ("PATCH", "/webhooks/42/messages/15"),
         ("GET", "/channels/42/messages"),
      ] {
         assert_eq!(route(method, path), None);
      }
   }

   #[test]
   fn waits_for_a_pending_send_and_passes_mutations_to_the_next_hook() {
      let engine = setup();
      let resolve = engine.with(|ctx| {
         let api = Object::new(ctx.clone()).unwrap();
         let (decision, resolve, _reject) = ctx.promise().unwrap();
         api.set("decision", decision).unwrap();
         install(
            &ctx,
            &api,
            r#"
            phoenix.onPreSend(async (message, ctx) => {
               await phoenix.decision;
               message.content = ctx.channelId + ":" + message.content;
            });
            phoenix.onPreSend(message => { message.content += "!"; });
            phoenix.onPreEdit(() => false);
         "#,
         );
         Persistent::save(&ctx, resolve)
      });
      let result = request(
         &engine,
         "POST",
         "/channels/42/messages",
         br#"{"content":"hello","attachments":[{"id":"0","filename":"a.txt"}],"nonce":9007199254740993}"#,
      );
      assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
      engine.with(|ctx| resolve.restore(&ctx).unwrap().call::<_, ()>(()).unwrap());
      let RequestAction::Replace(body) = result.try_recv().unwrap() else {
         panic!("expected a replacement");
      };
      assert_eq!(
         serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
         json!({"content":"42:hello!","attachments":[{"id":"0","filename":"a.txt"}],"nonce":9_007_199_254_740_993_u64})
      );
   }

   #[test]
   fn edit_can_cancel_asynchronously_and_does_not_run_later_handlers() {
      let engine = setup();
      let called = Rc::new(Cell::new(false));
      let called_in_hook = Rc::clone(&called);
      engine.with(|ctx| {
         let api = Object::new(ctx.clone()).unwrap();
         api.set(
            "unexpected",
            Function::new(ctx.clone(), move || called_in_hook.set(true)).unwrap(),
         )
         .unwrap();
         install(
            &ctx,
            &api,
            r#"
            phoenix.onPreSend(() => phoenix.unexpected());
            phoenix.onPreEdit(async (_message, ctx) => {
               await Promise.resolve();
               if (ctx.channelId === "42" && ctx.messageId === "15") return false;
            });
            phoenix.onPreEdit(() => phoenix.unexpected());
         "#,
         );
      });
      let result = request(
         &engine,
         "PATCH",
         "/channels/42/messages/15",
         br#"{"content":"edit"}"#,
      );
      assert!(matches!(
         result.try_recv(),
         Err(mpsc::TryRecvError::Disconnected)
      ));
      assert!(!called.get());
   }

   #[test]
   fn removing_a_hook_cancels_its_pending_request() {
      let engine = setup();
      let remove = engine.with(|ctx| {
         let api = Object::new(ctx.clone()).unwrap();
         register(&ctx, "test", &api).unwrap();
         let remove = compile(
            &ctx,
            "return phoenix.onPreSend(() => new Promise(() => {}));",
         )
         .unwrap()
         .call::<_, Function<'_>>((api,))
         .unwrap();
         Persistent::save(&ctx, remove)
      });
      let send = request(
         &engine,
         "POST",
         "/channels/42/messages",
         br#"{"content":"unchanged"}"#,
      );
      assert!(matches!(send.try_recv(), Err(mpsc::TryRecvError::Empty)));
      engine.with(|ctx| remove.restore(&ctx).unwrap().call::<_, ()>(()).unwrap());
      assert!(matches!(
         send.try_recv(),
         Err(mpsc::TryRecvError::Disconnected)
      ));
      let next = request(
         &engine,
         "POST",
         "/channels/42/messages",
         br#"{"content":"new"}"#,
      );
      assert!(matches!(next.try_recv(), Ok(RequestAction::Pass)));
   }

   #[test]
   fn canceling_the_wait_releases_it_and_ignores_a_late_decision() {
      let engine = setup();
      let resolve = engine.with(|ctx| {
         let api = Object::new(ctx.clone()).unwrap();
         let (decision, resolve, _reject) = ctx.promise().unwrap();
         api.set("decision", decision).unwrap();
         install(&ctx, &api, "phoenix.onPreEdit(() => phoenix.decision);");
         Persistent::save(&ctx, resolve)
      });
      let (done, result) = mpsc::channel();
      let pending = Pending::new(
         route("PATCH", "/channels/42/messages/15").unwrap(),
         br#"{"content":"edit"}"#,
         done,
         Instant::now() + TIMEOUT,
      );
      let request_id = pending.request_id;
      start(&engine, pending);
      assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
      engine.with(|ctx| {
         assert!(take_waiting(&ctx, request_id).is_some());
         resolve.restore(&ctx).unwrap().call::<_, ()>(()).unwrap();
         assert!(take_waiting(&ctx, request_id).is_none());
      });
      assert!(matches!(
         result.try_recv(),
         Err(mpsc::TryRecvError::Disconnected)
      ));
   }

   #[test]
   fn multipart_replacement_only_changes_the_message_json() {
      let body = b"head\xff{\"content\":\"old\"}\r\ntail";
      let range = 5..body.len() - 6;
      let replacement = br#"{"content":"longer"}"#.to_vec();
      let RequestAction::Replace(result) = replace_payload(
         RequestAction::Replace(replacement.clone()),
         body,
         range.clone(),
         &[],
      ) else {
         panic!("expected a replacement");
      };
      assert_eq!(result, b"head\xff{\"content\":\"longer\"}\r\ntail");
      let header = ("Content-Length".to_owned(), body.len().to_string());
      assert!(matches!(
         replace_payload(RequestAction::Replace(replacement), body, range, &[header]),
         RequestAction::Drop
      ));
   }

   #[test]
   fn refuses_an_array_edit_that_can_lose_number_precision() {
      let engine = setup();
      engine.with(|ctx| {
         let api = Object::new(ctx.clone()).unwrap();
         install(
            &ctx,
            &api,
            r#"
            phoenix.onPreSend(message => { message.attachments.push({ id: "1" }); });
         "#,
         );
      });
      let result = request(
         &engine,
         "POST",
         "/channels/42/messages",
         br#"{"attachments":[{"id":9007199254740993}]}"#,
      );
      assert!(matches!(
         result.try_recv(),
         Err(mpsc::TryRecvError::Disconnected)
      ));
   }
}
