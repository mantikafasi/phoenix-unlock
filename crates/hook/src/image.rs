use std::{
   borrow::Cow,
   fmt,
   path::Path,
};
#[cfg(unix)]
use std::{
   ffi::{
      CStr,
      c_int,
      c_void,
   },
   slice,
   sync::atomic::{
      AtomicUsize,
      Ordering,
   },
};
#[cfg(windows)]
use std::{
   ffi::{
      OsString,
      c_void,
   },
   os::windows::ffi::OsStringExt,
   slice,
   sync::atomic::{
      AtomicUsize,
      Ordering,
   },
};

use memchr::memmem;

use crate::pattern::Pattern;

/// Held across each protect, swap and restore, so a second writer on the same
/// page never records the first one's temporary protection as the page's own.
#[cfg(any(unix, windows))]
static PROTECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `jmp [rip]` followed by its 8-byte target.
#[cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]
pub(crate) const ABSOLUTE_JUMP_LEN: usize = 14;

#[cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]
pub(crate) fn absolute_jump(to: usize) -> [u8; ABSOLUTE_JUMP_LEN] {
   let mut jump = [0; ABSOLUTE_JUMP_LEN];
   jump[..2].copy_from_slice(&[0xFF, 0x25]);
   jump[6..].copy_from_slice(&to.to_le_bytes());
   jump
}

/// `len` fresh read-write-execute bytes at `at`, or anywhere when `at` is 0.
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) fn alloc_exec(at: usize, len: usize) -> Option<usize> {
   // SAFETY: asks for a fresh mapping at a free address, or fails.
   let page = unsafe {
      win32::VirtualAlloc(
         at as *const c_void,
         len,
         win32::MEM_COMMIT_RESERVE,
         win32::PAGE_EXECUTE_READWRITE,
      )
   };
   (!page.is_null()).then_some(page as usize)
}

/// `len` fresh read-write-execute bytes, near `at` when the kernel can.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn alloc_exec(at: usize, len: usize) -> Option<usize> {
   // SAFETY: asks for a fresh anonymous mapping that replaces nothing.
   let page = unsafe {
      libc::mmap(
         at as *mut c_void,
         len,
         libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
         libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
         -1,
         0,
      )
   };
   (page != libc::MAP_FAILED).then_some(page as usize)
}

const NT_GNU_BUILD_ID: u32 = 3;
#[cfg(all(unix, not(target_vendor = "apple")))]
const PF_X: u32 = 1;
#[cfg(all(unix, not(target_vendor = "apple")))]
const PF_W: u32 = 2;
#[cfg(all(unix, not(target_vendor = "apple")))]
const PF_R: u32 = 4;
#[cfg(all(unix, not(target_vendor = "apple")))]
const PT_ARM_EXIDX: u32 = 0x7000_0001;

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
   fn _dyld_image_count() -> u32;
   fn _dyld_get_image_header(index: u32) -> *const c_void;
   fn _dyld_get_image_name(index: u32) -> *const std::ffi::c_char;
   fn _dyld_get_image_slide(header: *const c_void) -> isize;
}

/// The `NT_GNU_BUILD_ID` note of an ELF module, 1 to 32 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuildId {
   len:   u8,
   bytes: [u8; 32],
}

impl BuildId {
   #[must_use]
   pub fn from_bytes(id: &[u8]) -> Option<Self> {
      if id.is_empty() || id.len() > 32 {
         return None;
      }
      let mut bytes = [0; 32];
      bytes[..id.len()].copy_from_slice(id);
      Some(Self {
         len: u8::try_from(id.len()).ok()?,
         bytes,
      })
   }

   #[must_use]
   pub fn as_bytes(&self) -> &[u8] {
      &self.bytes[..usize::from(self.len)]
   }

   /// Reads the GNU build ID out of the contents of a `PT_NOTE` segment.
   #[must_use]
   pub fn from_notes(mut notes: &[u8]) -> Option<Self> {
      let word = |bytes: &[u8], at: usize| -> Option<u32> {
         Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
      };
      let align4 = |len: u32| (len as usize).next_multiple_of(4);
      while notes.len() >= 12 {
         let (name_len, desc_len, kind) = (word(notes, 0)?, word(notes, 4)?, word(notes, 8)?);
         let name = notes.get(12..12 + name_len as usize)?;
         let desc_at = 12 + align4(name_len);
         let desc = notes.get(desc_at..desc_at + desc_len as usize)?;
         if kind == NT_GNU_BUILD_ID && name == b"GNU\0" {
            return Self::from_bytes(desc);
         }
         notes = notes.get(desc_at + align4(desc_len)..)?;
      }
      None
   }
}

impl fmt::Display for BuildId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self
         .as_bytes()
         .iter()
         .try_for_each(|byte| write!(formatter, "{byte:02x}"))
   }
}

impl fmt::Debug for BuildId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(formatter, "BuildId({self})")
   }
}

/// One readable `PT_LOAD` segment at its runtime address.
pub struct Segment<'a> {
   pub addr:       usize,
   pub bytes:      Cow<'a, [u8]>,
   pub executable: bool,
   pub writable:   bool,
}

impl Segment<'_> {
   #[must_use]
   pub fn contains(&self, addr: usize) -> bool {
      (self.addr..self.addr + self.bytes.len()).contains(&addr)
   }
}

/// A loaded or offline binary module (ELF or PE), mapped with its relative
/// relocations applied so pointer slots hold the addresses code would see.
pub struct Image<'a> {
   pub path:          String,
   pub build_id:      Option<BuildId>,
   pub segments:      Vec<Segment<'a>>,
   /// Address of `.eh_frame_hdr`, from `PT_GNU_EH_FRAME`.
   pub eh_frame_hdr:  Option<usize>,
   /// `.ARM.exidx` address and byte length, from `PT_ARM_EXIDX`. ARM32 unwinds
   /// through this table, so it carries the function starts `.eh_frame_hdr`
   /// holds on other targets.
   pub arm_exidx:     Option<(usize, usize)>,
   /// PE64 exception directory `(image_base, pdata_addr, pdata_len)` holding
   /// sorted 12-byte `RUNTIME_FUNCTION` `{begin_rva, end_rva, unwind_rva}`
   /// entries.
   pub pdata:         Option<(usize, usize, usize)>,
   /// Width of a pointer in this image, 4 for 32-bit and 8 for 64-bit. A file
   /// parsed offline keeps the target's width, not the host's.
   pub pointer_width: usize,
}

