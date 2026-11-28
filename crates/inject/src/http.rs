//! Downloads through `WinHTTP`, which follows the system proxy and TLS store.
//! A hidden `curl.exe` child is one of the dropper patterns Defender flags.

use std::{
   ffi::c_void,
   fs::File,
   io::{
      self,
      BufWriter,
      Write as _,
   },
   path::Path,
   ptr,
   time::{
      Duration,
      Instant,
   },
};

use misstep::ResultExt as _;

const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const FLAG_SECURE: u32 = 0x0080_0000;
const QUERY_STATUS_CODE_NUMBER: u32 = 0x13 | 0x2000_0000;
const OPTION_IPV6_FAST_FALLBACK: u32 = 140;
/// How long a read may wait on a silent connection.
const STALL: Duration = Duration::from_secs(30);

#[link(name = "winhttp")]
unsafe extern "system" {
   fn WinHttpOpen(
      agent: *const u16,
      access: u32,
      proxy: *const u16,
      bypass: *const u16,
      flags: u32,
   ) -> *mut c_void;
   fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
   fn WinHttpSetTimeouts(
      handle: *mut c_void,
      resolve: i32,
      connect: i32,
      send: i32,
      receive: i32,
   ) -> i32;
   fn WinHttpSetOption(handle: *mut c_void, option: u32, buffer: *const c_void, len: u32) -> i32;
   fn WinHttpConnect(
      session: *mut c_void,
      host: *const u16,
      port: u16,
      reserved: u32,
   ) -> *mut c_void;
   fn WinHttpOpenRequest(
      connect: *mut c_void,
      verb: *const u16,
      object: *const u16,
      version: *const u16,
      referrer: *const u16,
      accept: *const *const u16,
      flags: u32,
   ) -> *mut c_void;
   fn WinHttpSendRequest(
      request: *mut c_void,
      headers: *const u16,
      headers_len: u32,
      body: *const c_void,
      body_len: u32,
      total_len: u32,
      context: usize,
   ) -> i32;
   fn WinHttpReceiveResponse(request: *mut c_void, reserved: *mut c_void) -> i32;
   fn WinHttpQueryHeaders(
      request: *mut c_void,
      level: u32,
      name: *const u16,
      buffer: *mut c_void,
      len: *mut u32,
      index: *mut u32,
   ) -> i32;
   fn WinHttpReadData(request: *mut c_void, buffer: *mut c_void, len: u32, read: *mut u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GetModuleHandleW(name: *const u16) -> *mut c_void;
   fn FormatMessageW(
      flags: u32,
      source: *const c_void,
      id: u32,
      language: u32,
      buffer: *mut u16,
      size: u32,
      args: *const c_void,
   ) -> u32;
}

struct Handle(*mut c_void);

impl Handle {
   fn new(raw: *mut c_void, call: &str) -> misstep::Result<Self> {
      if raw.is_null() {
         misstep::bail!("{call}: {}", last_error());
      }
      Ok(Self(raw))
   }
}

impl Drop for Handle {
   fn drop(&mut self) {
      // SAFETY: a live handle this guard owns.
      unsafe { WinHttpCloseHandle(self.0) };
   }
}

fn check(ok: i32, call: &str) -> misstep::Result<()> {
   if ok == 0 {
      misstep::bail!("{call}: {}", last_error());
   }
   Ok(())
}

/// The thread's last error. `WinHTTP` words its codes (12000 to 12999) in the message table of
/// `winhttp.dll`, which `io::Error` never reads.
fn last_error() -> String {
   const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x0200;
   const FORMAT_MESSAGE_FROM_HMODULE: u32 = 0x0800;

   let err = io::Error::last_os_error();
   let Some(code) = err.raw_os_error().filter(|code| (12000..13000).contains(code)) else {
      return err.to_string();
   };
   let module = wide("winhttp.dll");
   let mut text = [0u16; 512];
   // SAFETY: winhttp.dll is linked so the module handle is live, and `text` holds the 512 units
   // passed as its size.
   let len = unsafe {
      FormatMessageW(
         FORMAT_MESSAGE_FROM_HMODULE | FORMAT_MESSAGE_IGNORE_INSERTS,
         GetModuleHandleW(module.as_ptr()),
         code.cast_unsigned(),
         0,
         text.as_mut_ptr(),
         512,
         ptr::null(),
      )
   };
   text
      .get(..len as usize)
      .filter(|words| !words.is_empty())
      .map_or_else(
         || err.to_string(),
         |words| format!("{} (os error {code})", String::from_utf16_lossy(words).trim_end()),
      )
}

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain([0]).collect()
}

/// `url`'s body as text, within `limit`.
pub fn text(url: &str, limit: Duration) -> misstep::Result<String> {
   let mut body = Vec::new();
   get(url, limit, |chunk| {
      body.extend_from_slice(chunk);
      Ok(())
   })?;
   Ok(String::from_utf8(body).context("the response is not UTF-8")?)
}

