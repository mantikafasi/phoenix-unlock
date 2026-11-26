//! x86-64 disassembly, and on Windows live entry hooks that report a call's
//! argument registers, for the debug tools.

#[cfg(any(windows, target_os = "linux"))]
use std::ffi::c_void;
use std::ops::Range;

pub use iced_x86::Register;
#[cfg(windows)]
use iced_x86::{
   BlockEncoder,
   BlockEncoderOptions,
   FlowControl,
   InstructionBlock,
};
use iced_x86::{
   Decoder,
   DecoderOptions,
   Formatter as _,
   Instruction,
   IntelFormatter,
};

use crate::image::Image;
#[cfg(windows)]
use crate::{
   Error,
   image::{
      ABSOLUTE_JUMP_LEN,
      absolute_jump,
      alloc_exec,
      win32,
   },
};

/// One decoded instruction.
pub struct Line {
   pub addr:   usize,
   pub len:    usize,
   pub text:   String,
   /// The address a RIP-relative operand or a near branch points at.
   pub target: Option<usize>,
}

/// A decoded address. Only built for 64-bit hosts, where it always fits.
#[expect(clippy::cast_possible_truncation, reason = "x86_64 hosts only")]
const fn addr(value: u64) -> usize {
   value as usize
}

/// Whether `code` at `ip` is whole instructions that run the same from
/// anywhere: no RIP-relative operand and no branch.
#[must_use]
pub fn relocatable(code: &[u8], ip: usize) -> bool {
   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut instruction = Instruction::default();
   while decoder.can_decode() {
      decoder.decode_out(&mut instruction);
      if instruction.is_invalid()
         || instruction.is_ip_rel_memory_operand()
         || instruction.flow_control() != iced_x86::FlowControl::Next
      {
         return false;
      }
   }
   true
}

/// Up to `count` instructions decoded from `code`, which starts at `ip`. Stops
/// early at bytes that do not decode.
#[must_use]
pub fn disasm(code: &[u8], ip: usize, count: usize) -> Vec<Line> {
   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut formatter = IntelFormatter::new();
   let mut instruction = Instruction::default();
   let mut lines = Vec::new();
   let mut offset = 0;
   while lines.len() < count && decoder.can_decode() {
      decoder.decode_out(&mut instruction);
      if instruction.is_invalid() {
         break;
      }
      let mut text = String::new();
      formatter.format(&instruction, &mut text);
      let target = if instruction.is_ip_rel_memory_operand() {
         Some(instruction.ip_rel_memory_address())
      } else if instruction.near_branch_target() != 0 {
         Some(instruction.near_branch_target())
      } else {
         None
      };
      lines.push(Line {
         addr: ip + offset,
         len: instruction.len(),
         text,
         target: target.and_then(|target| usize::try_from(target).ok()),
      });
      offset += instruction.len();
   }
   lines
}

/// The calling convention code in an image follows: Win64 in PE images,
/// System V in ELF ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abi {
   Win64,
   SysV,
}

/// Where an integer argument sits at the `call`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
   Reg(Register),
   /// `[rsp + offset]`.
   Stack(u64),
}

impl Abi {
   #[must_use]
   pub const fn of(image: &Image<'_>) -> Self {
      if image.pdata.is_some() {
         Self::Win64
      } else {
         Self::SysV
      }
   }

   /// The integer argument registers, in order. The hidden return pointer is
   /// the first.
   #[must_use]
   pub const fn arg_regs(self) -> &'static [Register] {
      match self {
         Self::Win64 => &[Register::RCX, Register::RDX, Register::R8, Register::R9],
         Self::SysV => {
            &[
               Register::RDI,
               Register::RSI,
               Register::RDX,
               Register::RCX,
               Register::R8,
               Register::R9,
            ]
         },
      }
   }

   /// The `index`th integer argument register.
   ///
   /// # Panics
   ///
   /// When `index` is 4 or more, past what Win64 passes in registers.
   #[must_use]
   pub const fn arg(self, index: usize) -> Register {
      assert!(
         index < 4,
         "Win64 passes four integer arguments in registers"
      );
      self.arg_regs()[index]
   }

   /// Where the `index`th integer argument sits at the `call`. Win64 keeps a
   /// 32-byte shadow space below the stacked ones.
   #[must_use]
   pub const fn arg_slot(self, index: usize) -> Slot {
      let regs = self.arg_regs();
      if index < regs.len() {
         return Slot::Reg(regs[index]);
      }
      let shadow = match self {
         Self::Win64 => 0x20,
         Self::SysV => 0,
      };
      Slot::Stack(shadow + 8 * (index - regs.len()) as u64)
   }

   /// The registers a call may clobber.
   #[must_use]
   pub const fn volatile(self) -> &'static [Register] {
      match self {
         Self::Win64 => {
            &[
               Register::RAX,
               Register::RCX,
               Register::RDX,
               Register::R8,
               Register::R9,
               Register::R10,
               Register::R11,
            ]
         },
         Self::SysV => {
            &[
               Register::RAX,
               Register::RCX,
               Register::RDX,
               Register::RSI,
               Register::RDI,
               Register::R8,
               Register::R9,
               Register::R10,
               Register::R11,
            ]
         },
      }
   }
}

