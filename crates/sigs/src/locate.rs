//! Locators written once per source tree on [`Index`] queries. Each runs on
//! every binary of its [`Family`], whatever the ISA.

use crate::{
   Found,
   index::{
      Func,
      Index,
      Op,
   },
};

/// Binaries built from one Rust tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
   /// `libdiscord/desktop`: Windows, Linux and macOS Desktop Phoenix.
   Desktop,
   /// `libdiscord/crates` behind the wasm host: Android and iOS.
   Mobile,
}

const DESKTOP: &[Family] = &[Family::Desktop];
const ALL: &[Family] = &[Family::Desktop, Family::Mobile];

/// A locator, the families it reads, and what it found as text.
pub struct Locator {
   pub name:     &'static str,
   pub families: &'static [Family],
   pub run:      for<'ix> fn(&'ix Index<'ix>) -> Found<String>,
}

pub const LOCATORS: &[Locator] = &[
   Locator {
      name:     "access gate",
      families: DESKTOP,
      run:      |ix| access_gate(ix).map(|func| format!("{func:?}")),
   },
   Locator {
      name:     "media fetch",
      families: DESKTOP,
      run:      |ix| media_fetch(ix).map(|func| format!("{func:?}")),
   },
   Locator {
      name:     "emoji check",
      families: ALL,
      run:      |ix| {
         emoji_check(ix)
            .map(|check| format!("{:#x}, intention +{:#x}", check.func, check.intention))
      },
   },
];

/// The `ClientAccessGate` read the runtime makes answer "no gate". The
/// blocked page calls the `signed_in && access(..) != 2` stub, whose first
/// call is the read, and then the read itself.
pub fn access_gate<'ix>(ix: &'ix Index<'ix>) -> Found<Func<'ix>> {
   const STUB: usize = 64;

   let page = ix
      .literal(b"desktop_access_gate_blocked")
      .one("access gate page")?;
   let reads = page
      .calls()
      .windows(2)
      .filter_map(|pair| {
         let stub = ix.func(pair[0]).ok()?;
         (stub.size() <= STUB && stub.calls().first() == Some(&pair[1]))
            .then(|| ix.func(pair[1]).ok())
            .flatten()
      })
      .collect::<Vec<Func<'ix>>>();

   crate::one("access gate read", &reads)
}

/// `libdiscord_media::fetch`, the function that enters its tracing span.
pub fn media_fetch<'ix>(ix: &'ix Index<'ix>) -> Found<Func<'ix>> {
   ix.tracing("libdiscord_media::fetch")
      .one("libdiscord_media::fetch")
}

/// `getEmojiUnavailableReason(&Emoji, &EmojiContext)`, a leaf that answers 3
/// for profiles before anything else.
#[derive(Clone, Copy, Debug)]
pub struct EmojiCheck {
   pub func:      usize,
   /// `EmojiContext.intention`, 3 (`CHAT`) for the message box.
   pub intention: usize,
}

/// Every build opens it `intention = ctx.intention; if intention | 2 == 14
/// { return 3 }`, a byte load off the second argument, a compare with 14, a
/// branch and a return of 3 within 8 instructions.
pub fn emoji_check(ix: &Index<'_>) -> Found<EmojiCheck> {
   const HEAD: usize = 8;
   let context = ix.isa.arg(1).ok_or("no second argument register")?;
   let checks = ix.heads(HEAD, |head| {
      let [first, rest @ ..] = head else {
         return false;
      };
      let ops = rest.iter().map(|step| step.op);
      matches!(first.op, Op::Load { base, size: 1, .. } if base == context)
         && ops
            .clone()
            .any(|op| matches!(op, Op::Cmp { value: 14, .. }))
         && ops.clone().any(|op| matches!(op, Op::Imm { value: 3, .. }))
         && ops.clone().any(|op| op == Op::Ret)
         && ops.clone().any(|op| matches!(op, Op::Branch(_)))
   });
   let func = checks.one("emoji check")?;
   let Some(Op::Load { offset, .. }) = func.steps().first().map(|step| step.op) else {
      return Err(format!("emoji check at {func:?} reads no context byte"));
   };
   Ok(EmojiCheck {
      func:      func.start,
      intention: usize::try_from(offset)
         .map_err(|_| format!("emoji check at {func:?} reads a negative offset"))?,
   })
}
