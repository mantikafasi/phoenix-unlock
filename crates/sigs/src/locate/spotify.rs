//! Spotify behaviors `phoenix.spotify` turns off, each a one-instruction
//! rewrite of `libdiscord/crates/ops-spotify` and the presence builder.

#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
use phoenix_hook::pattern::Pattern;

use crate::{
   Found,
   index::{
      Index,
      Isa,
      Op,
   },
};

/// One instruction's bytes as Discord ships them and as patched. An x86 patch
/// is one byte and an arm64 one a whole aligned instruction, so a thread at
/// `site` runs the old or the new instruction whole.
#[derive(Clone, Copy, Debug)]
pub struct CodePatch {
   pub site:    usize,
   pub len:     usize,
   pub shipped: [u8; 4],
   pub patched: [u8; 4],
}

impl CodePatch {
   fn at(ix: &Index<'_>, site: usize, patched: &[u8]) -> Found<Self> {
      let len = patched.len();
      let shipped = ix
         .image
         .read(site, len)
         .ok_or_else(|| format!("{site:#x} is unmapped"))?;
      let mut patch = Self {
         site,
         len,
         shipped: [0; 4],
         patched: [0; 4],
      };
      patch.shipped[..len].copy_from_slice(shipped);
      patch.patched[..len].copy_from_slice(patched);
      Ok(patch)
   }
}

/// The entry of the function that pauses Spotify after 30 s of talking in
/// voice, which logs `spotify: playback auto paused`, made to return at once.
///
/// Every caller is its timer's poll, which passes the address of its own
/// `Arc` and takes that address again after the call to drop it, so the
/// callee only borrows and returning early leaves nothing behind. The check
/// requires that shape at every call site.
pub fn auto_pause(ix: &Index<'_>) -> Found<CodePatch> {
   const ARM64_RET: [u8; 4] = 0xD65F_03C0_u32.to_le_bytes();
   // The caller's `nop` after a call that may unwind sits between the two.
   const REACH: usize = 2;

   let func = ix
      .literal(b"spotify: playback auto paused")
      .one("spotify auto pause")?;
   let this = ix.isa.arg(0).ok_or("no first argument register")?;
   let sites = ix.call_sites(func.start);
   if sites.is_empty() {
      return Err(format!("spotify auto pause at {func:?} has no callers"));
   }
   for site in sites {
      let caller = ix
         .function(site)
         .ok_or_else(|| format!("spotify auto pause call {site:#x} is outside a function"))?;
      let steps = caller.steps();
      let at = steps
         .iter()
         .position(|step| step.at == site)
         .ok_or_else(|| format!("spotify auto pause call {site:#x} does not decode"))?;
      let lent = at.checked_sub(1).map(|before| steps[before].op);
      let Some(Op::Offset { reg, .. }) = lent.filter(|&op| matches!(op, Op::Offset { .. })) else {
         return Err(format!("spotify auto pause call {site:#x} passes no local's address"));
      };
      let kept = steps[at + 1..]
         .iter()
         .take(REACH)
         .any(|step| Some(step.op) == lent);
      if reg != this || !kept {
         return Err(format!("spotify auto pause call {site:#x} gives its argument away"));
      }
   }
   match ix.isa {
      #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
      Isa::X86_64 { .. } => CodePatch::at(ix, func.start, &[0xC3]),
      Isa::Aarch64 => CodePatch::at(ix, func.start, &ARM64_RET),
      Isa::Thumb2 => Err("Desktop Phoenix has no Thumb-2 build".into()),
   }
}