/// Tracks the constant an instruction loads: `mov reg, imm`, or `push imm;
/// pop reg`, the form size-optimized builds pick for small constants.
#[derive(Default)]
struct Constants {
   pushed: Option<u64>,
}

impl Constants {
   /// The constant `instruction` writes to its first operand.
   fn loaded(&mut self, instruction: &Instruction) -> Option<u64> {
      use iced_x86::{
         Mnemonic,
         OpKind,
      };
      let immediate = |operand: u32| {
         matches!(
            instruction.op_kind(operand),
            OpKind::Immediate8
               | OpKind::Immediate8to32
               | OpKind::Immediate8to64
               | OpKind::Immediate32
               | OpKind::Immediate32to64
               | OpKind::Immediate64
         )
         .then(|| instruction.immediate(operand))
      };
      match instruction.mnemonic() {
         Mnemonic::Push => {
            self.pushed = immediate(0);
            None
         },
         Mnemonic::Pop => self.pushed.take(),
         Mnemonic::Mov => {
            self.pushed = None;
            immediate(1)
         },
         _ => {
            self.pushed = None;
            None
         },
      }
   }
}

/// The `(offset, size)` stores a function makes through its hidden return
/// pointer, the first argument register on entry.
///
/// Decoding stops at the first `ret`, and a `memcpy` into the pointer counts
/// as one store of the copied length. It shows how large the returned
/// struct is and which fields the constructor sets, so a layout baked into a
/// caller can be checked against a new build. Control flow is followed
/// linearly, which suits the straight-line constructors it is for.
#[must_use]
pub fn sret_stores(abi: Abi, code: &[u8], ip: usize) -> Vec<(usize, usize)> {
   use iced_x86::{
      Code,
      InstructionInfoFactory,
      Mnemonic,
      OpAccess,
      OpKind,
   };

   let writes = |access: OpAccess| {
      matches!(
         access,
         OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
      )
   };
   let (dest, len) = (abi.arg(0), abi.arg(2));

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut factory = InstructionInfoFactory::new();
   let mut constants = Constants::default();
   let mut holders = vec![dest];
   let mut len_imm = None;
   let mut stores = Vec::new();
   while decoder.can_decode() {
      let instruction = decoder.decode();
      if instruction.is_invalid() || instruction.mnemonic() == Mnemonic::Ret {
         break;
      }
      if instruction.mnemonic() == Mnemonic::Call {
         if holders.contains(&dest)
            && let Some(len) = len_imm
         {
            stores.push((0, len));
         }
         holders.retain(|register| !abi.volatile().contains(register));
         len_imm = None;
         continue;
      }
      let constant = constants.loaded(&instruction);
      let info = factory.info(&instruction);
      for memory in info.used_memory() {
         if writes(memory.access())
            && holders.contains(&memory.base())
            && memory.index() == Register::None
         {
            stores.push((addr(memory.displacement()), memory.memory_size().size()));
         }
      }
      let copied = matches!(instruction.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
         && instruction.op0_kind() == OpKind::Register
         && instruction.op1_kind() == OpKind::Register
         && holders.contains(&instruction.op1_register());
      let written = info
         .used_registers()
         .iter()
         .filter(|used| writes(used.access()))
         .map(|used| used.register().full_register())
         .collect::<Vec<_>>();
      for register in written {
         holders.retain(|&held| held != register);
         if copied && register == instruction.op0_register() {
            holders.push(register);
         }
         if register == len {
            len_imm = constant.and_then(|value| usize::try_from(value).ok());
         }
      }
   }
   stores
}

/// What a register holds, relative to the pointer [`pointer_uses`] follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
   /// The pointer plus this offset.
   At(usize),
   /// The value read from the pointer at this offset.
   Field(usize),
   /// The address of memory that starts with a copy of the bytes at this
   /// offset.
   CopyOf(usize),
   /// A constant.
   Constant(u64),
}

/// An instruction [`pointer_uses`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerUse {
   /// `movsxd r, [table + index*4]`, a jump table indexed by the field at
   /// this offset.
   Dispatch { at: usize, field: usize },
   /// A call, and what its first four argument registers held.
   Call {
      at:   usize,
      args: [Option<Held>; 4],
   },
   /// `rep movsb`, and what its destination, source and count held.
   Copy {
      at:     usize,
      dest:   Option<Held>,
      source: Option<Held>,
      count:  Option<Held>,
   },
   /// A store of `size` bytes at this offset from the pointer.
   Store {
      at:     usize,
      offset: usize,
      size:   usize,
   },
}

