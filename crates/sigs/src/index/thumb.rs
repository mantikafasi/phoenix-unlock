//! The Thumb-2 forms armeabi-v7a Phoenix uses for calls, branches, field
//! accesses and position-independent addresses.
//!
//! PIC code builds an address as `movw`/`movt` or a literal-pool load of an
//! offset, then `add rN, pc`. The decoder keeps the last constant each
//! register got and turns that `add` into [`Op::Addr`].

use phoenix_hook::image::Image;

use crate::index::op::{
   Native,
   Op,
   PAIR_REACH,
   Reg,
   Step,
   Value,
};

/// `sp`, `r13`.
const SP: Reg = 13;

const fn sext(value: u32, bits: u32) -> i32 {
   let shift = 32 - bits;
   (value << shift).cast_signed() >> shift
}

/// `ThumbExpandImm` of the 12-bit `i:imm3:imm8`.
const fn expand(imm12: u32) -> u32 {
   let imm8 = imm12 & 0xFF;
   if imm12 >> 10 == 0 {
      match (imm12 >> 8) & 3 {
         0 => imm8,
         1 => imm8 << 16 | imm8,
         2 => imm8 << 24 | imm8 << 8,
         _ => imm8 * 0x0101_0101,
      }
   } else {
      (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7)
   }
}

/// The constant each register last got, and the step that set it.
struct Constants {
   values: [(usize, u32); 16],
   step:   usize,
}

impl Constants {
   const fn set(&mut self, reg: u32, value: u32) {
      self.values[reg as usize & 15] = (self.step, value);
   }

   fn get(&self, reg: u32) -> Option<u32> {
      let (set, value) = self.values[reg as usize & 15];
      (set != usize::MAX && self.step - set <= PAIR_REACH).then_some(value)
   }
}

#[allow(
   clippy::too_many_lines,
   clippy::cast_possible_truncation,
   clippy::cast_possible_wrap,
   clippy::cast_sign_loss,
   reason = "each field is masked to its encoded width before the cast"
)]
pub fn decode(image: &Image<'_>, code: &[u8], start: usize) -> Vec<Step> {
   let halves = code
      .as_chunks::<2>()
      .0
      .iter()
      .map(|&half| u16::from_le_bytes(half))
      .collect::<Vec<u16>>();
   let mut constants = Constants {
      values: [(usize::MAX, 0); 16],
      step:   0,
   };
   let mut steps = Vec::with_capacity(halves.len());
   let mut index = 0;
   while index < halves.len() {
      let at = start + index * 2;
      let pc = at as u32 + 4;
      let hw1 = u32::from(halves[index]);
      let wide = matches!(hw1 >> 11, 0b11101..=0b11111);
      if wide && index + 1 >= halves.len() {
         break;
      }
      constants.step = steps.len();
      let mut writeback = None;
      let (op, native) = if wide {
         let hw2 = u32::from(halves[index + 1]);
         (
            wide_op(image, hw1, hw2, pc, &mut constants, &mut writeback),
            hw1 << 16 | hw2,
         )
      } else {
         (
            narrow_op(image, hw1, pc, &mut constants, &mut writeback),
            hw1,
         )
      };
      steps.push(Step {
         at,
         len: if wide { 4 } else { 2 },
         op,
         writeback,
         writes: 0,
         native: Native::Thumb(native),
      });
      index += if wide { 2 } else { 1 };
   }
   steps
}