#[cfg(target_vendor = "apple")]
impl Image<'static> {
   /// Every image dyld has loaded. `__TEXT` is split into its sections, since
   /// it maps strings and constants executable beside the code, and
   /// `__DATA_CONST`, which dyld makes read-only after fixups, reads as not
   /// writable.
   ///
   /// # Safety
   ///
   /// The segments borrow mapped memory, which dyld keeps mapped while an image
   /// is loaded.
   #[must_use]
   pub unsafe fn loaded() -> Vec<Self> {
      const MH_MAGIC_64: u32 = 0xFEED_FACF;
      const LC_SEGMENT_64: u32 = 0x19;
      const LC_UUID: u32 = 0x1B;
      const SEGMENT_COMMAND_SIZE: usize = 72;
      const SECTION_SIZE: usize = 80;
      const INSTRUCTIONS: u32 = 0x8000_0400;
      const READ_ONLY_AFTER_FIXUPS: [&[u8]; 2] = [b"__DATA_CONST", b"__AUTH_CONST"];

      // SAFETY: every read below is inside the header and load commands of an
      // image dyld reported, which stay mapped.
      let u32_at = |at: usize| unsafe { std::ptr::read_unaligned(at as *const u32) };
      let word_at = |at: usize| unsafe { std::ptr::read_unaligned(at as *const usize) };
      let name_at = |at: usize| {
         // SAFETY: as above, a 16-byte name field.
         let name = unsafe { slice::from_raw_parts(at as *const u8, 16) };
         &name[..name.iter().position(|&byte| byte == 0).unwrap_or(16)]
      };

      // SAFETY: a plain query.
      let count = unsafe { _dyld_image_count() };
      (0..count)
         .filter_map(|index| {
            // SAFETY: index is below the count dyld reported.
            let header = unsafe { _dyld_get_image_header(index) };
            if header.is_null() || u32_at(header as usize) != MH_MAGIC_64 {
               return None;
            }
            // SAFETY: a header dyld reported.
            let slide = unsafe { _dyld_get_image_slide(header) };
            // SAFETY: index is below the count dyld reported.
            let name = unsafe { _dyld_get_image_name(index) };
            let path = if name.is_null() {
               String::new()
            } else {
               // SAFETY: dyld returns a NUL-terminated path.
               unsafe { CStr::from_ptr(name) }
                  .to_string_lossy()
                  .into_owned()
            };
            let mut build_id = None;
            let mut segments = Vec::new();
            let mut push = |addr: usize, len: usize, executable: bool, writable: bool| {
               let addr = addr.wrapping_add_signed(slide);
               // SAFETY: dyld maps the segment for the image's lifetime.
               let bytes = unsafe { slice::from_raw_parts(addr as *const u8, len) };
               segments.push(Segment {
                  addr,
                  bytes: Cow::Borrowed(bytes),
                  executable,
                  writable,
               });
            };

            let base = header as usize;
            let mut command = base + 32;
            for _ in 0..u32_at(base + 16) {
               let kind = u32_at(command);
               if kind == LC_SEGMENT_64 {
                  let (vmaddr, vmsize) = (word_at(command + 24), word_at(command + 32));
                  let initprot = u32_at(command + 60).cast_signed();
                  let readable = vmsize != 0 && initprot & libc::PROT_READ != 0;
                  if readable && initprot & libc::PROT_EXEC != 0 {
                     for at in 0..u32_at(command + 64) as usize {
                        let section = command + SEGMENT_COMMAND_SIZE + at * SECTION_SIZE;
                        let flags = u32_at(section + 64);
                        let (addr, size) = (word_at(section + 32), word_at(section + 40));
                        push(addr, size, flags & INSTRUCTIONS != 0, false);
                     }
                  } else if readable {
                     let writable = initprot & libc::PROT_WRITE != 0
                        && !READ_ONLY_AFTER_FIXUPS.contains(&name_at(command + 8));
                     push(vmaddr, vmsize, false, writable);
                  }
               } else if kind == LC_UUID && build_id.is_none() {
                  // SAFETY: LC_UUID carries a 16-byte UUID at offset 8.
                  let uuid = unsafe { slice::from_raw_parts((command + 8) as *const u8, 16) };
                  build_id = BuildId::from_bytes(uuid);
               }
               command += u32_at(command + 4) as usize;
            }
            Some(Image {
               path,
               build_id,
               segments,
               eh_frame_hdr: None,
               arm_exidx: None,
               pdata: None,
               pointer_width: size_of::<usize>(),
            })
         })
         .collect()
   }

   /// Atomically swaps each `(slot, value)` pair in this image and returns the
   /// values the slots held. Read-only pages are made writable for the write
   /// and get their protection back afterwards.
   ///
   /// # Safety
   ///
   /// Every slot is a pointer-aligned word in this image, and code that reads
   /// it accepts either value.
   pub unsafe fn swap_words(&self, writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
      // SAFETY: sysconf has no preconditions.
      let page_size =
         usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(16384);
      let mut pages = writes
         .iter()
         .map(|&(slot, _)| slot & !(page_size - 1))
         .collect::<Vec<usize>>();
      pages.sort_unstable();
      pages.dedup();
      let restore = pages
         .iter()
         .map(|&page| {
            region_protection(page)
               .map(|prot| (page, prot))
               .ok_or(crate::Error::Unmapped { addr: page })
         })
         .collect::<Result<Vec<_>, _>>()?;
      // SAFETY: forwarded from the caller, and `restore` holds each slot's
      // page.
      unsafe { swap_protected(writes, page_size, &restore) }
   }
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
   static mach_task_self_: u32;
   fn vm_region_64(
      task: u32,
      address: *mut usize,
      size: *mut usize,
      flavor: c_int,
      info: *mut c_int,
      count: *mut u32,
      object_name: *mut u32,
   ) -> c_int;
}

