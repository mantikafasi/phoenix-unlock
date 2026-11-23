//! Localhost-only HTTP debug shell on `127.0.0.1:4242` for ADB development
//! (`adb forward tcp:4242 tcp:4242`). Off by default; gated behind Debug →
//! Remote debug shell in Phoenix Unlocker settings.

use std::{
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
   sync::atomic::{
      AtomicBool,
      Ordering,
   },
   thread,
   time::Duration,
};

pub const ADDR: &str = "127.0.0.1:4242";
const MAX_BODY: usize = 4 << 20;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

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

   let Ok((method, path, body)) = read_request(&stream) else {
      return;
   };
   let (status, content_type, response) = dispatch(&method, &path, &body);
   let header = format!(
      "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
      response.len()
   );
   let _ = stream.write_all(header.as_bytes());
   let _ = stream.write_all(&response);
}

fn read_request(stream: &TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
   let mut reader = BufReader::new(stream);
   let mut request_line = String::new();
   if reader.read_line(&mut request_line)? == 0 {
      return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
   }
   let mut parts = request_line.split_whitespace();
   let method = parts.next().unwrap_or_default().to_owned();
   let raw_path = parts.next().unwrap_or("/");
   let path = raw_path.split('?').next().unwrap_or("/").to_owned();

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
   Ok((method, path, body))
}

fn dispatch(method: &str, path: &str, body: &[u8]) -> (&'static str, &'static str, Vec<u8>) {
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
         "endpoints: GET /status, GET /plugins, GET|PUT|DELETE /plugins/<name>, POST /plugins/<name>/{reload,enable,disable}, POST /eval, GET /logs",
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