/// The calls, jump tables, block copies and stores in a function, with what
/// each takes from or writes to the pointer `pointer` holds on entry.
///
/// The pointer is followed through register copies, `lea`, stack spills and
/// their reloads, and a field copied through an XMM register to `[r]` makes
/// `r` point at that copy. Calls clobber the ABI's volatile registers and
/// every XMM register. Control flow is followed linearly past `ret` and jumps,
/// so a function's cold blocks count too.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one match arm per instruction shape")]
pub fn pointer_uses(abi: Abi, code: &[u8], ip: usize, pointer: Register) -> Vec<PointerUse> {
   use std::collections::HashMap;

   use iced_x86::{
      InstructionInfoFactory,
      Mnemonic,
      OpAccess,
      OpKind,
   };

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut factory = InstructionInfoFactory::new();
   let mut constants = Constants::default();
   let mut held = HashMap::from([(pointer.full_register(), Held::At(0))]);
   // Stack slots the pointer was spilled to, by base and displacement.
   let mut spilled: HashMap<(Register, u64), Held> = HashMap::new();
   let mut uses = Vec::new();
   while decoder.can_decode() {
      let instruction = decoder.decode();
      if instruction.is_invalid() {
         break;
      }
      let at = addr(instruction.ip());
      let get = |held: &HashMap<Register, Held>, register: Register| {
         held.get(&register.full_register()).copied()
      };
      let memory = (0..instruction.op_count()).any(|op| instruction.op_kind(op) == OpKind::Memory);
      let (base, index, disp) = (
         instruction.memory_base(),
         instruction.memory_index(),
         instruction.memory_displacement64(),
      );
      let simple = memory && index == Register::None && base != Register::None;
      let slot = (simple && matches!(base, Register::RSP | Register::RBP)).then_some((base, disp));
      // The pointer offset the memory operand addresses.
      let field = match get(&held, base) {
         Some(Held::At(offset)) if simple => {
            isize::try_from(disp.cast_signed())
               .ok()
               .and_then(|disp| offset.checked_add_signed(disp))
         },
         _ => None,
      };

      match instruction.mnemonic() {
         Mnemonic::Call => {
            uses.push(PointerUse::Call {
               at,
               args: [0, 1, 2, 3].map(|arg| get(&held, abi.arg(arg))),
            });
            held.retain(|register, _| !abi.volatile().contains(register) && !register.is_zmm());
            constants = Constants::default();
            continue;
         },
         Mnemonic::Movsxd if memory && instruction.memory_index_scale() == 4 => {
            if let Some(Held::Field(field)) = get(&held, index) {
               uses.push(PointerUse::Dispatch { at, field });
            }
         },
         Mnemonic::Movsb if instruction.has_rep_prefix() => {
            uses.push(PointerUse::Copy {
               at,
               dest: get(&held, Register::RDI),
               source: get(&held, Register::RSI),
               count: get(&held, Register::RCX),
            });
         },
         _ => {},
      }

      let moves = matches!(
         instruction.mnemonic(),
         Mnemonic::Mov | Mnemonic::Movups | Mnemonic::Movaps | Mnemonic::Movdqu | Mnemonic::Movdqa
      );
      let constant = constants.loaded(&instruction);
      let source = |op: u32| {
         match instruction.op_kind(op) {
            OpKind::Register => get(&held, instruction.op_register(op)),
            OpKind::Memory => {
               slot
                  .and_then(|slot| spilled.get(&slot).copied())
                  .or_else(|| field.map(Held::Field))
            },
            _ => None,
         }
      };
      let mut written_now = None;
      if instruction.op0_kind() == OpKind::Register {
         let dest = instruction.op0_register().full_register();
         written_now = match instruction.mnemonic() {
            Mnemonic::Lea => field.map(|offset| (dest, Held::At(offset))),
            _ if constant.is_some() => constant.map(|value| (dest, Held::Constant(value))),
            _ if moves => source(1).map(|value| (dest, value)),
            _ => None,
         };
      } else if moves && instruction.op0_kind() == OpKind::Memory {
         let value = source(1);
         if let Some(slot) = slot {
            match value {
               Some(Held::At(offset)) => spilled.insert(slot, Held::At(offset)),
               _ => spilled.remove(&slot),
            };
         } else if let Some(Held::Field(offset)) = value
            && simple
            && disp == 0
            && get(&held, base).is_none()
         {
            written_now = Some((base.full_register(), Held::CopyOf(offset)));
         }
      }

      let info = factory.info(&instruction);
      for used in info.used_memory() {
         let writes = matches!(used.access(), OpAccess::Write | OpAccess::ReadWrite);
         if writes
            && used.index() == Register::None
            && let Some(Held::At(offset)) = get(&held, used.base())
            && let Some(offset) = isize::try_from(used.displacement().cast_signed())
               .ok()
               .and_then(|disp| offset.checked_add_signed(disp))
         {
            uses.push(PointerUse::Store {
               at,
               offset,
               size: used.memory_size().size(),
            });
         }
         if writes && !moves && matches!(used.base(), Register::RSP | Register::RBP) {
            spilled.remove(&(used.base(), used.displacement()));
         }
      }
      for used in info.used_registers() {
         if !matches!(
            used.access(),
            OpAccess::Read | OpAccess::CondRead | OpAccess::None
         ) {
            held.remove(&used.register().full_register());
         }
      }
      if let Some((register, value)) = written_now {
         held.insert(register, value);
      }
   }
   uses
}

/// One `DebugStruct::field(name, &self.field)` call in a derived `Debug::fmt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugField {
   /// Address and length of the field name literal.
   pub name:     usize,
   pub name_len: usize,
   /// Offset of the field in `self`.
   pub offset:   usize,
   /// The `&dyn Debug` vtable passed for the field, whose slot 3 is its `fmt`.
   pub vtable:   Option<usize>,
}