/// Protection of the mapping holding `addr`, from the kernel's region table.
#[cfg(target_vendor = "apple")]
fn region_protection(addr: usize) -> Option<c_int> {
   const VM_REGION_BASIC_INFO_64: c_int = 9;
   const VM_REGION_BASIC_INFO_COUNT_64: u32 = 9;

   let (mut start, mut size, mut object) = (addr, 0usize, 0u32);
   let mut info = [0 as c_int; VM_REGION_BASIC_INFO_COUNT_64 as usize];
   let mut count = VM_REGION_BASIC_INFO_COUNT_64;
   // SAFETY: `info` holds the ints this flavor writes, and the kernel fails
   // rather than faults on an unmapped address.
   let status = unsafe {
      vm_region_64(
         mach_task_self_,
         &raw mut start,
         &raw mut size,
         VM_REGION_BASIC_INFO_64,
         info.as_mut_ptr(),
         &raw mut count,
         &raw mut object,
      )
   };
   // The kernel returns the next region when nothing maps `addr`.
   (status == 0 && start <= addr && addr - start < size).then_some(info[0])
}

/// Swaps each `(slot, value)` pair with every page in `restore` made writable,
/// then gives each page its protection back. A page that stays writable after
/// a failed restore is left that way, since the swap has already landed.
///
/// # Safety
///
/// Every slot is a pointer-aligned word on one of the pages in `restore`, and
/// code that reads it accepts either value.
#[cfg(unix)]
unsafe fn swap_protected(
   writes: &[(usize, usize)],
   page_size: usize,
   restore: &[(usize, c_int)],
) -> Result<Vec<usize>, crate::Error> {
   let protect = |page: usize, prot: c_int| {
      // SAFETY: `page` is a mapped page the caller looked up.
      if unsafe { libc::mprotect(page as *mut c_void, page_size, prot) } == 0 {
         Ok(())
      } else {
         Err(crate::Error::Protect {
            addr:   page,
            source: std::io::Error::last_os_error(),
         })
      }
   };

   let _serial = PROTECT
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   for (done, &(page, prot)) in restore.iter().enumerate() {
      if let Err(err) = protect(page, prot | libc::PROT_WRITE) {
         for &(page, prot) in &restore[..done] {
            let _ = protect(page, prot);
         }
         return Err(err);
      }
   }
   let previous = writes
      .iter()
      // SAFETY: the caller guarantees aligned slots, now writable.
      .map(|&(slot, value)| {
         unsafe { AtomicUsize::from_ptr(slot as *mut usize) }.swap(value, Ordering::AcqRel)
      })
      .collect();
   for &(page, prot) in restore {
      let _ = protect(page, prot);
   }
   Ok(previous)
}

#[cfg(all(unix, not(target_vendor = "apple")))]
impl Image<'static> {
   /// Every module `dl_iterate_phdr` reports.
   ///
   /// # Safety
   ///
   /// The segments borrow mapped memory. The modules stay loaded while the
   /// images live, and no thread writes the writable segments during a scan.
   pub unsafe fn loaded() -> Vec<Self> {
      #[allow(
         clippy::cast_possible_truncation,
         reason = "ElfW(Addr) is pointer-sized, so these casts are lossless on every target"
      )]
      unsafe extern "C" fn visit(
         info: *mut libc::dl_phdr_info,
         _size: usize,
         out: *mut c_void,
      ) -> c_int {
         // SAFETY: the loader passes a valid info record and `out` is the Vec
         // below.
         let (info, images) = unsafe { (&*info, &mut *out.cast::<Vec<Image<'static>>>()) };
         let bias = info.dlpi_addr as usize;
         let headers = if info.dlpi_phdr.is_null() {
            &[][..]
         } else {
            // SAFETY: the loader reports dlpi_phnum headers at dlpi_phdr.
            unsafe { slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) }
         };
         let mut path = if info.dlpi_name.is_null() {
            String::new()
         } else {
            // SAFETY: dlpi_name is NUL-terminated when present.
            unsafe { CStr::from_ptr(info.dlpi_name) }
               .to_string_lossy()
               .into_owned()
         };
         // The loader reports the main executable first and without a name.
         if path.is_empty()
            && images.is_empty()
            && let Ok(exe) = std::fs::read_link("/proc/self/exe")
         {
            path = exe.to_string_lossy().into_owned();
         }

         let mut image = Image {
            path,
            build_id: None,
            segments: Vec::new(),
            eh_frame_hdr: None,
            arm_exidx: None,
            pdata: None,
            pointer_width: size_of::<usize>(),
         };
         for header in headers {
            let addr = bias + header.p_vaddr as usize;
            let len = header.p_memsz as usize;
            // SAFETY: PT_LOAD and PT_NOTE ranges are mapped for the module's
            // lifetime.
            let bytes = || unsafe { slice::from_raw_parts(addr as *const u8, len) };
            match header.p_type {
               libc::PT_LOAD if header.p_flags & PF_R != 0 => {
                  image.segments.push(Segment {
                     addr,
                     bytes: Cow::Borrowed(bytes()),
                     executable: header.p_flags & PF_X != 0,
                     writable: header.p_flags & PF_W != 0,
                  });
               },
               libc::PT_NOTE if image.build_id.is_none() => {
                  image.build_id = BuildId::from_notes(bytes());
               },
               libc::PT_GNU_EH_FRAME => image.eh_frame_hdr = Some(addr),
               PT_ARM_EXIDX => image.arm_exidx = Some((addr, len)),
               _ => {},
            }
         }
         images.push(image);
         0
      }

      let mut images = Vec::new();
      // SAFETY: `visit` only touches `images` through the pointer it is handed.
      unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut images).cast()) };
      images
   }
}

