//! Build-independent discovery of Desktop Phoenix's `libdiscord/crates/http`
//! `ReqwestTransport` and `SendFuture` vtable slots.
//!
//! Desktop Phoenix routes all Discord REST traffic through a single-method
//! `dyn Transport` trait (`ReqwestTransport::send(&self, req) -> Pin<Box<dyn
//! Future>>`). Swapping the `send` and `poll` vtable slots in `.rdata`
//! intercepts every outgoing request and incoming response without any inline
//! code patching.

use memchr::memchr_iter;
use phoenix_hook::{
   image::Image,
   x86::{
      Abi,
      Held,
      PointerUse,
      call_target,
      jump_target,
      pointer_uses,
   },
};

use crate::Found;

/// Unique error string inside `ReqwestTransport::send` when handed a mobile
/// host-file body.
const HOST_FILE_ANCHOR: &[u8] = b"a host file body needs the host's transport";

/// `http::HeaderMap`, which `send` moves into reqwest's builder whole.
pub const HEADER_MAP_SIZE: usize = 0x60;
/// The request's `Body` enum: its tag and a `bytes::Bytes`.
pub const BODY_SIZE: usize = 0x38;
/// The request's `String` URL.
pub const URL_SIZE: usize = 0x18;
/// `http::Method`.
pub const METHOD_SIZE: usize = 0x18;
/// The `Poll<Result<Response, _>>` the response hooks read: headers, body
/// and status.
pub const POLL_RESULT_SIZE: usize = 0x80;

/// Where `ReqwestTransport::send` reads each field of the `TransportRequest`
/// it is handed. Its Rust layout moves between builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLayout {
   pub headers: usize,
   pub body:    usize,
   pub url:     usize,
   pub method:  usize,
}

/// Located vtable slots and host allocator entry points for Desktop Phoenix's
/// `libdiscord/crates/http` transport.
#[derive(Debug, Clone)]
pub struct DesktopHttpTransport {
   /// All `.rdata` vtable slots holding `ReqwestTransport::send` (may be
   /// duplicated across LLVM CGUs).
   pub send_slots:   Vec<usize>,
   /// Original `ReqwestTransport::send` function address.
   pub send_fn:      usize,
   /// All `.rdata` vtable slots holding `SendFuture::poll`.
   pub poll_slots:   Vec<usize>,
   /// Original `SendFuture::poll` function address.
   pub poll_fn:      usize,
   /// Host's `__rust_alloc(size, align)` if located.
   pub host_alloc:   Option<usize>,
   /// Host's `__rust_dealloc(ptr, size, align)` if located.
   pub host_dealloc: Option<usize>,
   pub request:      RequestLayout,
   /// The size of `poll`'s `Poll<Result<Response, _>>`, from the stores
   /// through its hidden return pointer, when any are found.
   pub poll_result:  Option<usize>,
}