/// The fields a derived `Debug::fmt` prints, in code order.
///
/// Each field is a call `field(builder, name, len, &self.field, vtable)` with
/// its arguments in `abi`'s slots: a `lea` of the name, a constant length,
/// `lea [self + off]` (or `self` after `add self, off`) and a `lea` of the
/// vtable. `self` is the register the first argument is first copied into.
/// Cold blocks are decoded linearly too, so fields printed out of line count.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one match arm per instruction shape")]
pub fn debug_fields(abi: Abi, code: &[u8], ip: usize) -> Vec<DebugField> {
   use std::collections::HashMap;

   use iced_x86::{
      Code,
      Mnemonic,
      OpKind,
   };

   #[derive(Clone, Copy)]
   enum Value {
      Rip(usize),
      Field(usize),
      Imm(u64),
   }

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut constants = Constants::default();
   let mut this = None;
   let mut added = 0usize;
   let mut regs: HashMap<Register, Value> = HashMap::new();
   let mut stack: HashMap<u64, Value> = HashMap::new();
   let mut fields = Vec::new();
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let constant = constants.loaded(&ins);
      let dst = (ins.op_count() > 0 && ins.op0_kind() == OpKind::Register)
         .then(|| ins.op0_register().full_register());
      let to_this = dst.is_some() && dst == this;
      let self_lea = ins.mnemonic() == Mnemonic::Lea
         && Some(ins.memory_base()) == this
         && ins.memory_index() == Register::None;
      // Any other write to `self`'s register means it no longer holds `self`.
      // The epilogue's `pop` is the exception: cold blocks decoded after the
      // `ret` are reached from the body and still have `self` there.
      if to_this
         && !matches!(
            ins.mnemonic(),
            Mnemonic::Add | Mnemonic::Inc | Mnemonic::Pop
         )
         && !self_lea
      {
         this = None;
         added = 0;
      }
      match (ins.mnemonic(), dst) {
         (Mnemonic::Mov, Some(_))
            if this.is_none()
               && matches!(ins.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
               && ins.op1_kind() == OpKind::Register
               && ins.op1_register() == abi.arg(0) =>
         {
            this = dst;
         },
         (Mnemonic::Lea, Some(dst)) => {
            let value = if ins.is_ip_rel_memory_operand() {
               Some(Value::Rip(addr(ins.ip_rel_memory_address())))
            } else if self_lea {
               added
                  .checked_add(addr(ins.memory_displacement64()))
                  .map(Value::Field)
            } else {
               None
            };
            match value {
               Some(Value::Field(offset)) if to_this => added = offset,
               Some(value) => {
                  regs.insert(dst, value);
               },
               None => {
                  regs.remove(&dst);
               },
            }
         },
         (Mnemonic::Add, Some(_)) if to_this => {
            // A register, a negative or an overflowing step loses track of `self`.
            if let Some(sum) = ins
               .try_immediate(1)
               .ok()
               .and_then(|step| i64::try_from(step).ok())
               .and_then(|step| usize::try_from(step).ok())
               .and_then(|step| added.checked_add(step))
            {
               added = sum;
            } else {
               this = None;
               added = 0;
            }
         },
         (Mnemonic::Inc, Some(_)) if to_this => added = added.saturating_add(1),
         (Mnemonic::Mov | Mnemonic::Pop, Some(dst)) => {
            let value = match constant {
               Some(value) => Some(Value::Imm(value)),
               None if ins.op_count() < 2 || ins.op1_kind() != OpKind::Register => None,
               None if Some(ins.op1_register().full_register()) == this => {
                  Some(Value::Field(added))
               },
               None => regs.get(&ins.op1_register().full_register()).copied(),
            };
            match value {
               Some(value) => regs.insert(dst, value),
               None => regs.remove(&dst),
            };
         },
         (Mnemonic::Mov, None)
            if ins.op0_kind() == OpKind::Memory
               && ins.memory_base() == Register::RSP
               && ins.memory_index() == Register::None
               && ins.op1_kind() == OpKind::Register =>
         {
            match regs.get(&ins.op1_register().full_register()) {
               Some(&value) => stack.insert(ins.memory_displacement64(), value),
               None => stack.remove(&ins.memory_displacement64()),
            };
         },
         (Mnemonic::Call, _) => {
            let arg = |index: usize| {
               match abi.arg_slot(index) {
                  Slot::Reg(register) => regs.get(&register),
                  Slot::Stack(offset) => stack.get(&offset),
               }
            };
            if let (Some(Value::Rip(name)), Some(Value::Imm(len)), Some(Value::Field(offset))) =
               (arg(1), arg(2), arg(3))
               && (1..64).contains(len)
            {
               fields.push(DebugField {
                  name:     *name,
                  name_len: addr(*len),
                  offset:   *offset,
                  vtable:   match arg(4) {
                     Some(Value::Rip(at)) => Some(*at),
                     _ => None,
                  },
               });
            }
            // `self` lives in a callee-saved register, so it survives the call.
            regs.clear();
            stack.clear();
         },
         (Mnemonic::Jmp | Mnemonic::Ret, _) => {
            regs.clear();
            stack.clear();
            added = 0;
         },
         _ => {},
      }
   }
   fields
}