#[cfg(target_os = "linux")]
impl Image<'static> {
   /// Hands the pages a scan faulted in back to the kernel. A scan reads every
   /// page of the module, so without this the whole binary counts as resident
   /// until memory pressure reclaims it, not only what Discord itself runs.
   /// Patched pages are private copies: paging out may swap them, never drop
   /// them.
   pub fn release_scanned(&self) {
      // SAFETY: no preconditions.
      let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
      for segment in self.segments.iter().filter(|segment| !segment.writable) {
         let start = segment.addr & !(page - 1);
         let len = segment.addr + segment.bytes.len() - start;
         // SAFETY: the range is this module's own mapping, and paging out
         // never changes what a later read returns. A kernel without
         // `MADV_PAGEOUT` (before 5.4) fails the call and nothing changes.
         unsafe { libc::madvise(start as *mut c_void, len, libc::MADV_PAGEOUT) };
      }
   }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
impl Image<'static> {
   /// Writes each `(slot, value)` pair into this module's mapped memory and
   /// returns the values the slots held. Pages under `PT_GNU_RELRO` are made
   /// writable for the write and get their protection back afterwards.
   ///
   /// # Safety
   ///
   /// Every slot is a pointer-aligned word in this module, and no thread reads
   /// a slot while it is written.
   pub unsafe fn swap_words(&self, writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
      // SAFETY: forwarded from the caller.
      unsafe { swap_words(writes) }
   }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