impl DesktopHttpTransport {
   /// Scans a 64-bit PE or ELF `Image` for `ReqwestTransport::send`, its
   /// returned `SendFuture::poll` vtable, and the host's Rust allocator
   /// functions.
   pub fn locate(image: &Image<'_>) -> Found<Self> {
      if image.pointer_width != 8 {
         return Err("not a 64-bit image".to_owned());
      }
      let abi = Abi::of(image);

      // 1. The unique anchor string inside `ReqwestTransport::send`.
      let anchor = crate::one("transport anchor", &image.find_bytes(HOST_FILE_ANCHOR))?;
      let lea = *crate::desktop::lea_refs(image, anchor)
         .first()
         .ok_or("transport anchor is never loaded")?;
      let (send_fn, send_end) = image
         .function_bounds(lea)
         .ok_or("ReqwestTransport::send has no unwind entry")?;
      let request = request_layout(image, abi, send_fn, send_end, lea)?;

      // 2. Every `dyn Transport` vtable slot in `.rdata` pointing to `send_fn`.
      let send_slots = find_vtable_method_slots(image, send_fn, 1..=0x2000);
      if send_slots.is_empty() {
         return Err("no Transport vtable holds ReqwestTransport::send".to_owned());
      }

      // 3. The `Box::pin` of the async state machine returns `(ptr,
      //    poll_vtable)`, from `send_fn` itself or a small helper it calls.
      let (poll_fn, poll_vtable, box_future) = find_poll_vtable(image, send_fn, send_end)?;
      let mut poll_slots = find_vtable_method_slots(image, poll_fn, 0x100..=0x4000);
      let direct_slot = poll_vtable + 24;
      if !poll_slots.contains(&direct_slot) {
         poll_slots.push(direct_slot);
      }

      // 4. The host's own `__rust_alloc` and `__rust_dealloc`, so response body
      //    growth stays allocator-compatible. Both hand the same global
      //    allocator static to its implementation.
      let host_alloc = box_future.and_then(|box_future| find_host_alloc(image, abi, box_future));
      let host_dealloc = find_host_dealloc(image, abi, send_fn, send_end)
         .or_else(|| host_alloc.and_then(|alloc| dealloc_beside(image, abi, alloc)));
      let (host_alloc, host_dealloc) = match (host_alloc, host_dealloc) {
         (Some(alloc), Some(dealloc))
            if allocator_static(image, abi, alloc).is_none()
               || allocator_static(image, abi, alloc) != allocator_static(image, abi, dealloc) =>
         {
            (None, None)
         },
         pair => pair,
      };

      // Poll writes its result in whichever state arm finishes, so every
      // store through the return pointer counts, not only those before a `ret`.
      let poll_result = image
         .function_bounds(poll_fn)
         .and_then(|(start, end)| image.read(start, end - start))
         .and_then(|code| {
            pointer_uses(abi, code, poll_fn, abi.arg(0))
               .into_iter()
               .filter_map(|used| {
                  match used {
                     PointerUse::Store { offset, size, .. } => Some(offset + size),
                     PointerUse::Copy {
                        dest: Some(Held::At(offset)),
                        count: Some(Held::Constant(count)),
                        ..
                     } => Some(offset + usize::try_from(count).ok()?),
                     _ => None,
                  }
               })
               .max()
         })
         .map(|end| end.next_multiple_of(8));

      Ok(Self {
         send_slots,
         send_fn,
         poll_slots,
         poll_fn,
         host_alloc,
         host_dealloc,
         request,
         poll_result,
      })
   }
}

/// Decodes the `TransportRequest` layout from what `send` does with its
/// second argument, followed by [`pointer_uses`]:
///
/// - The body's tag indexes a jump table, the last one before the host-file
///   error at `anchor`, one of its arms.
/// - The URL and method go to reqwest's builder in one call, its 4th and 3rd
///   arguments, each the field's address or that of a copy of it.
/// - The headers are moved out whole with a `rep movsb` of their size.
///
/// The four fields must be 8-byte aligned and not overlap.
fn request_layout(
   image: &Image<'_>,
   abi: Abi,
   send_fn: usize,
   send_end: usize,
   anchor: usize,
) -> Found<RequestLayout> {
   let code = image
      .read(send_fn, send_end - send_fn)
      .ok_or("ReqwestTransport::send is unmapped")?;
   let uses = pointer_uses(abi, code, send_fn, abi.arg(1));

   let body = uses
      .iter()
      .rev()
      .find_map(|used| {
         match *used {
            PointerUse::Dispatch { at, field } if at < anchor => Some(field),
            _ => None,
         }
      })
      .ok_or("no jump table on the request body before the host-file arm")?;

   let points_at = |held: Option<Held>| {
      match held? {
         Held::At(offset) | Held::CopyOf(offset) => Some(offset),
         Held::Field(_) | Held::Constant(_) => None,
      }
   };
   let (url, method) = uses
      .iter()
      .find_map(|used| {
         match *used {
            PointerUse::Call { args, .. } => Some((points_at(args[3])?, points_at(args[2])?)),
            _ => None,
         }
      })
      .ok_or("no builder call takes the request's URL and method")?;

   let headers = uses
      .iter()
      .find_map(|used| {
         match *used {
            PointerUse::Copy {
               source: Some(Held::At(offset)),
               count: Some(Held::Constant(count)),
               ..
            } if count == HEADER_MAP_SIZE as u64 => Some(offset),
            _ => None,
         }
      })
      .ok_or("send never moves the request's headers out")?;

   let layout = RequestLayout {
      headers,
      body,
      url,
      method,
   };
   let mut spans = [
      (headers, HEADER_MAP_SIZE),
      (body, BODY_SIZE),
      (url, URL_SIZE),
      (method, METHOD_SIZE),
   ];
   spans.sort_unstable();
   if spans
      .windows(2)
      .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
   {
      return Err(format!("request fields overlap: {layout:?}"));
   }
   if spans.iter().any(|&(offset, _)| offset % 8 != 0) {
      return Err(format!("request fields are not 8-byte aligned: {layout:?}"));
   }
   Ok(layout)
}