/// The idle test that hides the Spotify activity while you are idle.
///
/// The presence builder takes the user's state as its second argument (`idle:
/// bool` at +0, `idle_since: u64` at +8) and whether Spotify is shown as its
/// sixth. It keeps Spotify activities only when `idle_since == 0 && shown`,
/// computed as `test since, since; sete r8; and r8, shown`, with `since` loaded
/// from `[state + 8]` and `shown` in `r9b` on System V or the second stack
/// argument on Win64. Turning the `test` (`85`) into `cmp` (`39`) on the same
/// register sets ZF, so the flag is `shown` alone and presence keeps its
/// `since`.
pub fn idle_activity(ix: &Index<'_>) -> Found<CodePatch> {
   match ix.isa {
      #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
      Isa::X86_64 { win64 } => {
         let sites = ix
            .image
            .scan(Pattern::new("?? 85 ?? 0F 94 ??"))
            .into_iter()
            .filter(|&site| x86::is_idle_flag(ix, site, win64))
            .collect::<Vec<_>>();
         let site = crate::one("spotify idle flag", &sites)?;
         CodePatch::at(ix, site + 1, &[0x39])
      },
      Isa::Aarch64 => Err("the spotify idle flag is not located on arm64 yet".into()),
      Isa::Thumb2 => Err("Desktop Phoenix has no Thumb-2 build".into()),
   }
}

#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
mod x86 {
   use iced_x86::{
      Decoder,
      DecoderOptions,
      Instruction,
      Mnemonic,
      OpKind,
      Register,
   };

   use crate::index::{
      Index,
      Op,
   };

   // Above the return address, the 32-byte shadow space and the fifth argument.
   const WIN64_SIXTH: u64 = 0x30;
   const IDLE: isize = 0;
   const SINCE: isize = 8;

   fn decode<const N: usize>(ix: &Index<'_>, at: usize) -> Option<[Instruction; N]> {
      const LONGEST: usize = 15;
      let code = ix.image.read(at, N * LONGEST)?;
      let mut decoder = Decoder::with_ip(64, code, at as u64, DecoderOptions::NONE);
      let insns = std::array::from_fn(|_| decoder.decode());
      insns.iter().all(|insn| !insn.is_invalid()).then_some(insns)
   }

   fn number(reg: Register) -> Option<u8> {
      u8::try_from(reg.full_register().number()).ok()
   }

   pub(super) fn is_idle_flag(ix: &Index<'_>, site: usize, win64: bool) -> bool {
      let Some([test, set, and]) = decode::<3>(ix, site) else {
         return false;
      };
      let since = test.op0_register();
      let flag = set.op0_register();
      let shape = test.mnemonic() == Mnemonic::Test
         && since.is_gpr64()
         && test.op1_kind() == OpKind::Register
         && test.op1_register() == since
         && set.mnemonic() == Mnemonic::Sete
         && flag.is_gpr8()
         && and.mnemonic() == Mnemonic::And
         && and.op0_kind() == OpKind::Register
         && and.op0_register() == flag;
      if !shape {
         return false;
      }
      let Some(func) = ix.function(site) else {
         return false;
      };
      let sixth = if win64 {
         and.op1_kind() == OpKind::Memory
            && and.memory_base() == Register::RSP
            && and.memory_index() == Register::None
            && frame_below_return(ix, func.start).is_some_and(|frame| {
               and.memory_displacement64() == frame + WIN64_SIXTH
            })
      } else {
         and.op1_kind() == OpKind::Register
            && and.op1_register().is_gpr8()
            && number(and.op1_register()) == ix.isa.arg(5)
      };
      let (Some(state), Some(since)) = (ix.isa.arg(1), number(since)) else {
         return false;
      };
      let steps = func.steps();
      let before = steps.iter().take_while(|step| step.at < site);
      let reads_idle = before.clone().any(|step| {
         step.op
            == Op::CmpMem {
               base:   state,
               offset: IDLE,
               size:   1,
               value:  0,
            }
      });
      let loads_since = before.clone().any(|step| {
         step.op
            == Op::Load {
               reg:    since,
               base:   state,
               offset: SINCE,
               size:   8,
            }
      });
      sixth && reads_idle && loads_since
   }

   fn frame_below_return(ix: &Index<'_>, start: usize) -> Option<u64> {
      let mut pushed = 0;
      for insn in decode::<12>(ix, start)? {
         match (insn.mnemonic(), insn.op0_kind(), insn.op1_kind()) {
            (Mnemonic::Push, OpKind::Register, _) => pushed += 8,
            (Mnemonic::Sub, OpKind::Register, OpKind::Immediate8to64 | OpKind::Immediate32to64)
               if insn.op0_register() == Register::RSP =>
            {
               return Some(pushed + insn.immediate(1));
            },
            _ => return None,
         }
      }
      None
   }
}
