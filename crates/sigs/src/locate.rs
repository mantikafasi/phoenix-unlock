//! Locators written once per source tree on [`Index`] queries. Each runs on
//! every binary of its [`Family`], whatever the ISA.

#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod call_idle;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod message;
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod settings;
pub mod spotify;
pub mod staff;

#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
pub mod gpui;

use crate::{
   Found,
   index::{
      Func,
      Index,
      Op,
      Step,
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

pub mod mobile;

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
      name:     "spotify auto pause",
      families: DESKTOP,
      run:      |ix| spotify::auto_pause(ix).map(|patch| format!("{patch:x?}")),
   },
   Locator {
      name:     "spotify idle activity",
      families: DESKTOP,
      run:      |ix| spotify::idle_activity(ix).map(|patch| format!("{patch:x?}")),
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "gpui builders",
      families: DESKTOP,
      run:      |ix| gpui::gpui(ix).map(|found| format!("{found:x?}")),
   },
   Locator {
      name:     "staff flag",
      families: ALL,
      run:      |ix| staff::staff(ix).map(|staff| staff.to_string()),
   },
   Locator {
      name:     "emoji check",
      families: ALL,
      run:      |ix| {
         emoji_check(ix)
            .map(|check| format!("{:#x}, intention +{:#x}", check.func, check.intention))
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "message row",
      families: DESKTOP,
      run:      |ix| message::message_row(ix).map(|row| format!("{row:x?}")),
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "message body",
      families: DESKTOP,
      run:      |ix| {
         message::message_row(ix)
            .and_then(|row| message::message_body(ix, row.func))
            .map(|body| format!("{body:x?}"))
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "settings label",
      families: DESKTOP,
      run:      |ix| {
         settings::settings_label(ix).map(|label| {
            format!(
               "{:#x} (id {:#x}, suffix {:#x})",
               label.name, label.id, label.suffix
            )
         })
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "settings visibility",
      families: DESKTOP,
      run:      |ix| {
         settings::settings_visibility(ix).map(|guards| {
            guards
               .iter()
               .map(|guard| format!("{:#x} (imm8 {:#x})", guard.cmp, guard.imm8()))
               .collect::<Vec<_>>()
               .join(", ")
         })
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "settings section",
      families: DESKTOP,
      run:      |ix| {
         settings::settings_section(ix)
            .map(|section| format!("{:#x} (page +{:#x})", section.func, section.page_offset))
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "settings sidebar rows",
      families: DESKTOP,
      run:      |ix| {
         let section = settings::settings_section(ix)?;
         let rows = settings::settings_sidebar_rows(ix, section.page_offset)?;
         Ok(format!(
            "{rows:x?}, hooks {}, {}, {}",
            hookable(ix, rows.sidebar)?,
            hookable(ix, rows.selected).or_else(|err| {
               rows
                  .selected_via
                  .map(|slot| format!("through {slot:#x}"))
                  .ok_or(err)
            })?,
            hookable(ix, rows.push)?
         ))
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "settings page changed",
      families: DESKTOP,
      run:      |ix| {
         settings::settings_section(ix)
            .and_then(|section| settings::settings_page_changed(ix, section.page_offset))
            .and_then(|func| hookable(ix, func))
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "optimistic delete",
      families: DESKTOP,
      run:      |ix| {
         settings::optimistic_delete_of(ix).map(|found| {
            let deletes = found.deletes.iter().map(|func| format!("{func:#x}"));
            format!(
               "{} (sibling {:#x})",
               deletes.collect::<Vec<_>>().join(", "),
               found.sibling
            )
         })
      },
   },
   #[cfg(any(target_arch = "x86_64", target_os = "macos"))]
   Locator {
      name:     "call idle",
      families: DESKTOP,
      run:      |ix| {
         let dm = call_idle::dm_idle(ix)?;
         let afk = call_idle::guild_afk(ix)?;
         Ok(format!(
            "dm {}, afk move {}, afk disconnect {}",
            hookable(ix, dm)?,
            hookable(ix, afk.move_poll)?,
            hookable(ix, afk.disconnect_poll)?
         ))
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
   let opens = |step: &Step| matches!(step.op, Op::Load { base, size: 1, .. } if base == context);
   // Rejecting on the first instruction keeps the scan of every function to one decode each.
   let checks = ix
      .heads(1, |head| head.first().is_some_and(opens))
      .filter(|func| {
         let head = func.head(HEAD);
         let ops = head.iter().skip(1).map(|step| step.op);
         ops.clone().any(|op| matches!(op, Op::Cmp { value: 14, .. }))
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

/// `func` with the whole instructions an x86-64 entry hook displaces there.
/// arm64 hooks always displace four.
#[cfg(any(target_arch = "x86_64", target_os = "macos"))]
fn hookable(ix: &Index<'_>, func: usize) -> Found<String> {
   match ix.isa {
      crate::index::Isa::X86_64 { .. } => {
         crate::desktop::entry_moved(ix.image, func)
            .map(|moved| format!("{func:#x} ({moved} bytes moved)"))
      },
      _ => Ok(format!("{func:#x}")),
   }
}