/// Finds all `.rdata` vtable method-0 slots (`vtable + 24`) holding `method_fn`
/// where `[drop_fn, size, align, method_fn]` passes Rust trait vtable checks.
fn find_vtable_method_slots(
   image: &Image<'_>,
   method_fn: usize,
   size_range: std::ops::RangeInclusive<usize>,
) -> Vec<usize> {
   let mut slots = Vec::new();
   for slot in image.pointers_to(method_fn) {
      let Some(vtable) = slot.checked_sub(24) else {
         continue;
      };
      let (Some(drop_fn), Some(size), Some(align)) = (
         image.read_usize(vtable),
         image.read_usize(vtable + 8),
         image.read_usize(vtable + 16),
      ) else {
         continue;
      };
      if image.is_function(drop_fn)
         && size_range.contains(&size)
         && align.is_power_of_two()
         && align <= 64
      {
         slots.push(slot);
      }
   }
   slots.sort_unstable();
   slots.dedup();
   slots
}

/// The functions called inside `[start, end)`.
fn calls(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   crate::desktop::callees(image, start, end)
      .into_iter()
      .filter(|&callee| image.is_function(callee))
      .collect()
}

/// The bounds of `func` when it is at most `max` bytes long.
fn small(image: &Image<'_>, func: usize, max: usize) -> Option<(usize, usize)> {
   image
      .function_bounds(func)
      .filter(|&(start, end)| end - start <= max)
}

/// The `SendFuture` vtable loaded with `lea rdx, [rip + disp32]` (`48 8D 15`),
/// the high half of the returned fat pointer, in a small callee of `send_fn`
/// that boxes the future, or else in `send_fn`. Returns `(poll_fn, vtable,
/// box_future)`, where `box_future` is the callee that boxes it, when one does.
///
/// `lea rdx` also passes an argument, so the vtable must be the only one in
/// whichever of the two places has any.
fn find_poll_vtable(
   image: &Image<'_>,
   send_fn: usize,
   send_end: usize,
) -> Found<(usize, usize, Option<usize>)> {
   let boxers: Vec<(usize, usize)> = calls(image, send_fn, send_end)
      .into_iter()
      .filter_map(|callee| small(image, callee, 0x80))
      .collect();
   for group in [boxers, vec![(send_fn, send_end)]] {
      let mut found: Vec<(usize, usize, usize)> = group
         .into_iter()
         .flat_map(|(cs, ce)| {
            future_vtables(image, cs, ce).map(move |(vtable, poll_fn)| (vtable, poll_fn, cs))
         })
         .collect();
      found.sort_unstable();
      found.dedup_by_key(|&mut (vtable, ..)| vtable);
      match found[..] {
         [] => {},
         [(vtable, poll_fn, cs)] => return Ok((poll_fn, vtable, (cs != send_fn).then_some(cs))),
         _ => return Err(format!("{} SendFuture vtable candidates", found.len())),
      }
   }
   Err("SendFuture vtable not found".to_owned())
}