/// Atomically swaps each `(slot, value)` pair in mapped memory and returns the
/// values the slots held. Read-only pages are made writable for the write and
/// get their protection back afterwards.
///
/// # Safety
///
/// Every slot is a mapped, pointer-aligned word, and code that reads it accepts
/// either value.
pub unsafe fn swap_words(writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
   // SAFETY: sysconf has no preconditions.
   let page_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
   let mut pages = writes
      .iter()
      .map(|&(slot, _)| slot & !(page_size - 1))
      .collect::<Vec<usize>>();
   pages.sort_unstable();
   pages.dedup();

   let maps = std::fs::read_to_string("/proc/self/maps")
      .map_err(|source| crate::Error::Protect { addr: 0, source })?;
   let restore = pages
      .iter()
      .map(|&page| Some((page, protection(&maps, page)?)))
      .collect::<Option<Vec<(usize, c_int)>>>()
      .ok_or_else(|| {
         crate::Error::Unmapped {
            addr: pages.first().copied().unwrap_or(0),
         }
      })?;
   // SAFETY: forwarded from the caller, and `restore` holds each slot's page.
   unsafe { swap_protected(writes, page_size, &restore) }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
/// Protection of the mapping holding `addr`, from `/proc/self/maps`.
fn protection(maps: &str, addr: usize) -> Option<c_int> {
   maps.lines().find_map(|line| {
      let mut fields = line.split_ascii_whitespace();
      let (start, end) = fields.next()?.split_once('-')?;
      let range = usize::from_str_radix(start, 16).ok()?..usize::from_str_radix(end, 16).ok()?;
      let perms = fields.next()?.as_bytes();
      range.contains(&addr).then(|| {
         [
            (b'r', libc::PROT_READ),
            (b'w', libc::PROT_WRITE),
            (b'x', libc::PROT_EXEC),
         ]
         .iter()
         .zip(perms)
         .filter(|((flag, _), perm)| flag == *perm)
         .fold(libc::PROT_NONE, |prot, ((_, bit), _)| prot | bit)
      })
   })
}

#[cfg(windows)]
pub(crate) mod win32 {
   use std::ffi::c_void;

   pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
   pub const MEM_COMMIT_RESERVE: u32 = 0x3000;
   pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
   pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
   pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

   #[repr(C)]
   pub struct SystemInfo {
      pub arch_or_oem: u32,
      pub page_size:   u32,
      pub _rest:       [u8; 40],
   }

   #[link(name = "kernel32")]
   unsafe extern "system" {
      pub fn GetCurrentProcess() -> *mut c_void;
      pub fn ReadProcessMemory(
         process: *mut c_void,
         addr: *const c_void,
         buffer: *mut c_void,
         size: usize,
         read: *mut usize,
      ) -> i32;
      pub fn GetSystemInfo(info: *mut SystemInfo);
      pub fn VirtualAlloc(addr: *const c_void, size: usize, kind: u32, protect: u32)
      -> *mut c_void;
      pub fn VirtualProtect(
         addr: *mut c_void,
         size: usize,
         new_protect: u32,
         old_protect: *mut u32,
      ) -> i32;
      pub fn K32EnumProcessModules(
         process: *mut c_void,
         modules: *mut *mut c_void,
         cb: u32,
         needed: *mut u32,
      ) -> i32;
      pub fn K32GetModuleFileNameExW(
         process: *mut c_void,
         module: *mut c_void,
         filename: *mut u16,
         size: u32,
      ) -> u32;
   }
}

#[cfg(windows)]
impl Image<'static> {
   /// Every loaded PE module in the current process.
   ///
   /// # Safety
   ///
   /// The segments borrow mapped memory. The modules stay loaded while the
   /// images live, and no thread writes the writable segments during a scan.
   #[must_use]
   pub unsafe fn loaded() -> Vec<Self> {
      // SAFETY: GetCurrentProcess returns a valid pseudo-handle.
      let process = unsafe { win32::GetCurrentProcess() };
      let mut needed = 0u32;
      let mut handles = vec![std::ptr::null_mut::<c_void>(); 256];
      loop {
         let cb = u32::try_from(handles.len() * size_of::<*mut c_void>()).unwrap_or(u32::MAX);
         // SAFETY: `handles` has space for `cb` bytes.
         let ok = unsafe {
            win32::K32EnumProcessModules(process, handles.as_mut_ptr(), cb, &raw mut needed)
         };
         if ok == 0 {
            return Vec::new();
         }
         let count = (needed as usize) / size_of::<*mut c_void>();
         if count <= handles.len() {
            handles.truncate(count);
            break;
         }
         handles.resize(count, std::ptr::null_mut());
      }

      handles
         .into_iter()
         .filter_map(|handle| {
            // SAFETY: `handle` is an HMODULE reported by K32EnumProcessModules.
            unsafe { Self::from_hmodule(process, handle) }
         })
         .collect()
   }

   /// # Safety
   ///
   /// `module` is a live `HMODULE` mapped into `process`.
   unsafe fn from_hmodule(process: *mut c_void, module: *mut c_void) -> Option<Self> {
      let base = module as usize;
      if base == 0 {
         return None;
      }
      let mut name_buf = [0u16; 1024];
      // SAFETY: `name_buf` is valid for its length.
      let name_len = unsafe {
         win32::K32GetModuleFileNameExW(
            process,
            module,
            name_buf.as_mut_ptr(),
            u32::try_from(name_buf.len()).ok()?,
         )
      } as usize;
      let path = OsString::from_wide(&name_buf[..name_len])
         .to_string_lossy()
         .into_owned();

      // SAFETY: a valid PE module starts with an IMAGE_DOS_HEADER (at least 64
      // bytes).
      let dos = unsafe { slice::from_raw_parts(base as *const u8, 64) };
      if &dos[0..2] != b"MZ" {
         return None;
      }
      let pe_off = usize::try_from(u32::from_le_bytes(dos[0x3C..0x40].try_into().ok()?)).ok()?;
      // SAFETY: NT headers sit within the mapped image header page.
      let nt = unsafe { slice::from_raw_parts((base + pe_off) as *const u8, 24) };
      if &nt[0..4] != b"PE\0\0" {
         return None;
      }
      let nsec = usize::from(u16::from_le_bytes(nt[6..8].try_into().ok()?));
      let opt_size = usize::from(u16::from_le_bytes(nt[20..22].try_into().ok()?));
      // SAFETY: optional header and section table follow the COFF header.
      let opt = unsafe { slice::from_raw_parts((base + pe_off + 24) as *const u8, opt_size) };
      let magic = u16::from_le_bytes(opt.get(0..2)?.try_into().ok()?);
      let (pointer_width, pdata) = match magic {
         0x020B => {
            // PE32+: Exception directory is data directory index 3 at optional
            // header offset 112 + 3*8.
            let pdata = opt.get(136..144).and_then(|dir| {
               let rva = usize::try_from(u32::from_le_bytes(dir[0..4].try_into().ok()?)).ok()?;
               let len = usize::try_from(u32::from_le_bytes(dir[4..8].try_into().ok()?)).ok()?;
               (rva != 0 && len >= 12).then_some((base, base + rva, len))
            });
            (8, pdata)
         },
         0x010B => (4, None),
         _ => return None,
      };

      let sec_table = base + pe_off + 24 + opt_size;
      // SAFETY: `nsec` 40-byte section headers follow the optional header.
      let raw_secs = unsafe { slice::from_raw_parts(sec_table as *const u8, nsec * 40) };
      let mut segments = Vec::with_capacity(nsec);
      for hdr in raw_secs.as_chunks::<40>().0 {
         let vsize = usize::try_from(u32::from_le_bytes(hdr[8..12].try_into().ok()?)).ok()?;
         let va = usize::try_from(u32::from_le_bytes(hdr[12..16].try_into().ok()?)).ok()?;
         let chars = u32::from_le_bytes(hdr[36..40].try_into().ok()?);
         if vsize == 0 || chars & win32::IMAGE_SCN_MEM_READ == 0 {
            continue;
         }
         let addr = base + va;
         // SAFETY: mapped readable PE section lives for the module's lifetime.
         let bytes = unsafe { slice::from_raw_parts(addr as *const u8, vsize) };
         segments.push(Segment {
            addr,
            bytes: Cow::Borrowed(bytes),
            executable: chars & win32::IMAGE_SCN_MEM_EXECUTE != 0,
            writable: chars & win32::IMAGE_SCN_MEM_WRITE != 0,
         });
      }

      Some(Self {
         path,
         build_id: None,
         segments,
         eh_frame_hdr: None,
         arm_exidx: None,
         pdata,
         pointer_width,
      })
   }

   /// Writes each `(slot, value)` pair into this module's mapped memory and
   /// returns the values the slots held. Read-only pages are made writable for
   /// the write and get their protection back afterwards.
   ///
   /// # Safety
   ///
   /// Every slot is a pointer-aligned word in this module, and no thread reads
   /// a slot while it is written.
   pub unsafe fn swap_words(&self, writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
      // SAFETY: forwarded from the caller.
      unsafe { swap_words(writes) }
   }
}

#[cfg(windows)]
/// Atomically swaps each `(slot, value)` pair in mapped memory and returns the
/// values the slots held. Read-only pages are made writable for the write and
/// get their protection back afterwards.
///
/// # Safety
///
/// Every slot is a mapped, pointer-aligned word, and code that reads it accepts
/// either value.
pub unsafe fn swap_words(writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
   let mut info = win32::SystemInfo {
      arch_or_oem: 0,
      page_size:   4096,
      _rest:       [0; 40],
   };
   // SAFETY: GetSystemInfo fills the struct.
   unsafe { win32::GetSystemInfo(&raw mut info) };
   let page_size = (info.page_size as usize).max(4096);

   let mut pages = writes
      .iter()
      .map(|&(slot, _)| slot & !(page_size - 1))
      .collect::<Vec<usize>>();
   pages.sort_unstable();
   pages.dedup();

   let _serial = PROTECT
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   let mut restore = Vec::with_capacity(pages.len());
   for page in pages {
      let mut old = 0u32;
      // SAFETY: `page` is a mapped page in this process.
      if unsafe {
         win32::VirtualProtect(
            page as *mut c_void,
            page_size,
            win32::PAGE_EXECUTE_READWRITE,
            &raw mut old,
         )
      } == 0
      {
         for &(done, prot) in &restore {
            let mut ignored = 0u32;
            // SAFETY: restoring previously changed page protection.
            unsafe {
               win32::VirtualProtect(done as *mut c_void, page_size, prot, &raw mut ignored);
            }
         }
         return Err(crate::Error::Protect {
            addr:   page,
            source: std::io::Error::last_os_error(),
         });
      }
      restore.push((page, old));
   }

   let previous = writes
      .iter()
      // SAFETY: the caller guarantees aligned, mapped slots, now writable.
      .map(|&(slot, value)| {
         unsafe { AtomicUsize::from_ptr(slot as *mut usize) }.swap(value, Ordering::AcqRel)
      })
      .collect();

   // A page left writable after a failed restore stays that way: the swap
   // has landed, and reporting an error would hide it from the caller.
   for (page, prot) in restore {
      let mut ignored = 0u32;
      // SAFETY: restoring the page's original protection.
      unsafe { win32::VirtualProtect(page as *mut c_void, page_size, prot, &raw mut ignored) };
   }
   Ok(previous)
}