/// `(offset, value)` of each `mov byte [..], imm8` into the struct a call
/// just returned through its hidden return pointer.
///
/// `before` ends at the call and holds that pointer's `lea arg0, [base +
/// disp]`; `after` starts right behind it and is read up to the next branch
/// or call, through registers loaded with `lea reg, [base + disp]`.
#[must_use]
pub fn sret_byte_stores(
   abi: Abi,
   before: &[u8],
   before_ip: usize,
   after: &[u8],
   after_ip: usize,
) -> Vec<(usize, u8)> {
   use std::collections::HashMap;

   use iced_x86::{
      FlowControl,
      Mnemonic,
      OpKind,
   };

   let mut out = None;
   let mut decoder = Decoder::with_ip(64, before, before_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let writes_sret = ins.op_count() > 0
         && ins.op0_kind() == OpKind::Register
         && ins.op0_register().full_register() == abi.arg(0);
      if writes_sret {
         out = (ins.mnemonic() == Mnemonic::Lea
            && !ins.is_ip_rel_memory_operand()
            && ins.memory_index() == Register::None)
            .then(|| (ins.memory_base(), ins.memory_displacement64().cast_signed()));
      }
   }
   let Some((base, disp)) = out else {
      return Vec::new();
   };

   let mut aliases: HashMap<Register, i64> = HashMap::from([(base, 0)]);
   let mut stores = Vec::new();
   let mut decoder = Decoder::with_ip(64, after, after_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() || ins.flow_control() != FlowControl::Next {
         break;
      }
      if ins.op_count() > 0 && ins.op0_kind() == OpKind::Register {
         let dst = ins.op0_register().full_register();
         if dst != base {
            aliases.remove(&dst);
            if ins.mnemonic() == Mnemonic::Lea
               && ins.memory_base() == base
               && ins.memory_index() == Register::None
            {
               aliases.insert(dst, ins.memory_displacement64().cast_signed() - disp);
            }
         }
         continue;
      }
      if ins.mnemonic() == Mnemonic::Mov
         && ins.op0_kind() == OpKind::Memory
         && ins.memory_size().size() == 1
         && ins.op1_kind() == OpKind::Immediate8
         && ins.memory_index() == Register::None
         && let Some(&delta) = aliases.get(&ins.memory_base())
      {
         let at = ins.memory_displacement64().cast_signed() + delta
            - if ins.memory_base() == base { disp } else { 0 };
         if let Ok(at) = usize::try_from(at) {
            stores.push((at, ins.immediate8()));
         }
      }
   }
   stores
}

/// The length and callee of the call starting `code`, which sits at `ip`:
/// `call rel32`, or `call [rip + slot]` through a pointer to code, the form
/// ELF builds without a PLT take for most calls.
#[must_use]
pub fn call_target(image: &Image<'_>, code: &[u8], ip: usize) -> Option<(usize, usize)> {
   let rel = |at: usize| {
      let bytes = code.get(at..at + 4)?;
      Some(i32::from_le_bytes(bytes.try_into().ok()?) as isize)
   };
   match code {
      [0xE8, ..] => Some((5, (ip + 5).checked_add_signed(rel(1)?)?)),
      [0xFF, 0x15, ..] => {
         let slot = (ip + 6).checked_add_signed(rel(2)?)?;
         let callee = image
            .read_usize(slot)
            .filter(|&callee| image.is_code(callee))?;
         Some((6, callee))
      },
      _ => None,
   }
}

/// The target of the `jmp rel32` or `jmp [rip + slot]` starting `code`,
/// which sits at `ip`.
#[must_use]
pub fn jump_target(image: &Image<'_>, code: &[u8], ip: usize) -> Option<usize> {
   let rel = |at: usize| {
      let bytes = code.get(at..at + 4)?;
      Some(i32::from_le_bytes(bytes.try_into().ok()?) as isize)
   };
   match code {
      [0xE9, ..] => (ip + 5).checked_add_signed(rel(1)?),
      [0xFF, 0x25, ..] => image.read_usize((ip + 6).checked_add_signed(rel(2)?)?),
      _ => None,
   }
}

/// An instruction that points at an address, and the function holding it.
pub struct CodeRef {
   pub function: usize,
   pub line:     Line,
}

/// The instructions in `image` whose RIP-relative operand or near branch
/// points into `targets`, confirmed by decoding their function.
#[must_use]
pub fn code_refs(image: &Image<'_>, targets: Range<usize>) -> Vec<CodeRef> {
   image
      .rip_refs(targets)
      .into_iter()
      .filter_map(|(field, target)| {
         let (start, end) = image.function_bounds(field)?;
         let line = disasm(image.read(start, end - start)?, start, usize::MAX)
            .into_iter()
            .find(|line| (line.addr..line.addr + line.len).contains(&field))?;
         (line.target == Some(target)).then_some(CodeRef {
            function: start,
            line,
         })
      })
      .collect()
}

/// How far before a match a string literal can start. Rust merges literals
/// into one blob, so code loads the start of the literal, not the match.
const LITERAL_REACH: usize = 512;