/// `(vtable, poll_fn)` for each `lea rdx, [rip + vtable]` in `[cs, ce)` whose
/// target reads as a future's vtable.
fn future_vtables(image: &Image<'_>, cs: usize, ce: usize) -> impl Iterator<Item = (usize, usize)> {
   let code = image.read(cs, ce - cs).unwrap_or_default();
   memchr_iter(0x8D, code)
      .filter(move |&pos| {
         pos > 0 && pos + 6 <= code.len() && code[pos - 1] == 0x48 && code[pos + 1] == 0x15
      })
      .filter_map(move |pos| {
         let disp = i32::from_le_bytes(code[pos + 2..pos + 6].try_into().ok()?);
         let vtable = (cs + pos + 6).checked_add_signed(disp as isize)?;
         let (drop_fn, size, align, poll_fn) = (
            image.read_usize(vtable)?,
            image.read_usize(vtable + 8)?,
            image.read_usize(vtable + 16)?,
            image.read_usize(vtable + 24)?,
         );
         (image.is_function(drop_fn)
            && image.is_function(poll_fn)
            && (0x100..=0x4000).contains(&size)
            && align == 8)
            .then_some((vtable, poll_fn))
      })
      .collect::<Vec<_>>()
      .into_iter()
}

/// `__rust_alloc(size, align)`, reached from `box_future`.
///
/// Some call in `box_future` or a small callee passes the future's alignment,
/// a constant 8, in an argument register. Win64 builds make that the call to
/// `__rust_alloc`, System V ones a call to `Layout` wrappers that forward to
/// it, so small callees are followed breadth-first to the first function
/// shaped like `__rust_alloc`: its first call, whether the allocator is set
/// up, is followed by `test al, al`. Bool helpers share that shape, so the
/// first level with any must have exactly one.
fn find_host_alloc(image: &Image<'_>, abi: Abi, box_future: usize) -> Option<usize> {
   let (bs, be) = image.function_bounds(box_future)?;
   let align_8: Vec<Vec<u8>> = (0..2)
      .flat_map(|index| crate::desktop::imm_loads(abi.arg(index), 8))
      .collect();
   let mut frontier = Vec::new();
   for (ws, we) in std::iter::once((bs, be)).chain(
      calls(image, bs, be)
         .into_iter()
         .filter_map(|f| small(image, f, 0x60)),
   ) {
      let code = image.read(ws, we - ws)?;
      for pos in 0..code.len() {
         let Some((_, callee)) = call_target(image, &code[pos..], ws + pos) else {
            continue;
         };
         let before = &code[pos.saturating_sub(12)..pos];
         if align_8
            .iter()
            .any(|load| before.windows(load.len()).any(|w| w == load))
            && image.is_function(callee)
         {
            frontier.push(callee);
         }
      }
   }
   for _ in 0..3 {
      let mut found: Vec<usize> = frontier
         .iter()
         .copied()
         .filter(|&func| checks_allocator(image, func))
         .collect();
      found.sort_unstable();
      found.dedup();
      match found[..] {
         [] => {},
         [alloc] => return Some(alloc),
         _ => return None,
      }
      frontier = frontier
         .iter()
         .filter_map(|&func| small(image, func, 0x60))
         .flat_map(|(start, end)| calls(image, start, end))
         .collect();
   }
   None
}

/// Whether the first call in `func` is followed by `test al, al`.
fn checks_allocator(image: &Image<'_>, func: usize) -> bool {
   let Some(code) = image.read(func, 0x20) else {
      return false;
   };
   phoenix_hook::x86::disasm(code, func, 12)
      .iter()
      .find(|line| line.text.starts_with("call "))
      .is_some_and(|call| image.read(call.addr + call.len, 2) == Some(&[0x84, 0xC0]))
}