#[allow(
   clippy::too_many_lines,
   clippy::cast_possible_truncation,
   clippy::cast_possible_wrap,
   clippy::cast_sign_loss,
   reason = "each field is masked to its encoded width before the cast"
)]
fn narrow_op(
   image: &Image<'_>,
   hw: u32,
   pc: u32,
   constants: &mut Constants,
   writeback: &mut Option<(Reg, isize)>,
) -> Op {
   let low3 = |shift: u32| ((hw >> shift) & 7) as Reg;
   let imm5 = (hw >> 6) & 0x1F;
   let load = |scale: u32, size: u8| {
      Op::Load {
         reg: low3(0),
         base: low3(3),
         offset: (imm5 * scale) as isize,
         size,
      }
   };
   let store = |scale: u32, size: u8| {
      Op::Store {
         value: Value::Reg(low3(0)),
         base: low3(3),
         offset: (imm5 * scale) as isize,
         size,
      }
   };
   let pushed = (hw & 0xFF).count_ones() + ((hw >> 8) & 1);
   match hw {
      0x4770 => Op::Ret,
      _ if hw & 0xFF00 == 0xBD00 => Op::Ret,
      _ if hw & 0xFE00 == 0xB400 => {
         *writeback = Some((SP, -4 * pushed as isize));
         Op::Other
      },
      _ if hw & 0xFE00 == 0xBC00 => {
         *writeback = Some((SP, 4 * pushed as isize));
         Op::Other
      },
      _ if hw & 0xFF00 == 0xB000 => {
         let imm = ((hw & 0x7F) * 4) as isize;
         Op::Offset {
            reg:    SP,
            base:   SP,
            offset: if hw & 0x80 == 0 { imm } else { -imm },
         }
      },
      _ if hw & 0xF000 == 0xD000 && (hw >> 8) & 0xE != 0xE => {
         Op::Branch(pc.wrapping_add_signed(sext((hw & 0xFF) << 1, 9)) as usize)
      },
      _ if hw & 0xF800 == 0xE000 => {
         Op::Jump(pc.wrapping_add_signed(sext((hw & 0x7FF) << 1, 12)) as usize)
      },
      _ if hw & 0xF500 == 0xB100 => {
         let imm = ((hw >> 9) & 1) << 6 | ((hw >> 3) & 0x1F) << 1;
         Op::Branch(pc.wrapping_add(imm) as usize)
      },
      _ if hw & 0xF800 == 0x2000 => {
         let (reg, value) = ((hw >> 8) & 7, hw & 0xFF);
         constants.set(reg, value);
         Op::Imm {
            reg:   reg as Reg,
            value: u64::from(value),
         }
      },
      _ if hw & 0xF800 == 0x2800 => {
         Op::Cmp {
            reg:   low3(8),
            value: u64::from(hw & 0xFF),
         }
      },
      _ if hw & 0xFF78 == 0x4478 => {
         let reg = ((hw >> 4) & 8) | (hw & 7);
         constants.get(reg).map_or(Op::Other, |offset| {
            Op::Addr {
               reg:    reg as Reg,
               target: offset.wrapping_add(pc) as usize,
            }
         })
      },
      _ if hw & 0xFF00 == 0x4400 => {
         let reg = ((hw >> 4) & 8) | (hw & 7);
         constants.get((hw >> 3) & 0xF).map_or(Op::Other, |offset| {
            Op::Offset {
               reg:    reg as Reg,
               base:   reg as Reg,
               offset: offset as isize,
            }
         })
      },
      _ if hw & 0xFF00 == 0x4600 => {
         Op::Move {
            dst: (((hw >> 4) & 8) | (hw & 7)) as Reg,
            src: ((hw >> 3) & 0xF) as Reg,
         }
      },
      _ if hw & 0xF800 == 0x4800 => {
         let target = (pc & !3) + (hw & 0xFF) * 4;
         if let Some(value) = image.read_u32(target as usize) {
            constants.set((hw >> 8) & 7, value);
         }
         Op::Global {
            target: target as usize,
            store:  false,
         }
      },
      _ if hw & 0xF800 == 0x6800 => load(4, 4),
      _ if hw & 0xF800 == 0x7800 => load(1, 1),
      _ if hw & 0xF800 == 0x8800 => load(2, 2),
      _ if hw & 0xF800 == 0x6000 => store(4, 4),
      _ if hw & 0xF800 == 0x7000 => store(1, 1),
      _ if hw & 0xF800 == 0x8000 => store(2, 2),
      _ if hw & 0xF800 == 0x9000 => {
         Op::Store {
            value:  Value::Reg(((hw >> 8) & 7) as Reg),
            base:   SP,
            offset: ((hw & 0xFF) * 4) as isize,
            size:   4,
         }
      },
      _ if hw & 0xF800 == 0x9800 => {
         Op::Load {
            reg:    ((hw >> 8) & 7) as Reg,
            base:   13,
            offset: ((hw & 0xFF) * 4) as isize,
            size:   4,
         }
      },
      _ => Op::Other,
   }
}