/// The code that loads the string literal holding the bytes at `at`: the
/// loads of the closest literal start at or before it.
#[must_use]
pub fn literal_loads(image: &Image<'_>, at: usize) -> Vec<CodeRef> {
   let mut loads = code_refs(image, at.saturating_sub(LITERAL_REACH)..at + 1);
   let start = loads.iter().filter_map(|load| load.line.target).max();
   loads.retain(|load| load.line.target == start);
   loads
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(windows)]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   let mut buffer = vec![0u8; len];
   let mut read = 0usize;
   // SAFETY: `buffer` holds `len` writable bytes, and ReadProcessMemory
   // checks the source range instead of faulting.
   let ok = unsafe {
      win32::ReadProcessMemory(
         win32::GetCurrentProcess(),
         addr as *const c_void,
         buffer.as_mut_ptr().cast(),
         len,
         &raw mut read,
      )
   };
   (ok != 0 || read > 0).then(|| {
      buffer.truncate(read);
      buffer
   })
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(target_os = "linux")]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   let mut buffer = vec![0u8; len];
   let local = libc::iovec {
      iov_base: buffer.as_mut_ptr().cast(),
      iov_len:  len,
   };
   let remote = libc::iovec {
      iov_base: addr as *mut c_void,
      iov_len:  len,
   };
   // SAFETY: `buffer` holds `len` writable bytes, and process_vm_readv checks
   // the source range instead of faulting.
   let read = unsafe {
      libc::process_vm_readv(libc::getpid(), &raw const local, 1, &raw const remote, 1, 0)
   };
   let read = usize::try_from(read).ok().filter(|&read| read > 0)?;
   buffer.truncate(read);
   Some(buffer)
}

/// The registers at a hooked function's entry, as the stub saved them.
#[cfg(windows)]
#[repr(C)]
pub struct Regs {
   pub r9:  u64,
   pub r8:  u64,
   pub rdx: u64,
   pub rcx: u64,
   /// The caller's return address.
   pub ret: u64,
}

#[cfg(windows)]
impl Regs {
   /// The first four integer arguments, `rcx`, `rdx`, `r8` and `r9`.
   #[must_use]
   pub const fn args(&self) -> [u64; 4] {
      [self.rcx, self.rdx, self.r8, self.r9]
   }
}

/// Called on every entry of an observed function, on the caller's thread,
/// with the context passed to [`observe`]. It must not unwind.
#[cfg(windows)]
pub type Observer = extern "C" fn(ctx: usize, regs: &Regs);

/// A function [`observe`] redirects through its stub.
#[cfg(windows)]
pub struct Observed {
   addr:     usize,
   original: [u8; ABSOLUTE_JUMP_LEN],
}

#[cfg(windows)]
const STUB_PAGE: usize = 0x1000;
#[cfg(windows)]
const TRAMPOLINE_AT: usize = 0x100;

#[cfg(windows)]
impl Observed {
   #[must_use]
   pub const fn addr(&self) -> usize {
      self.addr
   }

   /// Puts the function's first bytes back. The stub and trampoline stay
   /// allocated, since a thread may still run them.
   ///
   /// # Safety
   ///
   /// As for [`observe`].
   pub unsafe fn remove(&self, image: &Image<'static>) -> Result<(), Error> {
      // SAFETY: forwarded from the caller.
      unsafe { hot_patch(image, self.addr, &self.original) }
   }
}

/// Makes every call to the function at `addr` run `observer(ctx, regs)` first,
/// then the function unchanged.
///
/// The displaced prologue is decoded and relocated, so unlike
/// [`Image::hook_entry`] it needs no known pattern. The entry is patched while
/// other threads run: one that reaches it meanwhile spins on a `jmp $` until
/// the jump is whole.
///
/// # Safety
///
/// `addr` starts a function of `image` that nothing branches back into within
/// its first 14 bytes, and no thread is paused inside those bytes.
#[cfg(windows)]
pub unsafe fn observe(
   image: &Image<'static>,
   addr: usize,
   observer: Observer,
   ctx: usize,
) -> Result<Observed, Error> {
   let refuse = |message: &str| {
      Error::Attach {
         backend: "observe",
         target:  addr,
         message: message.to_owned(),
      }
   };
   if addr % 8 == 7 {
      return Err(refuse(
         "the entry ends an 8-byte word, so it cannot be parked atomically",
      ));
   }
   if let Some((start, end)) = image.function_bounds(addr) {
      if start != addr {
         return Err(refuse("not a function start"));
      }
      if end - addr < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function is shorter than the 14-byte jump"));
      }
   }
   let code = image.read(addr, 32).ok_or(Error::Unmapped { addr })?;
   let original: [u8; ABSOLUTE_JUMP_LEN] = code[..ABSOLUTE_JUMP_LEN]
      .try_into()
      .map_err(|_| Error::Unmapped { addr })?;
   let mut decoder = Decoder::with_ip(64, code, addr as u64, DecoderOptions::NONE);
   let mut displaced = Vec::new();
   let mut covered = 0;
   while covered < ABSOLUTE_JUMP_LEN {
      let instruction = decoder.decode();
      if instruction.is_invalid() {
         return Err(refuse("the prologue does not decode"));
      }
      covered += instruction.len();
      let ends = matches!(
         instruction.flow_control(),
         FlowControl::Return
            | FlowControl::UnconditionalBranch
            | FlowControl::IndirectBranch
            | FlowControl::Interrupt
      );
      if ends && covered < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function ends within the 14-byte jump"));
      }
      displaced.push(instruction);
   }

   let page = alloc_near(addr).ok_or_else(|| refuse("no free memory within 1.75 GiB"))?;
   let trampoline = page + TRAMPOLINE_AT;
   let mut relocated = BlockEncoder::encode(
      64,
      InstructionBlock::new(&displaced, trampoline as u64),
      BlockEncoderOptions::NONE,
   )
   .map_err(|err| refuse(&format!("relocating the prologue: {err}")))?
   .code_buffer;
   relocated.extend(absolute_jump(addr + covered));
   let stub = stub(observer, ctx, trampoline);
   debug_assert!(stub.len() <= TRAMPOLINE_AT && relocated.len() <= STUB_PAGE - TRAMPOLINE_AT);
   // SAFETY: a fresh page of `STUB_PAGE` bytes, filled before the entry
   // jumps to it.
   unsafe {
      std::ptr::copy_nonoverlapping(stub.as_ptr(), page as *mut u8, stub.len());
      std::ptr::copy_nonoverlapping(relocated.as_ptr(), trampoline as *mut u8, relocated.len());
   }
   // SAFETY: forwarded from the caller.
   unsafe { hot_patch(image, addr, &absolute_jump(page)) }?;
   Ok(Observed { addr, original })
}

