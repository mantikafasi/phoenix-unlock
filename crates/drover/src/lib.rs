//! Discord Drover's voice unblock: two one-byte UDP packets to the voice
//! server, then a 50 ms pause, before each IP discovery request. Some networks
//! (the UAE's, for one) block voice by its first packet, and this gets it
//! through without a proxy. On when `SUNBIRD_DROVER` is set.
//!
//! Voice may run in Discord's process or in `discord_media`, so both load
//! this library: on Linux through the `LD_PRELOAD` the client keeps, on
//! Windows through `CreateProcessW`. The loader calls [`init`] on Linux and
//! [`attach`] on Windows.

#![cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]

use std::{
   sync::atomic::{
      AtomicBool,
      Ordering,
   },
   time::Duration,
};

static ON: AtomicBool = AtomicBool::new(false);

/// Reads `SUNBIRD_DROVER` once, before any thread sends.
pub fn init() -> bool {
   let on = std::env::var_os("SUNBIRD_DROVER").is_some();
   ON.store(on, Ordering::Relaxed);
   on
}

pub fn enabled() -> bool {
   ON.load(Ordering::Relaxed)
}

/// Discord's IP discovery request: type 1 and length 70, then the SSRC, a
/// 64-byte address and the port.
fn is_ip_discovery(head: &[u8], len: usize) -> bool {
   enabled() && len == 74 && head.starts_with(&[0, 1, 0, 70])
}

