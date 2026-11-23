//! Localhost-only HTTP debug shell on `127.0.0.1:4242` for ADB development
//! (`adb forward tcp:4242 tcp:4242`). Off by default; gated behind Debug →
//! Remote debug shell in Phoenix Unlocker settings.

use std::{
   collections::VecDeque,
   io::{
      BufRead,
      BufReader,
      Read,
      Write,
   },
   net::{
      TcpListener,
      TcpStream,
   },
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
   time::Duration,
};

use phoenix_ui::Op;

use crate::patches::mirror::wire_to_json;

pub const ADDR: &str = "127.0.0.1:4242";
const MAX_BODY: usize = 4 << 20;
const MAX_OPS_LOG: usize = 64;
const MAX_EVENTS_LOG: usize = 128;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static OPS_LOG: LazyLock<Mutex<VecDeque<serde_json::Value>>> =
   LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_OPS_LOG)));
static EVENTS_LOG: LazyLock<Mutex<VecDeque<serde_json::Value>>> =
   LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_EVENTS_LOG)));

/// Records a decoded Phoenix UI-op batch into the debug ring buffer when the
/// remote debug shell is enabled. Zero-cost atomic check when disabled.
pub fn record_ops(ops: &[Op]) {
   if !enabled() || ops.is_empty() {
      return;
   }
   let items = ops.iter().map(op_to_json).collect::<Vec<_>>();
   let entry = serde_json::json!({
      "count": items.len(),
      "ops": items,
   });
   let mut log = OPS_LOG.lock().unwrap_or_else(PoisonError::into_inner);
   if log.len() == MAX_OPS_LOG {
      log.pop_front();
   }
   log.push_back(entry);
}

/// Records a view or host event into the debug ring buffer when the remote
/// debug shell is enabled. Never pass HTTP headers or credentials here.
pub fn record_event(source: &str, mut detail: serde_json::Value) {
   if !enabled() {
      return;
   }
   if let Some(map) = detail.as_object_mut() {
      map.insert("source".to_owned(), serde_json::json!(source));
   }
   let mut log = EVENTS_LOG.lock().unwrap_or_else(PoisonError::into_inner);
   if log.len() == MAX_EVENTS_LOG {
      log.pop_front();
   }
   log.push_back(detail);
}

fn props_to_json(props: &phoenix_ui::Props) -> serde_json::Value {
   let mut map = serde_json::Map::new();
   for (key, value) in props {
      let name = key
         .name()
         .map_or_else(|| format!("#{}", key.key().get()), str::to_owned);
      map.insert(name, wire_to_json(value));
   }
   serde_json::Value::Object(map)
}

fn op_to_json(op: &Op) -> serde_json::Value {
   match op {
      Op::Create { id, kind, props } => {
         serde_json::json!({
            "op": "create",
            "id": id,
            "kind": kind.as_str(),
            "props": props_to_json(props),
         })
      },
      Op::Update { id, props } => {
         serde_json::json!({
            "op": "update",
            "id": id,
            "props": props_to_json(props),
         })
      },
      Op::Children { id, ids } => serde_json::json!({ "op": "children", "id": id, "ids": ids }),
      Op::ChildEdits { id, edits } => {
         let edits = edits
            .iter()
            .map(|edit| serde_json::json!({ "keep": edit.keep, "del": edit.del, "ins": edit.ins }))
            .collect::<Vec<_>>();
         serde_json::json!({ "op": "childEdits", "id": id, "edits": edits })
      },
      Op::Delete { ids } => serde_json::json!({ "op": "delete", "ids": ids }),
      Op::Root { id } => serde_json::json!({ "op": "root", "id": id }),
      Op::Focus { id } => serde_json::json!({ "op": "focus", "id": id }),
      Op::ScrollTo { id, value } => {
         serde_json::json!({ "op": "scrollTo", "id": id, "value": wire_to_json(value) })
      },
      Op::RequestFrame => serde_json::json!({ "op": "requestFrame" }),
      Op::Motion { words } => serde_json::json!({ "op": "motion", "words": words.len() }),
   }
}

#[must_use]
pub fn enabled() -> bool {
   ENABLED.load(Ordering::Relaxed)
}

pub fn set_flag(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_enabled(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
   if on {
      ensure_started();
   }
}

pub fn ensure_started() {
   if !enabled() {
      return;
   }
   if RUNNING.swap(true, Ordering::AcqRel) {
      return;
   }
   thread::spawn(|| {
      let listener = match TcpListener::bind(ADDR) {
         Ok(listener) => listener,
         Err(err) => {
            log::error!("remote shell: bind {ADDR} failed: {err}");
            RUNNING.store(false, Ordering::Release);
            return;
         },
      };
      let _ = listener.set_nonblocking(true);
      log::info!("remote shell listening on http://{ADDR} (use `adb forward tcp:4242 tcp:4242`)");
      while enabled() {
         match listener.accept() {
            Ok((stream, _)) => {
               thread::spawn(move || handle(stream));
            },
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
               thread::sleep(Duration::from_millis(100));
            },
            Err(err) => {
               log::warn!("remote shell accept: {err}");
               thread::sleep(Duration::from_millis(250));
            },
         }
      }
      log::info!("remote shell stopped");
      RUNNING.store(false, Ordering::Release);
   });
}

