//! The `Workspace` channel switch `ForwardDialog` makes after forwarding to a
//! single destination, for `phoenix.ui.disableForwardFollow`.

use crate::{
   Found,
   index::{
      Func,
      Index,
      Op,
   },
};

/// The switch, found from the functions that panic in `forward.rs` through
/// the small helper that checks the destination list holds one entry.
///
/// macOS moves the helper's call into a shim that tail-calls the helper, so
/// callees of those shims count too. The switch must have the helper as its
/// only caller, so skipping it never skips any other channel switch.
pub fn forward_follow(ix: &Index<'_>) -> Found<usize> {
   let mut switches = Vec::new();
   for dialog in ix.source("chat/dialogs/forward.rs").iter() {
      for callee in dialog.calls() {
         let Ok(func) = ix.func(callee) else {
            continue;
         };
         let shimmed = if func.size() <= 32 {
            func.calls()
         } else {
            Vec::new()
         };
         for helper in std::iter::once(callee).chain(shimmed) {
            if let Some(switch) = ix
               .func(helper)
               .ok()
               .and_then(|helper| one_destination(ix, &helper))
            {
               switches.push(switch);
            }
         }
      }
   }
   switches.sort_unstable();
   switches.dedup();
   let [switch] = switches.as_slice() else {
      return Err(format!(
         "expected 1 forward channel switch from forward.rs, found {}",
         switches.len()
      ));
   };
   let callers = ix.func(*switch)?.callers().iter().count();
   if callers != 1 {
      return Err(format!(
         "the forward channel switch {switch:#x} has {callers} callers, expected 1"
      ));
   }
   Ok(*switch)
}

/// What `helper` calls when it reads `if list.len == 1 { switch(list[0]) }`
/// over a `Vec` in its first argument: the length compared with 1, and the
/// pointer one word below it loaded.
fn one_destination(ix: &Index<'_>, helper: &Func<'_>) -> Option<usize> {
   let calls = helper.calls();
   let [switch] = calls.as_slice() else {
      return None;
   };
   if helper.size() > 128 {
      return None;
   }
   let this = ix.isa.arg(0)?;
   let steps = helper.steps();
   let len = steps.iter().enumerate().find_map(|(at, step)| {
      match step.op {
         Op::CmpMem {
            base,
            offset,
            size: 8,
            value: 1,
         } if base == this => Some(offset),
         Op::Load {
            reg,
            base,
            offset,
            size: 8,
         } if base == this => {
            let next = steps.get(at + 1)?;
            matches!(next.op, Op::Cmp { reg: compared, value: 1 } if compared == reg)
               .then_some(offset)
         },
         _ => None,
      }
   })?;
   let loads_ptr = steps.iter().any(|step| {
      matches!(step.op, Op::Load { base, offset, size: 8, .. } if base == this && offset == len - 8)
   });
   loads_ptr.then_some(*switch)
}