/// Sends the two probes with `send`, the caller's own socket and address.
fn probe(mut send: impl FnMut(&u8)) {
   send(&0);
   send(&1);
   std::thread::sleep(Duration::from_millis(50));
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux {
   //! `LD_PRELOAD` puts these ahead of libc's for the whole process.

   use std::{
      ffi::{
         CStr,
         c_int,
         c_void,
      },
      ptr,
      sync::OnceLock,
   };

   use libc::{
      msghdr,
      sockaddr,
      socklen_t,
   };

   type SendTo =
      unsafe extern "C" fn(c_int, *const c_void, usize, c_int, *const sockaddr, socklen_t) -> isize;
   type Send = unsafe extern "C" fn(c_int, *const c_void, usize, c_int) -> isize;
   type SendMsg = unsafe extern "C" fn(c_int, *const msghdr, c_int) -> isize;

   fn next(name: &CStr) -> Option<*mut c_void> {
      // SAFETY: a NUL-terminated name looked up past this library.
      let found = unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr()) };
      (!found.is_null()).then_some(found)
   }

   fn real_sendto() -> Option<SendTo> {
      static REAL: OnceLock<Option<usize>> = OnceLock::new();
      let addr = (*REAL.get_or_init(|| next(c"sendto").map(|found| found as usize)))?;
      // SAFETY: libc's `sendto` has this signature.
      Some(unsafe { std::mem::transmute::<usize, SendTo>(addr) })
   }

   fn real_send() -> Option<Send> {
      static REAL: OnceLock<Option<usize>> = OnceLock::new();
      let addr = (*REAL.get_or_init(|| next(c"send").map(|found| found as usize)))?;
      // SAFETY: libc's `send` has this signature.
      Some(unsafe { std::mem::transmute::<usize, Send>(addr) })
   }

   fn real_sendmsg() -> Option<SendMsg> {
      static REAL: OnceLock<Option<usize>> = OnceLock::new();
      let addr = (*REAL.get_or_init(|| next(c"sendmsg").map(|found| found as usize)))?;
      // SAFETY: libc's `sendmsg` has this signature.
      Some(unsafe { std::mem::transmute::<usize, SendMsg>(addr) })
   }

   fn missing() -> isize {
      // SAFETY: errno is this thread's.
      unsafe { *libc::__errno_location() = libc::ENOSYS };
      -1
   }

   /// The first four bytes of `len` at `buf`, or fewer.
   ///
   /// # Safety
   ///
   /// `buf` holds `len` readable bytes, or is null.
   unsafe fn head<'a>(buf: *const c_void, len: usize) -> &'a [u8] {
      if buf.is_null() {
         return &[];
      }
      // SAFETY: forwarded from the caller.
      unsafe { std::slice::from_raw_parts(buf.cast(), len.min(4)) }
   }

   fn probe(real: SendTo, fd: c_int, flags: c_int, addr: *const sockaddr, len: socklen_t) {
      // A stream that happens to carry the same 74 bytes must not get the
      // probes spliced in.
      let mut kind: c_int = 0;
      let mut size = socklen_t::try_from(size_of::<c_int>()).unwrap_or(0);
      // SAFETY: `kind` and `size` are valid for the call.
      let failed = unsafe {
         libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut size,
         )
      };
      if failed != 0 || kind != libc::SOCK_DGRAM {
         return;
      }
      // A null address sends on a connected socket, as `send` would.
      // SAFETY: one readable byte, and the caller's own socket and address.
      super::probe(|byte| unsafe {
         real(fd, ptr::from_ref(byte).cast(), 1, flags, addr, len);
      });
   }

   /// # Safety
   ///
   /// As for libc's `sendto`.
   #[unsafe(no_mangle)]
   pub unsafe extern "C" fn sendto(
      fd: c_int,
      buf: *const c_void,
      len: usize,
      flags: c_int,
      addr: *const sockaddr,
      addr_len: socklen_t,
   ) -> isize {
      let Some(real) = real_sendto() else {
         return missing();
      };
      // SAFETY: the caller passes `len` readable bytes.
      if super::is_ip_discovery(unsafe { head(buf, len) }, len) {
         probe(real, fd, flags, addr, addr_len);
      }
      // SAFETY: forwarded from the caller.
      unsafe { real(fd, buf, len, flags, addr, addr_len) }
   }

   /// # Safety
   ///
   /// As for libc's `send`.
   #[unsafe(no_mangle)]
   pub unsafe extern "C" fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize {
      let Some(real) = real_send() else {
         return missing();
      };
      // SAFETY: the caller passes `len` readable bytes.
      if super::is_ip_discovery(unsafe { head(buf, len) }, len)
         && let Some(sendto) = real_sendto()
      {
         probe(sendto, fd, flags, ptr::null(), 0);
      }
      // SAFETY: forwarded from the caller.
      unsafe { real(fd, buf, len, flags) }
   }

   /// # Safety
   ///
   /// As for libc's `sendmsg`.
   #[unsafe(no_mangle)]
   pub unsafe extern "C" fn sendmsg(fd: c_int, msg: *const msghdr, flags: c_int) -> isize {
      let Some(real) = real_sendmsg() else {
         return missing();
      };
      if super::enabled()
         // SAFETY: the caller passes a valid `msghdr` or null.
         && let Some(header) = unsafe { msg.as_ref() }
         && !header.msg_iov.is_null()
         && let Some(sendto) = real_sendto()
      {
         // SAFETY: `msg_iov` holds `msg_iovlen` entries.
         let iovs = unsafe { std::slice::from_raw_parts(header.msg_iov, header.msg_iovlen) };
         let len = iovs.iter().map(|iov| iov.iov_len).sum();
         // shortcut: reads the header from the first iovec only, the way every
         // sender seen builds a datagram this small.
         // SAFETY: each iovec points at `iov_len` readable bytes.
         let first = iovs
            .first()
            .map_or(&[][..], |iov| unsafe { head(iov.iov_base, iov.iov_len) });
         if super::is_ip_discovery(first, len) {
            probe(
               sendto,
               fd,
               flags,
               header.msg_name.cast(),
               header.msg_namelen,
            );
         }
      }
      // SAFETY: forwarded from the caller.
      unsafe { real(fd, msg, flags) }
   }
}

#[cfg(windows)]
pub use windows::{
   ProcessInformation,
   announce,
   attach,
   is_media,
   launched_with,
   queue_load,
};

#[cfg(windows)]
mod windows {
   //! Swaps `ws2_32`'s send functions in the executable's imports. Discord.exe
   //! also gets `CreateProcessW` swapped, so `discord_media.exe` starts with
   //! this DLL loaded, the way `sunbird.exe` starts Discord.

   use std::{
      ffi::c_void,
      io,
      path::Path,
      ptr,
      sync::{
         OnceLock,
         atomic::{
            AtomicUsize,
            Ordering,
         },
      },
   };

   use phoenix_hook::image::Image;