#[allow(
   clippy::too_many_lines,
   clippy::cast_possible_truncation,
   clippy::cast_possible_wrap,
   clippy::cast_sign_loss,
   reason = "each field is masked to its encoded width before the cast"
)]
fn wide_op(
   image: &Image<'_>,
   hw1: u32,
   hw2: u32,
   pc: u32,
   constants: &mut Constants,
   writeback: &mut Option<(Reg, isize)>,
) -> Op {
   let rn = hw1 & 0xF;
   let rt = hw2 >> 12;
   let rd = (hw2 >> 8) & 0xF;
   let branch_imm = |long: bool| {
      let sign = (hw1 >> 10) & 1;
      let (j1, j2) = ((hw2 >> 13) & 1, (hw2 >> 11) & 1);
      if long {
         let (i1, i2) = (!(j1 ^ sign) & 1, !(j2 ^ sign) & 1);
         let imm = sign << 24 | i1 << 23 | i2 << 22 | (hw1 & 0x3FF) << 12 | (hw2 & 0x7FF) << 1;
         sext(imm, 25)
      } else {
         let imm = sign << 20 | j2 << 19 | j1 << 18 | (hw1 & 0x3F) << 12 | (hw2 & 0x7FF) << 1;
         sext(imm, 21)
      }
   };
   let imm16 = (hw1 & 0xF) << 12 | ((hw1 >> 10) & 1) << 11 | ((hw2 >> 12) & 7) << 8 | (hw2 & 0xFF);
   let imm12 = ((hw1 >> 10) & 1) << 11 | ((hw2 >> 12) & 7) << 8 | (hw2 & 0xFF);
   let mem = |size: u8, store: bool| {
      let offset = (hw2 & 0xFFF) as isize;
      if store {
         Op::Store {
            value: Value::Reg(rt as Reg),
            base: rn as Reg,
            offset,
            size,
         }
      } else {
         Op::Load {
            reg: rt as Reg,
            base: rn as Reg,
            offset,
            size,
         }
      }
   };
   let branches = hw1 & 0xF800 == 0xF000;
   // `P U W` of the doubleword and 8-bit-offset forms: indexed before the
   // access, added, written back.
   let (pair_pre, pair_up, pair_back) = (hw1 & 0x100 != 0, hw1 & 0x80 != 0, hw1 & 0x20 != 0);
   let imm8 = |scale: u32, add: bool| {
      let imm = ((hw2 & 0xFF) * scale) as isize;
      if add { imm } else { -imm }
   };
   let sp_imm = |value: u32, add: bool| {
      let value = value as isize;
      Op::Offset {
         reg:    SP,
         base:   SP,
         offset: if add { value } else { -value },
      }
   };
   match (hw1, hw2) {
      _ if branches && hw2 & 0xD000 == 0xD000 => {
         Op::Call(pc.wrapping_add_signed(branch_imm(true)) as usize)
      },
      _ if branches && hw2 & 0xD001 == 0xC000 => {
         Op::Call((pc & !3).wrapping_add_signed(branch_imm(true)) as usize)
      },
      _ if branches && hw2 & 0xD000 == 0x9000 => {
         Op::Jump(pc.wrapping_add_signed(branch_imm(true)) as usize)
      },
      _ if branches && hw2 & 0xD000 == 0x8000 && (hw1 >> 7) & 7 != 7 => {
         Op::Branch(pc.wrapping_add_signed(branch_imm(false)) as usize)
      },
      _ if hw1 & 0xFBF0 == 0xF240 && hw2 & 0x8000 == 0 => {
         constants.set(rd, imm16);
         Op::Imm {
            reg:   rd as Reg,
            value: u64::from(imm16),
         }
      },
      _ if hw1 & 0xFBF0 == 0xF2C0 && hw2 & 0x8000 == 0 => {
         let value = constants.get(rd).unwrap_or(0) & 0xFFFF | imm16 << 16;
         constants.set(rd, value);
         Op::Imm {
            reg:   rd as Reg,
            value: u64::from(value),
         }
      },
      _ if hw1 & 0xFBEF == 0xF04F && hw2 & 0x8000 == 0 => {
         let value = expand(imm12);
         constants.set(rd, value);
         Op::Imm {
            reg:   rd as Reg,
            value: u64::from(value),
         }
      },
      _ if hw1 & 0xFBF0 == 0xF1B0 && hw2 & 0x8F00 == 0x0F00 => {
         Op::Cmp {
            reg:   rn as Reg,
            value: u64::from(expand(imm12)),
         }
      },
      (0xF85D, 0xFB04) => Op::Ret,
      (0xE8BD, _) if hw2 & 0x8000 != 0 => Op::Ret,
      (0xE92D | 0xE8BD, _) => {
         let moved = 4 * hw2.count_ones() as isize;
         *writeback = Some((SP, if hw1 == 0xE92D { -moved } else { moved }));
         Op::Other
      },
      // `sub`/`add sp, sp, #imm`, the modified immediate and the plain 12-bit
      // forms.
      _ if hw1 & 0xFBEF == 0xF1AD && rd == u32::from(SP) && hw2 & 0x8000 == 0 => {
         sp_imm(expand(imm12), false)
      },
      _ if hw1 & 0xFBEF == 0xF10D && rd == u32::from(SP) && hw2 & 0x8000 == 0 => {
         sp_imm(expand(imm12), true)
      },
      _ if hw1 & 0xFBFF == 0xF2AD && rd == u32::from(SP) && hw2 & 0x8000 == 0 => {
         sp_imm(imm12, false)
      },
      _ if hw1 & 0xFBFF == 0xF20D && rd == u32::from(SP) && hw2 & 0x8000 == 0 => {
         sp_imm(imm12, true)
      },
      // `ldrd`/`strd`, past the exclusive forms that share the encoding.
      _ if hw1 & 0xFE40 == 0xE840 && (pair_pre || pair_back) && rn != 15 => {
         let moved = imm8(4, pair_up);
         if pair_back {
            *writeback = Some((rn as Reg, moved));
         }
         let (base, offset) = (rn as Reg, if pair_pre { moved } else { 0 });
         let regs = [rt as Reg, ((hw2 >> 8) & 0xF) as Reg];
         if hw1 & 0x10 == 0 {
            Op::StorePair {
               values: regs,
               base,
               offset,
               size: 4,
            }
         } else {
            Op::LoadPair {
               regs,
               base,
               offset,
               size: 4,
            }
         }
      },
      _ if hw1 & 0xFF7F == 0xF85F => {
         let offset = hw2 & 0xFFF;
         let base = pc & !3;
         let target = if hw1 & 0x80 != 0 {
            base + offset
         } else {
            base - offset
         };
         if let Some(value) = image.read_u32(target as usize) {
            constants.set(rt, value);
         }
         Op::Global {
            target: target as usize,
            store:  false,
         }
      },
      _ if hw1 & 0xFFE0 == 0xE840 => {
         Op::Atomic {
            base:   rn as Reg,
            offset: 0,
         }
      },
      // `ldr`/`str` with an 8-bit offset, indexed or negative, past the
      // unprivileged `P U` = `1 1` forms.
      _ if hw1 & 0xFF80 == 0xF800
         && hw2 & 0x0800 != 0
         && rn != 15
         && !(hw2 & 0x0600 == 0x0600 && hw2 & 0x0100 == 0) =>
      {
         let size = 1u8 << ((hw1 >> 5) & 3);
         let (indexed, added, written) = (hw2 & 0x400 != 0, hw2 & 0x200 != 0, hw2 & 0x100 != 0);
         let moved = imm8(1, added);
         if written {
            *writeback = Some((rn as Reg, moved));
         }
         let (base, offset) = (rn as Reg, if indexed { moved } else { 0 });
         if hw1 & 0x10 == 0 {
            Op::Store {
               value: Value::Reg(rt as Reg),
               base,
               offset,
               size,
            }
         } else {
            Op::Load {
               reg: rt as Reg,
               base,
               offset,
               size,
            }
         }
      },
      _ if hw1 & 0xFFF0 == 0xF8D0 => mem(4, false),
      _ if hw1 & 0xFFF0 == 0xF890 => mem(1, false),
      _ if hw1 & 0xFFF0 == 0xF8B0 => mem(2, false),
      _ if hw1 & 0xFFF0 == 0xF8C0 => mem(4, true),
      _ if hw1 & 0xFFF0 == 0xF880 => mem(1, true),
      _ if hw1 & 0xFFF0 == 0xF8A0 => mem(2, true),
      _ => Op::Other,
   }
}