/// Writes 14 bytes at a function entry other threads may be calling: first a
/// `jmp $` that parks them, then the tail, then the head.
#[cfg(windows)]
unsafe fn hot_patch(
   image: &Image<'static>,
   addr: usize,
   bytes: &[u8; ABSOLUTE_JUMP_LEN],
) -> Result<(), Error> {
   // SAFETY: `addr % 8 != 7`, so each two-byte write is one atomic word swap,
   // and the tail is only reached through the head.
   unsafe {
      image.write_code(addr, &[0xEB, 0xFE])?;
      image.write_code(addr + 2, &bytes[2..])?;
      image.write_code(addr, &bytes[..2])
   }
}

/// A fresh executable page within reach of RIP-relative operands at `addr`.
#[cfg(windows)]
fn alloc_near(addr: usize) -> Option<usize> {
   const GRANULARITY: usize = 0x1_0000;
   const REACH: usize = 0x7000_0000;
   let home = addr & !(GRANULARITY - 1);
   (1..REACH / GRANULARITY)
      .flat_map(|step| {
         [
            home.checked_sub(step * GRANULARITY),
            home.checked_add(step * GRANULARITY),
         ]
      })
      .flatten()
      .find_map(|candidate| alloc_exec(candidate, STUB_PAGE))
}