#[cfg(any(unix, windows))]
impl Image<'static> {
   /// Overwrites `bytes.len()` bytes at `addr` in this module, through
   /// [`Self::swap_words`] on the aligned words they cover.
   ///
   /// # Safety
   ///
   /// No thread runs or reads the bytes while they are written, which spans
   /// more than one atomic write when they cross a word boundary.
   pub unsafe fn write_code(&self, addr: usize, bytes: &[u8]) -> Result<(), crate::Error> {
      const WORD: usize = size_of::<usize>();
      let span = addr..addr + bytes.len();
      let mut writes = Vec::new();
      for word in (span.start & !(WORD - 1)..span.end).step_by(WORD) {
         let mut current: [u8; WORD] = self
            .read(word, WORD)
            .and_then(|current| current.try_into().ok())
            .ok_or(crate::Error::Unmapped { addr: word })?;
         for (at, byte) in (word..).zip(&mut current) {
            if span.contains(&at) {
               *byte = bytes[at - span.start];
            }
         }
         writes.push((word, usize::from_le_bytes(current)));
      }
      // SAFETY: aligned words in this module, under the caller's guarantee.
      unsafe { self.swap_words(&writes) }.map(drop)
   }

   /// Sends calls to the function at `addr` to `detour` with an absolute jump
   /// over its first `prologue_len` bytes, and returns a trampoline that runs
   /// those bytes and jumps back past them: the detour calls the trampoline to
   /// reach the original. The detour can run as soon as the jump is written,
   /// before this returns, so hook from `DllMain` or make it cope with a
   /// trampoline not stored yet.
   ///
   /// `prologue` must match at `addr`, cover whole instructions with no
   /// RIP-relative operands, and span at least the jump's 14 bytes, so the
   /// displaced instructions run unchanged from the trampoline.
   ///
   /// # Safety
   ///
   /// `detour` has the function's exact ABI, and no thread runs the
   /// function's first bytes while they are written.
   #[cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]
   pub unsafe fn hook_entry(
      &self,
      addr: usize,
      prologue: Pattern,
      prologue_len: usize,
      detour: usize,
   ) -> Result<usize, crate::Error> {
      let refuse = |message: &str| {
         crate::Error::Attach {
            backend: "inline",
            target:  addr,
            message: message.to_owned(),
         }
      };
      let displaced = self
         .read(addr, prologue_len)
         .ok_or(crate::Error::Unmapped { addr })?;
      if prologue_len < ABSOLUTE_JUMP_LEN || prologue.find_in(displaced).first() != Some(&0) {
         return Err(refuse("the prologue does not match"));
      }
      if !crate::x86::relocatable(displaced, addr) {
         return Err(refuse(
            "the prologue does not end on an instruction, or moves badly",
         ));
      }
      let mut code = displaced.to_vec();
      code.extend(absolute_jump(addr + prologue_len));
      let trampoline =
         alloc_exec(0, code.len()).ok_or_else(|| refuse("no memory for the trampoline"))?;
      // SAFETY: `trampoline` holds `code.len()` writable bytes.
      unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), trampoline as *mut u8, code.len()) };
      // SAFETY: forwarded from the caller.
      unsafe { self.write_code(addr, &absolute_jump(detour)) }?;
      Ok(trampoline)
   }
}