fn handle(mut stream: TcpStream) {
   let _ = stream.set_nonblocking(false);
   let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
   let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

   let Ok((method, path, query, body)) = read_request(&stream) else {
      return;
   };
   let (status, content_type, response) = dispatch(&method, &path, &query, &body);
   let header = format!(
      "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
      response.len()
   );
   let _ = stream.write_all(header.as_bytes());
   let _ = stream.write_all(&response);
}

fn read_request(stream: &TcpStream) -> std::io::Result<(String, String, String, Vec<u8>)> {
   let mut reader = BufReader::new(stream);
   let mut request_line = String::new();
   if reader.read_line(&mut request_line)? == 0 {
      return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
   }
   let mut parts = request_line.split_whitespace();
   let method = parts.next().unwrap_or_default().to_owned();
   let raw_path = parts.next().unwrap_or("/");
   let (path, query) = match raw_path.split_once('?') {
      Some((path, query)) => (path.to_owned(), query.to_owned()),
      None => (raw_path.to_owned(), String::new()),
   };

   let mut content_length: usize = 0;
   loop {
      let mut line = String::new();
      if reader.read_line(&mut line)? == 0 {
         break;
      }
      let trimmed = line.trim_end_matches(['\r', '\n']);
      if trimmed.is_empty() {
         break;
      }
      if let Some((name, value)) = trimmed.split_once(':')
         && name.trim().eq_ignore_ascii_case("content-length")
      {
         content_length = value.trim().parse::<usize>().unwrap_or(0).min(MAX_BODY);
      }
   }
   let mut body = vec![0u8; content_length];
   if content_length > 0 {
      reader.read_exact(&mut body)?;
   }
   Ok((method, path, query, body))
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
   query
      .split('&')
      .filter_map(|pair| pair.split_once('='))
      .find(|(k, _)| *k == key)
      .map(|(_, v)| v)
}