/// Saves the argument registers, calls `observer(ctx, &regs)` on a 16-byte
/// aligned stack with its shadow space, restores them and jumps to
/// `trampoline`.
#[cfg(windows)]
fn stub(observer: Observer, ctx: usize, trampoline: usize) -> Vec<u8> {
   let mut code = vec![
      0x51, 0x52, 0x41, 0x50, 0x41, 0x51, // push rcx, rdx, r8, r9
      0x48, 0x83, 0xEC, 0x68, // sub rsp, 0x68
      0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x20, // movdqu [rsp+0x20], xmm0
      0xF3, 0x0F, 0x7F, 0x4C, 0x24, 0x30, // movdqu [rsp+0x30], xmm1
      0xF3, 0x0F, 0x7F, 0x54, 0x24, 0x40, // movdqu [rsp+0x40], xmm2
      0xF3, 0x0F, 0x7F, 0x5C, 0x24, 0x50, // movdqu [rsp+0x50], xmm3
      0x48, 0x8D, 0x54, 0x24, 0x68, // lea rdx, [rsp+0x68]
      0x48, 0xB9, // mov rcx, imm64
   ];
   code.extend(ctx.to_le_bytes());
   code.extend([0x48, 0xB8]); // mov rax, imm64
   code.extend((observer as usize).to_le_bytes());
   code.extend([
      0xFF, 0xD0, // call rax
      0xF3, 0x0F, 0x6F, 0x44, 0x24, 0x20, // movdqu xmm0, [rsp+0x20]
      0xF3, 0x0F, 0x6F, 0x4C, 0x24, 0x30, // movdqu xmm1, [rsp+0x30]
      0xF3, 0x0F, 0x6F, 0x54, 0x24, 0x40, // movdqu xmm2, [rsp+0x40]
      0xF3, 0x0F, 0x6F, 0x5C, 0x24, 0x50, // movdqu xmm3, [rsp+0x50]
      0x48, 0x83, 0xC4, 0x68, // add rsp, 0x68
      0x41, 0x59, 0x41, 0x58, 0x5A, 0x59, // pop r9, r8, rdx, rcx
   ]);
   code.extend(absolute_jump(trampoline));
   code
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn pointer_uses_follow_spills_leas_and_copies() {
      let code = [
         0x48, 0x89, 0x55, 0x10, // mov [rbp+0x10], rdx
         0x48, 0x8B, 0x55, 0x10, // mov rdx, [rbp+0x10]
         0x48, 0x8D, 0x5A, 0x60, // lea rbx, [rdx+0x60]
         0x48, 0x8B, 0x03, // mov rax, [rbx]
         0x48, 0x63, 0x04, 0x81, // movsxd rax, [rcx+rax*4]
         0x48, 0x8B, 0x4D, 0x10, // mov rcx, [rbp+0x10]
         0x0F, 0x10, 0x81, 0xC0, 0x00, 0x00, 0x00, // movups xmm0, [rcx+0xc0]
         0x4C, 0x8D, 0x45, 0x40, // lea r8, [rbp+0x40]
         0x41, 0x0F, 0x29, 0x00, // movaps [r8], xmm0
         0x4C, 0x8D, 0x89, 0x98, 0x00, 0x00, 0x00, // lea r9, [rcx+0x98]
         0xE8, 0x00, 0x00, 0x00, 0x00, // call
         0x6A, 0x60, // push 0x60
         0x59, // pop rcx
         0x48, 0x8B, 0x75, 0x10, // mov rsi, [rbp+0x10]
         0xF3, 0xA4, // rep movsb
      ];
      let uses = pointer_uses(Abi::Win64, &code, 0x1000, Register::RDX);
      assert!(uses.contains(&PointerUse::Dispatch {
         at:    0x100F,
         field: 0x60,
      }));
      assert!(uses.iter().any(|used| {
         matches!(used, PointerUse::Call {
            args: [_, _, Some(Held::CopyOf(0xC0)), Some(Held::At(0x98))],
            ..
         })
      }));
      assert!(uses.iter().any(|used| {
         matches!(used, PointerUse::Copy {
            source: Some(Held::At(0)),
            count: Some(Held::Constant(0x60)),
            ..
         })
      }));
   }

   #[test]
   fn sret_stores_follow_the_return_pointer() {
      let code = [
         0x48, 0x89, 0xC8, // mov rax, rcx
         0x88, 0x90, 0x23, 0x03, 0x00, 0x00, // mov [rax+0x323], dl
         0x48, 0x83, 0xA0, 0x80, 0x00, 0x00, 0x00, 0x00, // and qword ptr [rax+0x80], 0
         0x48, 0x89, 0x51, 0x08, // mov [rcx+8], rdx
         0x48, 0x89, 0xC1, // mov rcx, rax
         0x41, 0xB8, 0x40, 0x00, 0x00, 0x00, // mov r8d, 0x40
         0xE8, 0x00, 0x00, 0x00, 0x00, // call memcpy
         0x48, 0x89, 0x48, 0x10, // mov [rax+0x10], rcx: rax no longer holds it
         0xC3, // ret
         0x48, 0x89, 0x01, // mov [rcx], rax: past the ret
      ];
      assert_eq!(sret_stores(Abi::Win64, &code, 0x1000), [
         (0x323, 1),
         (0x80, 8),
         (8, 8),
         (0, 0x40)
      ]);
   }

   #[test]
   fn debug_fields_follow_sysv_argument_registers() {
      // The head of `<StyleRefinement as Debug>::fmt` in the Linux build: the
      // name in `rsi`, its length in `edx`, the field in `rcx` and the vtable
      // in `r8`.
      let code = [
         0x55, 0x53, 0x48, 0x83, 0xEC, 0x18, 0x48, 0x89, 0xFB, 0x48, 0x8D, 0x15, 0x84, 0x1C, 0x56,
         0xF0, 0x48, 0x8D, 0x7C, 0x24, 0x08, 0xB9, 0x0F, 0x00, 0x00, 0x00, 0xFF, 0x15, 0xA8, 0x02,
         0x95, 0x04, 0x80, 0xBB, 0x77, 0x02, 0x00, 0x00, 0xFF, 0x40, 0x0F, 0x95, 0xC5, 0x74, 0x25,
         0x48, 0x8D, 0x8B, 0x77, 0x02, 0x00, 0x00, 0x48, 0x8D, 0x35, 0x8D, 0x0C, 0x3F, 0xEF, 0x4C,
         0x8D, 0x05, 0x06, 0x51, 0x5C, 0x04, 0x48, 0x8D, 0x7C, 0x24, 0x08, 0xBA, 0x07, 0x00, 0x00,
         0x00, 0xFF, 0x15, 0x6E, 0xAD, 0x9E, 0x04,
      ];
      assert_eq!(debug_fields(Abi::SysV, &code, 0x1546_E250), [DebugField {
         name:     0x485_EF18,
         name_len: 7,
         offset:   0x277,
         vtable:   Some(0x19A3_3398),
      }]);
      assert_eq!(debug_fields(Abi::Win64, &code, 0x1546_E250), []);
   }

   #[cfg(windows)]
   extern "C" fn ignore(_: usize, _: &Regs) {}

   #[cfg(windows)]
   #[test]
   fn stub_decodes_as_written() {
      let stub = stub(ignore, 7, 0x1000);
      // The jump's 8-byte target is data.
      let code = &stub[..stub.len() - 8];
      let text = disasm(code, 0, 64)
         .into_iter()
         .map(|line| line.text)
         .collect::<Vec<_>>();
      assert_eq!(text[..6], [
         "push rcx",
         "push rdx",
         "push r8",
         "push r9",
         "sub rsp,68h",
         "movdqu [rsp+20h],xmm0"
      ]);
      assert!(text.contains(&"lea rdx,[rsp+68h]".to_owned()));
      assert!(text.contains(&"mov rcx,7".to_owned()));
      assert!(text.contains(&"call rax".to_owned()));
      assert_eq!(text[text.len() - 6..text.len() - 1], [
         "add rsp,68h",
         "pop r9",
         "pop r8",
         "pop rdx",
         "pop rcx"
      ]);
      assert!(
         text
            .last()
            .is_some_and(|last| last.starts_with("jmp qword ptr"))
      );
      assert_eq!(
         code.len(),
         disasm(code, 0, 64)
            .iter()
            .map(|line| line.len)
            .sum::<usize>()
      );
   }
}