   type Handle = *mut c_void;

   type SendTo = unsafe extern "system" fn(usize, *const u8, i32, i32, *const c_void, i32) -> i32;
   type Send = unsafe extern "system" fn(usize, *const u8, i32, i32) -> i32;
   type WsaSendTo = unsafe extern "system" fn(
      usize,
      *const WsaBuf,
      u32,
      *mut u32,
      u32,
      *const c_void,
      i32,
      *mut c_void,
      *mut c_void,
   ) -> i32;
   type CreateProcess = unsafe extern "system" fn(
      *const u16,
      *mut u16,
      *const c_void,
      *const c_void,
      i32,
      u32,
      *const c_void,
      *const u16,
      *const c_void,
      *mut ProcessInformation,
   ) -> i32;

   #[repr(C)]
   struct WsaBuf {
      len: u32,
      buf: *const u8,
   }

   /// `PROCESS_INFORMATION`, which `CreateProcessW` fills.
   #[repr(C)]
   pub struct ProcessInformation {
      pub process:    Handle,
      pub thread:     Handle,
      pub process_id: u32,
      pub thread_id:  u32,
   }

   const CREATE_SUSPENDED: u32 = 0x4;
   const MEM_COMMIT_RESERVE: u32 = 0x3000;
   const PAGE_READWRITE: u32 = 0x4;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn GetModuleHandleW(name: *const u16) -> Handle;
      fn GetModuleFileNameW(module: Handle, name: *mut u16, size: u32) -> u32;
      fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
      fn VirtualAllocEx(
         process: Handle,
         address: *const c_void,
         size: usize,
         kind: u32,
         protect: u32,
      ) -> *mut c_void;
      fn WriteProcessMemory(
         process: Handle,
         address: *mut c_void,
         buffer: *const c_void,
         size: usize,
         written: *mut usize,
      ) -> i32;
      fn QueueUserAPC(function: *mut c_void, thread: Handle, data: usize) -> u32;
      fn ResumeThread(thread: Handle) -> u32;
      fn CreateEventW(
         attributes: *const c_void,
         manual_reset: i32,
         initial: i32,
         name: *const u16,
      ) -> Handle;
      fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
      fn CloseHandle(handle: Handle) -> i32;
   }

   /// The named event a Discord started with these launcher flags holds.
   fn launch_event(gate_only: bool, drover: bool) -> Vec<u16> {
      wide(&format!(
         "Local\\Sunbird-gate{}-drover{}",
         u8::from(gate_only),
         u8::from(drover)
      ))
   }

   /// Marks this Discord as started with `gate_only` and drover as [`attach`]
   /// read it, for [`launched_with`]. The event lives as long as Discord.
   pub fn announce(gate_only: bool) {
      // SAFETY: a NUL-terminated name, and the handle is kept open on purpose.
      unsafe {
         CreateEventW(
            ptr::null(),
            1,
            0,
            launch_event(gate_only, super::enabled()).as_ptr(),
         )
      };
   }

   /// Whether a running Discord announced these launcher flags, so the
   /// launcher only restarts it when they changed.
   #[must_use]
   pub fn launched_with(gate_only: bool, drover: bool) -> bool {
      const SYNCHRONIZE: u32 = 0x0010_0000;
      // SAFETY: a NUL-terminated name.
      let event = unsafe { OpenEventW(SYNCHRONIZE, 0, launch_event(gate_only, drover).as_ptr()) };
      if event.is_null() {
         return false;
      }
      // SAFETY: opened above and closed once.
      unsafe { CloseHandle(event) };
      true
   }

   #[link(name = "ws2_32")]
   unsafe extern "system" {
      fn getsockopt(socket: usize, level: i32, name: i32, value: *mut u8, size: *mut i32) -> i32;
   }

   const SOL_SOCKET: i32 = 0xFFFF;
   const SO_TYPE: i32 = 0x1008;
   const SOCK_DGRAM: i32 = 2;

   static SENDTO: AtomicUsize = AtomicUsize::new(0);
   static SEND: AtomicUsize = AtomicUsize::new(0);
   static WSA_SENDTO: AtomicUsize = AtomicUsize::new(0);
   static CREATE_PROCESS: AtomicUsize = AtomicUsize::new(0);
   /// `ws2_32`'s `sendto`, which sends the probes whichever function is hooked.
   static PROBE: AtomicUsize = AtomicUsize::new(0);
   /// This DLL's path, NUL-terminated, for `discord_media.exe` to load.
   static OURS: OnceLock<Vec<u16>> = OnceLock::new();

   fn wide(text: &str) -> Vec<u16> {
      text.encode_utf16().chain([0]).collect()
   }

   /// Whether this process is Discord's media process, which gets only these
   /// hooks.
   #[must_use]
   pub fn is_media() -> bool {
      std::env::current_exe().is_ok_and(|exe| named_media(&exe.to_string_lossy()))
   }

   fn named_media(path: &str) -> bool {
      Path::new(path)
         .file_stem()
         .is_some_and(|stem| stem.eq_ignore_ascii_case("discord_media"))
   }

   /// Hooks the executable's sends, and in Discord.exe its `CreateProcessW`,
   /// from `DllMain`.
   ///
   /// # Safety
   ///
   /// `module` is this DLL's `HINSTANCE`.
   pub unsafe fn attach(module: Handle) {
      if !super::init() {
         return;
      }
      // SAFETY: modules stay loaded, and this runs before the host's own code.
      let Some(exe) = (unsafe { Image::loaded() }).into_iter().next() else {
         log::warn!("drover: no executable image");
         return;
      };
      let ws2 = resolve("ws2_32.dll");
      if !ws2.is_null() {
         // SAFETY: a loaded module and a NUL-terminated name.
         let found = unsafe { GetProcAddress(ws2, c"sendto".as_ptr().cast()) };
         PROBE.store(found as usize, Ordering::Release);
      }
      swap(&exe, ws2, c"sendto", sendto as SendTo as usize, &SENDTO);
      swap(&exe, ws2, c"send", send as Send as usize, &SEND);
      swap(
         &exe,
         ws2,
         c"WSASendTo",
         wsa_sendto as WsaSendTo as usize,
         &WSA_SENDTO,
      );
      if is_media() {
         return;
      }
      let mut path = vec![0u16; 1024];
      // SAFETY: `path` holds 1024 units, and the caller passes this DLL.
      let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), 1024) } as usize;
      if len == 0 || len >= path.len() {
         log::warn!("drover: this DLL's path is unknown, so discord_media starts without it");
         return;
      }
      path.truncate(len + 1);
      let _ = OURS.set(path);
      // An import through an API set lands on kernelbase's copy, not
      // kernel32's.
      for dll in ["kernelbase.dll", "kernel32.dll"] {
         swap(
            &exe,
            resolve(dll),
            c"CreateProcessW",
            create_process as CreateProcess as usize,
            &CREATE_PROCESS,
         );
      }
   }

   fn resolve(dll: &str) -> Handle {
      // SAFETY: a NUL-terminated name.
      unsafe { GetModuleHandleW(wide(dll).as_ptr()) }
   }

   /// Points every import slot of `name` in `exe` at `detour`, keeping the
   /// real function in `real`.
   fn swap(
      exe: &Image<'static>,
      dll: Handle,
      name: &std::ffi::CStr,
      detour: usize,
      real: &AtomicUsize,
   ) {
      if dll.is_null() {
         log::warn!("drover: {name:?}'s DLL is not loaded");
         return;
      }
      // SAFETY: a loaded module and a NUL-terminated name.
      let found = unsafe { GetProcAddress(dll, name.as_ptr().cast()) } as usize;
      if found == 0 {
         return;
      }
      let slots: Vec<_> = exe
         .pointers_to(found)
         .into_iter()
         .map(|slot| (slot, detour))
         .collect();
      if slots.is_empty() {
         return;
      }
      real.store(found, Ordering::Release);
      // SAFETY: import slots are pointer-aligned words of `exe`, and a reader
      // calls either value with the same arguments.
      match unsafe { exe.swap_words(&slots) } {
         Ok(_) => log::info!("drover: hooked {name:?} in {} slots", slots.len()),
         Err(err) => log::warn!("drover: {name:?}: {err}"),
      }
   }

   fn head<'a>(buf: *const u8, len: i32) -> &'a [u8] {
      match usize::try_from(len) {
         // SAFETY: the caller of the hooked function passes `len` readable
         // bytes.
         Ok(len) if !buf.is_null() => unsafe { std::slice::from_raw_parts(buf, len.min(4)) },
         _ => &[],
      }
   }

   fn probe(socket: usize, flags: i32, to: *const c_void, to_len: i32) {
      let real = PROBE.load(Ordering::Acquire);
      // A stream that happens to carry the same 74 bytes must not get the
      // probes spliced in.
      let mut kind = 0i32;
      let mut size = 4i32;
      // SAFETY: `kind` and `size` are valid for the call.
      let failed = unsafe {
         getsockopt(
            socket,
            SOL_SOCKET,
            SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut size,
         )
      };
      if real == 0 || failed != 0 || kind != SOCK_DGRAM {
         return;
      }
      // SAFETY: ws2_32's `sendto`.
      let real: SendTo = unsafe { std::mem::transmute(real) };
      // SAFETY: one readable byte, and the caller's own socket and address.
      super::probe(|byte| unsafe {
         real(socket, ptr::from_ref(byte), 1, flags, to, to_len);
      });
   }

   unsafe extern "system" fn sendto(
      socket: usize,
      buf: *const u8,
      len: i32,
      flags: i32,
      to: *const c_void,
      to_len: i32,
   ) -> i32 {
      if super::is_ip_discovery(head(buf, len), usize::try_from(len).unwrap_or(0)) {
         probe(socket, flags, to, to_len);
      }
      // SAFETY: set before this detour was swapped in.
      let real: SendTo = unsafe { std::mem::transmute(SENDTO.load(Ordering::Acquire)) };
      // SAFETY: forwarded from the caller.
      unsafe { real(socket, buf, len, flags, to, to_len) }
   }

   unsafe extern "system" fn send(socket: usize, buf: *const u8, len: i32, flags: i32) -> i32 {
      if super::is_ip_discovery(head(buf, len), usize::try_from(len).unwrap_or(0)) {
         probe(socket, flags, ptr::null(), 0);
      }
      // SAFETY: set before this detour was swapped in.
      let real: Send = unsafe { std::mem::transmute(SEND.load(Ordering::Acquire)) };
      // SAFETY: forwarded from the caller.
      unsafe { real(socket, buf, len, flags) }
   }

   unsafe extern "system" fn wsa_sendto(
      socket: usize,
      buffers: *const WsaBuf,
      count: u32,
      sent: *mut u32,
      flags: u32,
      to: *const c_void,
      to_len: i32,
      overlapped: *mut c_void,
      routine: *mut c_void,
   ) -> i32 {
      // SAFETY: the caller passes `count` buffers.
      if count == 1
         && let Some(only) = unsafe { buffers.as_ref() }
      {
         let len = i32::try_from(only.len).unwrap_or(0);
         if super::is_ip_discovery(head(only.buf, len), only.len as usize) {
            probe(socket, 0, to, to_len);
         }
      }
      // SAFETY: set before this detour was swapped in.
      let real: WsaSendTo = unsafe { std::mem::transmute(WSA_SENDTO.load(Ordering::Acquire)) };
      // SAFETY: forwarded from the caller.
      unsafe {
         real(
            socket, buffers, count, sent, flags, to, to_len, overlapped, routine,
         )
      }
   }

   /// The program `CreateProcessW` starts: its application name, or the
   /// command line's first word.
   fn program(app: *const u16, command: *const u16) -> String {
      let text = |at: *const u16| {
         if at.is_null() {
            return String::new();
         }
         let mut len = 0;
         // SAFETY: CreateProcessW's strings are NUL-terminated.
         while unsafe { *at.add(len) } != 0 {
            len += 1;
         }
         // SAFETY: `len` units precede the NUL.
         String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(at, len) })
      };
      let app = text(app);
      if !app.is_empty() {
         return app;
      }
      let command = text(command);
      let command = command.trim_start();
      command
         .strip_prefix('"')
         .map_or_else(
            || command.split(' ').next(),
            |quoted| quoted.split('"').next(),
         )
         .unwrap_or_default()
         .to_owned()
   }

   unsafe extern "system" fn create_process(
      app: *const u16,
      command: *mut u16,
      process_attributes: *const c_void,
      thread_attributes: *const c_void,
      inherit: i32,
      flags: u32,
      environment: *const c_void,
      directory: *const u16,
      startup: *const c_void,
      info: *mut ProcessInformation,
   ) -> i32 {
      // SAFETY: set before this detour was swapped in.
      let real: CreateProcess =
         unsafe { std::mem::transmute(CREATE_PROCESS.load(Ordering::Acquire)) };
      let media = named_media(&program(app, command)) && OURS.get().is_some() && !info.is_null();
      let suspend = if media { CREATE_SUSPENDED } else { 0 };
      // SAFETY: forwarded from the caller.
      let ok = unsafe {
         real(
            app,
            command,
            process_attributes,
            thread_attributes,
            inherit,
            flags | suspend,
            environment,
            directory,
            startup,
            info,
         )
      };
      if ok != 0 && media {
         // SAFETY: CreateProcessW filled `info` for the suspended child.
         let info = unsafe { &*info };
         match queue_load(info, OURS.get().map_or(&[][..], Vec::as_slice)) {
            Ok(()) => {
               log::info!(
                  "drover: loading into discord_media (pid {})",
                  info.process_id
               );
            },
            Err(err) => log::warn!("drover: discord_media starts without the voice unblock: {err}"),
         }
         if flags & CREATE_SUSPENDED == 0 {
            // SAFETY: the child's main thread, suspended above.
            unsafe { ResumeThread(info.thread) };
         }
      }
      ok
   }

   fn check(ok: bool) -> io::Result<()> {
      if ok {
         Ok(())
      } else {
         Err(io::Error::last_os_error())
      }
   }

   /// Loads `dll`, a NUL-terminated path, into a suspended child.
   ///
   /// Copies the path in and queues `LoadLibraryW` on the child's main
   /// thread, which runs once the OS loader finishes. `sunbird.exe` starts
   /// Discord this way too. kernel32 sits at the same address in every
   /// process of a boot.
   ///
   /// # Errors
   ///
   /// The OS error of the step that failed.
   pub fn queue_load(info: &ProcessInformation, dll: &[u16]) -> io::Result<()> {
      let size = size_of_val(dll);
      // SAFETY: `info.process` is a live process handle with full access.
      let remote = unsafe {
         VirtualAllocEx(
            info.process,
            ptr::null(),
            size,
            MEM_COMMIT_RESERVE,
            PAGE_READWRITE,
         )
      };
      check(!remote.is_null())?;
      // SAFETY: `remote` holds `size` bytes and `dll` is `size` bytes long.
      check(unsafe {
         WriteProcessMemory(
            info.process,
            remote,
            dll.as_ptr().cast(),
            size,
            ptr::null_mut(),
         ) != 0
      })?;
      // SAFETY: kernel32 is always loaded and the name is NUL-terminated.
      let load =
         unsafe { GetProcAddress(resolve("kernel32.dll"), c"LoadLibraryW".as_ptr().cast()) };
      check(!load.is_null())?;
      // SAFETY: LoadLibraryW takes one pointer argument, as an APC does.
      check(unsafe { QueueUserAPC(load, info.thread, remote as usize) } != 0)
   }

   #[cfg(test)]
   mod tests {
      #[test]
      fn finds_the_program() {
         use super::wide;
         let command = wide(r#""C:\Discord\discord_media.exe" --type=media"#);
         assert!(super::named_media(&super::program(
            std::ptr::null(),
            command.as_ptr()
         )));
         let app = wide(r"C:\Discord\Discord.exe");
         assert!(!super::named_media(&super::program(
            app.as_ptr(),
            command.as_ptr()
         )));
      }
   }
}

#[cfg(test)]
mod tests {
   #[test]
   fn matches_only_ip_discovery() {
      super::ON.store(true, std::sync::atomic::Ordering::Relaxed);
      assert!(super::is_ip_discovery(&[0, 1, 0, 70], 74));
      assert!(!super::is_ip_discovery(&[0, 1, 0, 70], 75));
      assert!(!super::is_ip_discovery(&[0x80, 0x78, 0, 1], 74));
      assert!(!super::is_ip_discovery(&[0, 1], 74));
   }
}