fn dispatch(
   method: &str,
   path: &str,
   query: &str,
   body: &[u8],
) -> (&'static str, &'static str, Vec<u8>) {
   match (method, path) {
      ("GET", "/" | "/status") => {
         let plugins = crate::plugins()
            .into_iter()
            .map(|plugin| {
               serde_json::json!({
                  "name": plugin.name,
                  "enabled": plugin.enabled,
                  "installed": plugin.installed,
                  "error": plugin.error,
                  "version": plugin.header.version,
                  "description": plugin.header.description,
               })
            })
            .collect::<Vec<_>>();
         let json = serde_json::json!({
            "version": crate::update::VERSION,
            "staff": crate::staff_enabled(),
            "remote_shell": enabled(),
            "plugins": plugins,
         });
         ok_json(&json)
      },
      ("GET", "/plugins") => {
         let plugins = crate::plugins()
            .into_iter()
            .map(|plugin| {
               serde_json::json!({
                  "name": plugin.name,
                  "enabled": plugin.enabled,
                  "installed": plugin.installed,
                  "error": plugin.error,
                  "version": plugin.header.version,
                  "author": plugin.header.author,
                  "description": plugin.header.description,
               })
            })
            .collect::<Vec<_>>();
         ok_json(&serde_json::Value::Array(plugins))
      },
      ("GET", "/logs") => {
         let mut lines = crate::console::lines();
         lines.reverse();
         let text = lines
            .into_iter()
            .map(|line| format!("{} [{}] {}", line.time, line.level, line.text))
            .collect::<Vec<_>>()
            .join("\n");
         ("200 OK", "text/plain; charset=utf-8", text.into_bytes())
      },
      ("POST", "/eval") => {
         let Ok(code) = std::str::from_utf8(body) else {
            return err("400 Bad Request", "eval body must be UTF-8");
         };
         match crate::evaluate_capture(code) {
            Ok(result) => ("200 OK", "text/plain; charset=utf-8", result.into_bytes()),
            Err(message) => err("500 Internal Server Error", &message),
         }
      },
      ("GET", "/ops") => {
         let filter = query_param(query, "match");
         let limit = query_param(query, "limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20);
         let log = OPS_LOG.lock().unwrap_or_else(PoisonError::into_inner);
         let items = log
            .iter()
            .rev()
            .filter(|entry| {
               filter.is_none_or(|needle| {
                  serde_json::to_string(entry)
                     .is_ok_and(|text| text.contains(needle))
               })
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
         ok_json(&serde_json::Value::Array(items))
      },
      ("GET", "/events") => {
         let filter = query_param(query, "match");
         let limit = query_param(query, "limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(50);
         let log = EVENTS_LOG.lock().unwrap_or_else(PoisonError::into_inner);
         let items = log
            .iter()
            .rev()
            .filter(|entry| {
               filter.is_none_or(|needle| {
                  serde_json::to_string(entry)
                     .is_ok_and(|text| text.contains(needle))
               })
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
         ok_json(&serde_json::Value::Array(items))
      },
      ("POST", "/clear") => {
         OPS_LOG
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
         EVENTS_LOG
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
         ok_json(&serde_json::json!({ "cleared": true }))
      },
      ("GET", "/tree") => {
         let depth = query_param(query, "depth")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(6);
         ok_json(&crate::patches::dump_tree(None, depth))
      },
      ("GET", _) if path.starts_with("/tree/") => {
         let Ok(id) = path["/tree/".len()..].parse::<i32>() else {
            return err("400 Bad Request", "invalid node id");
         };
         let depth = query_param(query, "depth")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(6);
         ok_json(&crate::patches::dump_tree(Some(id), depth))
      },
      ("GET", "/update") => {
         let found = crate::update::found().map(|u| {
            serde_json::json!({
               "loader": u.loader,
               "plugins": u.plugins,
            })
         });
         ok_json(&serde_json::json!({
            "version": crate::update::VERSION,
            "busy": crate::update::busy(),
            "status": crate::update::status(),
            "found": found,
         }))
      },
      ("POST", "/update/check") => {
         crate::update::check();
         ok_json(&serde_json::json!({ "started": true }))
      },
      ("POST", "/update/apply") => {
         crate::update::apply();
         ok_json(&serde_json::json!({ "started": true }))
      },
      ("POST", "/update/self") => {
         crate::update::self_update_from_site();
         ok_json(&serde_json::json!({ "started": true }))
      },
      ("POST", "/debug/staff") => {
         let text = String::from_utf8_lossy(body);
         let on = matches!(text.trim(), "1" | "true" | "on");
         crate::set_staff_enabled(on);
         crate::settings::refresh();
         ok_json(&serde_json::json!({ "staff": crate::staff_enabled() }))
      },
      _ if path.starts_with("/plugins/") => plugin_route(method, &path["/plugins/".len()..], body),
      _ => err(
         "404 Not Found",
         "endpoints: GET /status, GET /plugins, GET|PUT|DELETE /plugins/<name>, POST /plugins/<name>/{reload,enable,disable}, POST /eval, GET /logs, GET /ops[?match=&limit=], GET /events[?match=&limit=], GET /tree[/<id>][?depth=], POST /clear",
      ),
   }
}

fn plugin_route(method: &str, rest: &str, body: &[u8]) -> (&'static str, &'static str, Vec<u8>) {
   let (name, action) = match rest.split_once('/') {
      Some((name, action)) => (name, Some(action)),
      None => (rest, None),
   };
   let valid = !name.is_empty()
      && name
         .bytes()
         .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
   if !valid {
      return err("400 Bad Request", "invalid plugin name");
   }
   match (method, action) {
      ("GET", None) => match crate::source(name) {
         Some(source) => ("200 OK", "text/javascript; charset=utf-8", source.into_bytes()),
         None => err("404 Not Found", "plugin not found"),
      },
      ("PUT" | "POST", None) => {
         let Ok(source) = String::from_utf8(body.to_vec()) else {
            return err("400 Bad Request", "plugin source must be UTF-8");
         };
         match crate::save_source(name, source) {
            Ok(()) => {
               crate::settings::refresh();
               ok_json(&serde_json::json!({ "installed": name }))
            },
            Err(message) => err("500 Internal Server Error", &message.to_string()),
         }
      },
      ("DELETE", None) => match crate::uninstall(name) {
         Ok(()) => {
            crate::settings::refresh();
            ok_json(&serde_json::json!({ "uninstalled": name }))
         },
         Err(message) => err("500 Internal Server Error", &message.to_string()),
      },
      ("POST", Some("reload")) => {
         crate::reload(name);
         crate::settings::refresh();
         ok_json(&serde_json::json!({ "reloaded": name }))
      },
      ("POST", Some("enable")) => {
         crate::set_enabled(name, true);
         crate::settings::refresh();
         ok_json(&serde_json::json!({ "name": name, "enabled": true }))
      },
      ("POST", Some("disable")) => {
         crate::set_enabled(name, false);
         crate::settings::refresh();
         ok_json(&serde_json::json!({ "name": name, "enabled": false }))
      },
      _ => err("404 Not Found", "unknown plugin route"),
   }
}

fn ok_json(value: &serde_json::Value) -> (&'static str, &'static str, Vec<u8>) {
   (
      "200 OK",
      "application/json; charset=utf-8",
      serde_json::to_vec_pretty(value).unwrap_or_default(),
   )
}

fn err(status: &'static str, message: &str) -> (&'static str, &'static str, Vec<u8>) {
   (status, "text/plain; charset=utf-8", message.as_bytes().to_vec())
}