impl Image<'_> {
   #[must_use]
   pub fn file_name(&self) -> &str {
      Path::new(&self.path)
         .file_name()
         .and_then(|name| name.to_str())
         .unwrap_or(&self.path)
   }

   /// Addresses in executable segments where `pattern` matches.
   #[must_use]
   pub fn scan(&self, pattern: Pattern) -> Vec<usize> {
      self
         .segments
         .iter()
         .filter(|segment| segment.executable)
         .flat_map(|segment| {
            pattern
               .find_in(&segment.bytes)
               .into_iter()
               .map(|offset| segment.addr + offset)
         })
         .collect()
   }

   /// `(field, target)` for each 32-bit displacement in executable segments
   /// that points into `targets` relative to its own end, as in x86-64
   /// RIP-relative `lea`, `mov` and `call`. Every offset is tried, so a hit
   /// can be noise, and an instruction with an immediate after its
   /// displacement is missed. Disassemble around a hit to confirm it.
   #[must_use]
   pub fn rip_refs(&self, targets: std::ops::Range<usize>) -> Vec<(usize, usize)> {
      let mut found = Vec::new();
      for segment in self.segments.iter().filter(|segment| segment.executable) {
         for (offset, window) in segment.bytes.windows(4).enumerate() {
            let rel = i32::from_le_bytes([window[0], window[1], window[2], window[3]]);
            let field = segment.addr + offset;
            if let Some(target) = (field + 4).checked_add_signed(rel as isize)
               && targets.contains(&target)
            {
               found.push((field, target));
            }
         }
      }
      found
   }

   /// Addresses of `needle` in any segment. Linkers often merge `.rodata` into
   /// the executable segment, so strings are searched everywhere.
   #[must_use]
   pub fn find_bytes(&self, needle: &[u8]) -> Vec<usize> {
      self.find_in(needle, |_| true)
   }

   /// [`Image::find_bytes`], lazily in address order, so a caller can stop at
   /// the first hit it accepts instead of scanning (and faulting in) the rest.
   pub fn find_bytes_iter<'a>(&'a self, needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
      self.segments.iter().flat_map(move |segment| {
         memmem::find_iter(&segment.bytes, needle).map(move |offset| segment.addr + offset)
      })
   }

   /// Pointer-aligned slots in non-executable segments that hold `target`.
   #[must_use]
   pub fn pointers_to(&self, target: usize) -> Vec<usize> {
      let width = self.pointer_width;
      self
         .find_in(&target.to_le_bytes()[..width], |segment| {
            !segment.executable
         })
         .into_iter()
         .filter(|slot| slot % width == 0)
         .collect()
   }

   fn find_in(&self, needle: &[u8], keep: impl Fn(&Segment<'_>) -> bool) -> Vec<usize> {
      self
         .segments
         .iter()
         .filter(|segment| keep(segment))
         .flat_map(|segment| {
            memmem::find_iter(&segment.bytes, needle).map(|offset| segment.addr + offset)
         })
         .collect()
   }

   #[must_use]
   pub fn read(&self, addr: usize, len: usize) -> Option<&[u8]> {
      let segment = self
         .segments
         .iter()
         .find(|segment| segment.contains(addr))?;
      let offset = addr - segment.addr;
      segment.bytes.get(offset..offset + len)
   }

   #[must_use]
   pub fn read_usize(&self, addr: usize) -> Option<usize> {
      let bytes = self.read(addr, self.pointer_width)?;
      let mut word = [0u8; 8];
      word[..bytes.len()].copy_from_slice(bytes);
      usize::try_from(u64::from_le_bytes(word)).ok()
   }

   #[must_use]
   pub fn read_u32(&self, addr: usize) -> Option<u32> {
      Some(u32::from_le_bytes(self.read(addr, 4)?.try_into().ok()?))
   }

   #[must_use]
   pub fn is_code(&self, addr: usize) -> bool {
      self
         .segments
         .iter()
         .any(|segment| segment.executable && segment.contains(addr))
   }

   /// Whether an unwind table starts a function at `addr`. Linkers often put
   /// `.rodata` in the executable segment, so [`Image::is_code`] alone accepts
   /// pointers to strings. Images without a search table fall back to it.
   #[must_use]
   pub fn is_function(&self, addr: usize) -> bool {
      if self.arm_exidx.is_none() && self.eh_frame_hdr.is_none() && self.pdata.is_none() {
         return self.is_code(addr);
      }
      // Thumb code pointers carry bit 0, and `.ARM.exidx` holds the even start.
      let start = if self.arm_exidx.is_some() {
         addr & !1
      } else {
         addr
      };
      self.function_containing(start) == Some(start)
   }

   /// Start of the last function at or below `addr` in the unwind table. The
   /// table holds starts only, so an address past a function's end still maps
   /// to it. ARM32 reads `.ARM.exidx`, PE64 reads `.pdata`, every other target
   /// `.eh_frame_hdr`.
   #[must_use]
   pub fn function_containing(&self, addr: usize) -> Option<usize> {
      if let Some((image_base, base, len)) = self.pdata {
         return self
            .pdata_bounds(image_base, base, len / 12, addr)
            .map(|(start, _)| start);
      }

      if let Some((base, len)) = self.arm_exidx {
         // Each 8-byte entry opens with a prel31 offset from it to its
         // function.
         let start = |index: usize| {
            let at = base + index * 8;
            let prel31 = (self.read_u32(at)? << 1).cast_signed() >> 1;
            at.checked_add_signed(prel31 as isize)
         };
         return start(last_at_or_below(len / 8, start, addr)?);
      }

      self.eh_frame_entry(addr).map(|(start, _)| start)
   }

   /// Exact `[start, end)` bounds of the function containing `addr`, from PE64
   /// `.pdata` or the FDE `.eh_frame_hdr` points at.
   #[must_use]
   pub fn function_bounds(&self, addr: usize) -> Option<(usize, usize)> {
      if let Some((image_base, base, len)) = self.pdata {
         return self.pdata_bounds(image_base, base, len / 12, addr);
      }
      let (start, fde) = self.eh_frame_entry(addr)?;
      let end = start.checked_add(self.fde_range(fde, start)?)?;
      (addr < end).then_some((start, end))
   }

   /// `(start, fde)` of the last `.eh_frame_hdr` search table entry at or below
   /// `addr`.
   fn eh_frame_entry(&self, addr: usize) -> Option<(usize, usize)> {
      const UDATA4: u8 = 0x03;
      const SDATA4: u8 = 0x0B;
      const DATAREL_SDATA4: u8 = 0x3B;
      let hdr = self.eh_frame_hdr?;
      let &[1, frame_encoding, UDATA4, DATAREL_SDATA4] = self.read(hdr, 4)? else {
         return None;
      };
      if !matches!(frame_encoding & 0x0F, UDATA4 | SDATA4) {
         return None;
      }
      let word = |at: usize| Some(i32::from_ne_bytes(self.read(at, 4)?.try_into().ok()?));
      let field = |at: usize| word(at).and_then(|rel| hdr.checked_add_signed(rel as isize));
      let count = usize::try_from(word(hdr + 8)?).ok()?;
      let start = |index: usize| field(hdr + 12 + index * 8);
      let index = last_at_or_below(count, start, addr)?;
      Some((start(index)?, field(hdr + 16 + index * 8)?))
   }

   /// The `pc_range` of the FDE at `fde`, in the pointer encoding its CIE's
   /// `R` augmentation names. Its `pc_begin` must decode to `start`.
   fn fde_range(&self, fde: usize, start: usize) -> Option<usize> {
      let cie = (fde + 4).checked_sub(self.read_u32(fde + 4)? as usize)?;
      if self.read_u32(cie)? >= 0xFFFF_FFF0 || self.read_u32(cie + 4)? != 0 {
         return None;
      }
      let mut at = cie + 9;
      let augmentation = self.read(cie + 9, 16)?;
      let augmentation = &augmentation[..augmentation.iter().position(|&b| b == 0)?];
      at += augmentation.len() + 1;
      self.uleb(&mut at)?; // code alignment
      self.uleb(&mut at)?; // data alignment
      if self.read(cie + 8, 1)? == [1] {
         at += 1;
      } else {
         self.uleb(&mut at)?;
      }
      let mut encoding = 0; // DW_EH_PE_absptr
      if augmentation.first() == Some(&b'z') {
         self.uleb(&mut at)?;
         for &kind in &augmentation[1..] {
            if kind == b'S' {
               continue;
            }
            let enc = self.read(at, 1)?[0];
            at += 1;
            match kind {
               b'R' => encoding = enc,
               b'L' => {},
               b'P' => at += pointer_size(enc, self.pointer_width)?,
               _ => return None,
            }
         }
      }
      let size = pointer_size(encoding, self.pointer_width)?;
      let read = |at: usize| -> Option<i64> {
         let bytes = self.read(at, size)?;
         let mut word = [0u8; 8];
         word[..size].copy_from_slice(bytes);
         let raw = i64::from_le_bytes(word);
         // DW_EH_PE_sdata*
         Some(if encoding & 0x08 != 0 && size < 8 {
            raw << (64 - size * 8) >> (64 - size * 8)
         } else {
            raw
         })
      };
      let begin = match encoding & 0x70 {
         0x00 => usize::try_from(read(fde + 8)?).ok()?,
         0x10 => (fde + 8).checked_add_signed(isize::try_from(read(fde + 8)?).ok()?)?,
         _ => return None,
      };
      (begin == start).then_some(usize::try_from(read(fde + 8 + size)?).ok()?)
   }

   /// The ULEB128 at `*at`, moving `at` past it.
   fn uleb(&self, at: &mut usize) -> Option<usize> {
      let (mut value, mut shift) = (0usize, 0);
      loop {
         let byte = self.read(*at, 1)?[0];
         *at += 1;
         value |= usize::from(byte & 0x7F).checked_shl(shift)?;
         shift += 7;
         if byte & 0x80 == 0 {
            return Some(value);
         }
      }
   }

   /// Binary search over PE64 `.pdata`, whose `count` `RUNTIME_FUNCTION`
   /// entries are 12 bytes (`begin_rva`, `end_rva`, `unwind_rva`), sorted by
   /// `begin_rva`.
   fn pdata_bounds(
      &self,
      image_base: usize,
      base: usize,
      count: usize,
      addr: usize,
   ) -> Option<(usize, usize)> {
      let rva = addr.checked_sub(image_base)?;
      let field = |at: usize| self.read_u32(at).map(|value| value as usize);
      let index = last_at_or_below(count, |index| field(base + index * 12), rva)?;
      let (start, end) = (field(base + index * 12)?, field(base + index * 12 + 4)?);
      (rva < end).then_some((image_base + start, image_base + end))
   }
}