/// Follows `send_fn` → request builder helper → leaf `Method::drop`, a tail
/// call `__rust_dealloc(ptr, size, 1)` that sets only the alignment
/// argument. Leaf functions have no `.pdata` entry on Windows x64, so inner
/// call targets are checked via `is_code`. Other leaves end the same way, so
/// the target must also count the freed bytes down with `lock sub`.
fn find_host_dealloc(
   image: &Image<'_>,
   abi: Abi,
   send_fn: usize,
   send_end: usize,
) -> Option<usize> {
   let tails = crate::desktop::imm_loads(abi.arg(2), 1);
   let dealloc_in = |code: &[u8], ip: usize| {
      tails.iter().find_map(|tail| {
         memchr::memmem::find_iter(code, tail).find_map(|hit| {
            // A size-optimized leaf may load the pointer between the two.
            let after = hit + tail.len();
            (after..code.len().min(after + 8)).find_map(|at| {
               jump_target(image, &code[at..], ip + at)
                  .filter(|&dealloc| image.is_function(dealloc) && counts_down(image, dealloc))
            })
         })
      })
   };
   for callee in calls(image, send_fn, send_end) {
      let Some((cs, ce)) = small(image, callee, 0x400) else {
         continue;
      };
      let Some(callee_code) = image.read(cs, ce - cs) else {
         continue;
      };
      for pos in 0..callee_code.len() {
         let Some((_, inner)) = call_target(image, &callee_code[pos..], cs + pos) else {
            continue;
         };
         if let Some(code) = image.read(inner, 0x30).filter(|_| image.is_code(inner))
            && let Some(dealloc) = dealloc_in(code, inner)
         {
            return Some(dealloc);
         }
      }
   }
   None
}

/// Whether `func` counts freed bytes down with `lock sub` near its start.
fn counts_down(image: &Image<'_>, func: usize) -> bool {
   image.read(func, 0x40).is_some_and(|body| {
      body
         .windows(3)
         .any(|w| w == [0xF0, 0x48, 0x29] || w == [0xF0, 0x4C, 0x29])
   })
}

/// `__rust_dealloc`, from the allocator shims emitted next to `alloc`.
///
/// System V builds free request bodies through wrappers the leaf search in
/// [`find_host_dealloc`] does not reach. `alloc` hands the global allocator
/// static to its implementation with `lea rdi, [rip + disp32]`, and so does
/// `__rust_dealloc`, without `alloc`'s set-up check and counting the freed
/// bytes down after.
fn dealloc_beside(image: &Image<'_>, abi: Abi, alloc: usize) -> Option<usize> {
   /// The shims are a few dozen bytes each.
   const NEAR: usize = 0x400;
   let global = allocator_static(image, abi, alloc)?;
   let start = alloc.saturating_sub(NEAR);
   let mut found: Vec<usize> = lea_arg0(abi, image.read(start, 2 * NEAR)?, start)
      .filter(|&(_, target)| target == global)
      .filter_map(|(lea, _)| Some(image.function_bounds(lea)?.0))
      .filter(|&func| func != alloc && !checks_allocator(image, func) && counts_down(image, func))
      .collect();
   found.dedup();
   match found[..] {
      [dealloc] => Some(dealloc),
      _ => None,
   }
}

/// The global allocator static an allocator shim hands its implementation:
/// the first `lea` of it into the first argument register.
fn allocator_static(image: &Image<'_>, abi: Abi, shim: usize) -> Option<usize> {
   lea_arg0(abi, image.read(shim, 0x40)?, shim)
      .next()
      .map(|(_, target)| target)
}

/// Each `lea <first argument>, [rip + disp32]` in `code` at `ip`, as
/// (instruction, target).
fn lea_arg0(abi: Abi, code: &[u8], ip: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
   let lea: &'static [u8] = match abi {
      Abi::Win64 => &[0x48, 0x8D, 0x0D],
      Abi::SysV => &[0x48, 0x8D, 0x3D],
   };
   memchr::memmem::find_iter(code, lea).filter_map(move |pos| {
      let disp = i32::from_le_bytes(code.get(pos + 3..pos + 7)?.try_into().ok()?);
      Some((ip + pos, (ip + pos + 7).checked_add_signed(disp as isize)?))
   })
}