/// Writes `url`'s body to `dest`, within `limit`.
pub fn download(url: &str, dest: &Path, limit: Duration) -> misstep::Result<()> {
   let mut file = BufWriter::new(File::create(dest).context("creating the download")?);
   get(url, limit, |chunk| {
      Ok(file.write_all(chunk).context("writing the download")?)
   })?;
   Ok(file.flush().context("writing the download")?)
}

struct Target<'url> {
   secure: bool,
   host:   &'url str,
   port:   u16,
   path:   &'url str,
}

impl<'url> Target<'url> {
   fn parse(url: &'url str) -> misstep::Result<Self> {
      let (secure, rest) = match url.split_once("://") {
         Some(("https", rest)) => (true, rest),
         Some(("http", rest)) => (false, rest),
         _ => misstep::bail!("{url} is not an http(s) URL"),
      };
      let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
      let (host, port) = match authority.rsplit_once(':') {
         Some((host, port)) => (host, port.parse::<u16>().context("the URL's port")?),
         None => (authority, if secure { 443 } else { 80 }),
      };
      Ok(Self {
         secure,
         host,
         port,
         path,
      })
   }
}

/// GETs `url`, handing the body to `sink`. The whole request must finish
/// within `limit`, so a trickling connection cannot hold Discord's start.
#[expect(clippy::too_many_lines, reason = "one WinHTTP request, opened to closed")]
fn get(
   url: &str,
   limit: Duration,
   mut sink: impl FnMut(&[u8]) -> misstep::Result<()>,
) -> misstep::Result<()> {
   let deadline = Instant::now() + limit;
   let target = Target::parse(url)?;
   let ms = |time: Duration| i32::try_from(time.as_millis()).unwrap_or(i32::MAX);

   let agent = wide("Sunbird");
   // SAFETY: `agent` is NUL-terminated.
   let session = Handle::new(
      unsafe {
         WinHttpOpen(
            agent.as_ptr(),
            ACCESS_TYPE_AUTOMATIC_PROXY,
            ptr::null(),
            ptr::null(),
            0,
         )
      },
      "WinHttpOpen",
   )?;
   // SAFETY: a live session.
   check(
      unsafe {
         WinHttpSetTimeouts(
            session.0,
            ms(limit),
            ms(limit),
            ms(limit),
            ms(limit.min(STALL)),
         )
      },
      "WinHttpSetTimeouts",
   )?;
   let fast_fallback = 1u32;
   // Older Windows rejects the option, so the result is ignored.
   // SAFETY: a live session and a 4-byte BOOL.
   unsafe {
      WinHttpSetOption(
         session.0,
         OPTION_IPV6_FAST_FALLBACK,
         (&raw const fast_fallback).cast(),
         4,
      );
   }
   let host = wide(target.host);
   // SAFETY: a live session and a NUL-terminated host.
   let connect = Handle::new(
      unsafe { WinHttpConnect(session.0, host.as_ptr(), target.port, 0) },
      "WinHttpConnect",
   )?;
   let (verb, path) = (wide("GET"), wide(target.path));
   // SAFETY: a live connection and NUL-terminated strings.
   let request = Handle::new(
      unsafe {
         WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            if target.secure { FLAG_SECURE } else { 0 },
         )
      },
      "WinHttpOpenRequest",
   )?;
   // SAFETY: a live request with no headers or body.
   check(
      unsafe { WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) },
      "WinHttpSendRequest",
   )?;
   // SAFETY: the request was sent.
   check(
      unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) },
      "WinHttpReceiveResponse",
   )?;

   let mut status = 0u32;
   let mut len = 4u32;
   // SAFETY: `status` holds the `len` bytes the numeric query writes.
   check(
      unsafe {
         WinHttpQueryHeaders(
            request.0,
            QUERY_STATUS_CODE_NUMBER,
            ptr::null(),
            (&raw mut status).cast(),
            &raw mut len,
            ptr::null_mut(),
         )
      },
      "WinHttpQueryHeaders",
   )?;
   if !(200..300).contains(&status) {
      misstep::bail!("{url}: HTTP {status}");
   }

   let mut buf = vec![0u8; 64 << 10];
   loop {
      if Instant::now() > deadline {
         misstep::bail!("{url} took over {} s", limit.as_secs());
      }
      let mut read = 0u32;
      // SAFETY: `buf` holds the `len` bytes asked for.
      check(
         unsafe {
            WinHttpReadData(
               request.0,
               buf.as_mut_ptr().cast(),
               u32::try_from(buf.len()).unwrap_or(u32::MAX),
               &raw mut read,
            )
         },
         "WinHttpReadData",
      )?;
      if read == 0 {
         return Ok(());
      }
      sink(&buf[..read as usize])?;
   }
}