/// Bytes a `DW_EH_PE_*` pointer encoding takes.
const fn pointer_size(encoding: u8, pointer_width: usize) -> Option<usize> {
   match encoding & 0x0F {
      0x00 => Some(pointer_width),
      0x02 | 0x0A => Some(2),
      0x03 | 0x0B => Some(4),
      0x04 | 0x0C => Some(8),
      _ => None,
   }
}

/// Index of the last of `count` ascending function starts at or below `addr`.
fn last_at_or_below(
   count: usize,
   start: impl Fn(usize) -> Option<usize>,
   addr: usize,
) -> Option<usize> {
   let (mut low, mut high) = (0, count);
   while low < high {
      let mid = low + (high - low) / 2;
      if start(mid)? <= addr {
         low = mid + 1;
      } else {
         high = mid;
      }
   }
   low.checked_sub(1)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn function_bounds_read_the_fde_range() {
      let mut bytes = vec![0u8; 0x100];
      let mut put = |at: usize, data: &[u8]| bytes[at..at + data.len()].copy_from_slice(data);
      // `.eh_frame_hdr` at 0x1000 with one entry: the function at 0x2000 and
      // its FDE at 0x1040, both relative to the header.
      put(0x00, &[1, 0x1B, 0x03, 0x3B, 0, 0, 0, 0, 1, 0, 0, 0]);
      put(0x0C, &0x1000_i32.to_le_bytes());
      put(0x10, &0x40_i32.to_le_bytes());
      // CIE at 0x1020: version 1, "zR", code 1, data -8, return register 16,
      // pointers `pcrel | sdata4`.
      put(0x20, &[
         0x10, 0, 0, 0, 0, 0, 0, 0, 1, b'z', b'R', 0, 1, 0x78, 0x10, 1, 0x1B,
      ]);
      // FDE at 0x1040: its CIE 0x24 bytes back, `pc_begin` relative to itself
      // and a `pc_range` of 0x80.
      put(0x40, &[0x14, 0, 0, 0, 0x24, 0, 0, 0]);
      put(0x48, &(0x2000_i32 - 0x1048).to_le_bytes());
      put(0x4C, &0x80_u32.to_le_bytes());
      let image = Image {
         path:          String::new(),
         build_id:      None,
         segments:      vec![Segment {
            addr:       0x1000,
            bytes:      Cow::Owned(bytes),
            executable: false,
            writable:   false,
         }],
         eh_frame_hdr:  Some(0x1000),
         arm_exidx:     None,
         pdata:         None,
         pointer_width: 8,
      };
      assert_eq!(image.function_bounds(0x2010), Some((0x2000, 0x2080)));
      assert_eq!(image.function_bounds(0x2080), None);
      assert_eq!(image.function_containing(0x2080), Some(0x2000));
   }
}
