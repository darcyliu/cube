//! Enough of an x86 decoder to emulate MMIO. The fault GPA comes from the VMCS.
//! OVMF checks the firmware-volume header with CMP and MOVZX, not only MOV.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegOp {
    pub index: u8,
    pub width: u8,
    /// AH/CH/DH/BH (no REX, byte register 4..=7).
    pub high_byte: bool,
}

impl RegOp {
    /// AH/CH/DH/BH are the high bytes of RAX/RCX/RDX/RBX, not RSP/RBP/RSI/RDI.
    pub fn gpr_index(self) -> u8 {
        if self.high_byte {
            self.index - 4
        } else {
            self.index
        }
    }
}

pub fn reg_bits(full: u64, reg: RegOp) -> u64 {
    let value = if reg.high_byte { full >> 8 } else { full };
    value & width_mask(reg.width)
}

pub fn merge_reg(old: u64, reg: RegOp, value: u64) -> u64 {
    if reg.width == 4 {
        value & 0xffff_ffff
    } else if reg.width == 8 {
        value
    } else if reg.high_byte {
        (old & !0xff00) | ((value & 0xff) << 8)
    } else if reg.width == 1 {
        (old & !0xff) | (value & 0xff)
    } else {
        (old & !0xffff) | (value & 0xffff)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSrc {
    Reg(RegOp),
    Imm { value: u64, width: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluRhs {
    Imm(u64),
    Reg(RegOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSeg {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrKind {
    Movs,
    Cmps,
    Stos,
    Lods,
    Scas,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringOp {
    pub kind: StrKind,
    pub width: u8,
    pub rep: bool,
    pub repne: bool,
    pub seg: MemSeg,
    pub addr_override: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioOp {
    Load {
        dest: RegOp,
    },
    Store {
        src: StoreSrc,
    },
    /// `digit` is ADD, OR, ADC, SBB, AND, SUB, XOR, CMP. CMP and TEST do not store.
    Alu {
        digit: u8,
        width: u8,
        rhs: AluRhs,
        /// GPR is the left-hand side. Otherwise the MMIO operand is.
        reg_dest: bool,
        write: bool,
    },
    /// Zero- or sign-extend an MMIO load into `dest`.
    Extend {
        dest: RegOp,
        src_width: u8,
        sign: bool,
    },
    String(StringOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedMmio {
    pub len: usize,
    pub op: MmioOp,
}

pub fn decode_mmio(bytes: &[u8], addr16: bool) -> Result<DecodedMmio, String> {
    decode_mmio_bits(bytes, if addr16 { 16 } else { 32 })
}

/// `default_bits` is the address size without `0x67` (16, 32, or 64).
pub fn decode_mmio_bits(bytes: &[u8], default_bits: u32) -> Result<DecodedMmio, String> {
    let mut i = 0;
    let mut operand_16 = false;
    let mut addr_override = false;
    let mut rex = 0u8;
    let mut saw_rex = false;
    let mut rep_prefix = 0u8;
    let mut seg = MemSeg::Ds;
    while i < bytes.len() {
        match bytes[i] {
            0x66 => operand_16 = true,
            0x67 => addr_override = true,
            0xF0 => {}
            0xF2 | 0xF3 => rep_prefix = bytes[i],
            0x26 => seg = MemSeg::Es,
            0x2E => seg = MemSeg::Cs,
            0x36 => seg = MemSeg::Ss,
            0x3E => seg = MemSeg::Ds,
            0x64 => seg = MemSeg::Fs,
            0x65 => seg = MemSeg::Gs,
            0x40..=0x4F => {
                rex = bytes[i];
                saw_rex = true;
            }
            _ => break,
        }
        i += 1;
    }
    if i >= bytes.len() {
        return Err("truncated instruction".into());
    }
    let opcode = bytes[i];
    i += 1;
    let rex_w = rex & 0x8 != 0;
    let rex_r = rex & 0x4 != 0;

    // 16-bit ModRM uses disp16 where this decoder reads an immediate. Reject it.
    // `0x67` flips 16 and 32. In long mode it selects 32, which shares this ModRM.
    let string_op = matches!(
        opcode,
        0xA4 | 0xA5 | 0xA6 | 0xA7 | 0xAA | 0xAB | 0xAC | 0xAD | 0xAE | 0xAF
    );
    let effective_16 = match default_bits {
        16 => !addr_override,
        32 => addr_override,
        _ => false,
    };
    if effective_16 && !string_op {
        return Err("16-bit address size is not an MMIO operand".into());
    }

    if opcode == 0x0F {
        if i >= bytes.len() {
            return Err("truncated opcode".into());
        }
        let op2 = bytes[i];
        i += 1;
        let (src_width, sign) = match op2 {
            0xB6 => (1, false),
            0xB7 => (2, false),
            0xBE => (1, true),
            0xBF => (2, true),
            other => return Err(format!("not an MMIO opcode 0x0f {other:#x}")),
        };
        let (reg, _) = eat_modrm(bytes, &mut i, rex_r, false)?;
        let dest_width: u8 = if rex_w {
            8
        } else if operand_16 {
            2
        } else {
            4
        };
        return Ok(DecodedMmio {
            len: i,
            op: MmioOp::Extend {
                dest: RegOp {
                    index: reg,
                    width: dest_width,
                    high_byte: false,
                },
                src_width,
                sign,
            },
        });
    }

    if matches!(
        opcode,
        0xA4 | 0xA5 | 0xA6 | 0xA7 | 0xAA | 0xAB | 0xAC | 0xAD | 0xAE | 0xAF
    ) {
        let byte_op = opcode & 1 == 0;
        let width = if byte_op {
            1
        } else if rex_w {
            8
        } else if operand_16 {
            2
        } else {
            4
        };
        let kind = match opcode {
            0xA4 | 0xA5 => StrKind::Movs,
            0xA6 | 0xA7 => StrKind::Cmps,
            0xAA | 0xAB => StrKind::Stos,
            0xAC | 0xAD => StrKind::Lods,
            _ => StrKind::Scas,
        };
        return Ok(DecodedMmio {
            len: i,
            op: MmioOp::String(StringOp {
                kind,
                width,
                rep: rep_prefix != 0,
                repne: rep_prefix == 0xF2,
                seg,
                addr_override,
            }),
        });
    }

    let class = match opcode {
        0x88 => Class::Mov {
            is_load: false,
            is_imm: false,
            byte_op: true,
        },
        0x89 => Class::Mov {
            is_load: false,
            is_imm: false,
            byte_op: false,
        },
        0x8A => Class::Mov {
            is_load: true,
            is_imm: false,
            byte_op: true,
        },
        0x8B => Class::Mov {
            is_load: true,
            is_imm: false,
            byte_op: false,
        },
        0xC6 => Class::Mov {
            is_load: false,
            is_imm: true,
            byte_op: true,
        },
        0xC7 => Class::Mov {
            is_load: false,
            is_imm: true,
            byte_op: false,
        },
        0x63 => Class::ExtendDword,
        0x80 => Class::AluImm {
            byte_op: true,
            sx8: false,
        },
        0x81 => Class::AluImm {
            byte_op: false,
            sx8: false,
        },
        0x83 => Class::AluImm {
            byte_op: false,
            sx8: true,
        },
        0x84 | 0x85 => Class::AluReg {
            digit: 4,
            byte_op: opcode == 0x84,
            reg_dest: false,
            write: false,
        },
        0x00..=0x3B if opcode & 7 <= 3 => Class::AluReg {
            digit: opcode >> 3,
            byte_op: opcode & 1 == 0,
            reg_dest: opcode & 2 != 0,
            write: opcode >> 3 != 7,
        },
        other => return Err(format!("not an MMIO opcode {other:#x}")),
    };

    let digit_from_modrm = matches!(
        class,
        Class::AluImm { .. } | Class::Mov { is_imm: true, .. }
    );
    let (reg, digit) = eat_modrm(bytes, &mut i, rex_r, digit_from_modrm)?;
    if matches!(class, Class::Mov { is_imm: true, .. }) && digit != 0 {
        return Err("MOV imm /r is not /0".into());
    }

    let (byte_op, op_width_full) = match class {
        Class::Mov { byte_op, .. }
        | Class::AluImm { byte_op, .. }
        | Class::AluReg { byte_op, .. } => (byte_op, !byte_op),
        Class::ExtendDword => (false, true),
    };
    let width: u8 = if byte_op {
        1
    } else if rex_w && op_width_full {
        8
    } else if operand_16 {
        2
    } else {
        4
    };
    let reg_op = RegOp {
        index: reg,
        width,
        high_byte: byte_op && !saw_rex && reg >= 4,
    };

    match class {
        Class::ExtendDword => Ok(DecodedMmio {
            len: i,
            op: MmioOp::Extend {
                dest: RegOp {
                    index: reg,
                    width: if rex_w { 8 } else { 4 },
                    high_byte: false,
                },
                src_width: 4,
                sign: true,
            },
        }),
        Class::Mov {
            is_load, is_imm, ..
        } => {
            if is_load {
                return Ok(DecodedMmio {
                    len: i,
                    op: MmioOp::Load { dest: reg_op },
                });
            }
            if is_imm {
                let imm = read_imm(
                    bytes,
                    &mut i,
                    if byte_op {
                        ImmKind::Imm8
                    } else {
                        ImmKind::ImmOp
                    },
                    width,
                    operand_16,
                    rex_w,
                )?;
                return Ok(DecodedMmio {
                    len: i,
                    op: MmioOp::Store {
                        src: StoreSrc::Imm { value: imm, width },
                    },
                });
            }
            Ok(DecodedMmio {
                len: i,
                op: MmioOp::Store {
                    src: StoreSrc::Reg(reg_op),
                },
            })
        }
        Class::AluImm { byte_op, sx8 } => {
            let kind = if byte_op || sx8 {
                ImmKind::Imm8
            } else {
                ImmKind::ImmOp
            };
            let mut imm = read_imm(bytes, &mut i, kind, width, operand_16, rex_w)?;
            if sx8 {
                imm = sign_extend_imm(imm, 8, width);
            }
            Ok(DecodedMmio {
                len: i,
                op: MmioOp::Alu {
                    digit,
                    width,
                    rhs: AluRhs::Imm(imm),
                    reg_dest: false,
                    write: digit != 7,
                },
            })
        }
        Class::AluReg {
            digit,
            reg_dest,
            write,
            ..
        } => Ok(DecodedMmio {
            len: i,
            op: MmioOp::Alu {
                digit,
                width,
                rhs: AluRhs::Reg(reg_op),
                reg_dest,
                write,
            },
        }),
    }
}

enum Class {
    Mov {
        is_load: bool,
        is_imm: bool,
        byte_op: bool,
    },
    AluImm {
        byte_op: bool,
        sx8: bool,
    },
    AluReg {
        digit: u8,
        byte_op: bool,
        reg_dest: bool,
        write: bool,
    },
    ExtendDword,
}

enum ImmKind {
    Imm8,
    ImmOp,
}

fn eat_modrm(
    bytes: &[u8],
    i: &mut usize,
    rex_r: bool,
    digit_only: bool,
) -> Result<(u8, u8), String> {
    if *i >= bytes.len() {
        return Err("truncated ModRM".into());
    }
    let modrm = bytes[*i];
    *i += 1;
    let mod_bits = modrm >> 6;
    let digit = (modrm >> 3) & 7;
    let reg = digit | if rex_r && !digit_only { 8 } else { 0 };
    if mod_bits == 3 {
        return Err("register form is not MMIO".into());
    }
    let mut sib_base_5 = false;
    if (modrm & 7) == 4 {
        if *i >= bytes.len() {
            return Err("truncated SIB".into());
        }
        let sib = bytes[*i];
        *i += 1;
        sib_base_5 = sib & 7 == 5;
    }
    let disp_len = match mod_bits {
        0 if (modrm & 7) == 5 => 4,
        0 if (modrm & 7) == 4 && sib_base_5 => 4,
        0 => 0,
        1 => 1,
        2 => 4,
        _ => 0,
    };
    if *i + disp_len > bytes.len() {
        return Err("truncated displacement".into());
    }
    *i += disp_len;
    Ok((reg, digit))
}

fn read_imm(
    bytes: &[u8],
    i: &mut usize,
    kind: ImmKind,
    width: u8,
    operand_16: bool,
    rex_w: bool,
) -> Result<u64, String> {
    let imm_len = match kind {
        ImmKind::Imm8 => 1,
        ImmKind::ImmOp if operand_16 && !rex_w => 2,
        ImmKind::ImmOp => 4,
    };
    if *i + imm_len > bytes.len() {
        return Err("truncated immediate".into());
    }
    let mut raw = 0u64;
    for b in 0..imm_len {
        raw |= u64::from(bytes[*i + b]) << (8 * b);
    }
    *i += imm_len;
    if imm_len == 4 && (rex_w || width == 8) {
        raw = raw as u32 as i32 as i64 as u64;
    }
    Ok(raw & width_mask(width))
}

fn sign_extend_imm(raw: u64, imm_bits: u32, width: u8) -> u64 {
    let shift = 64 - imm_bits;
    let signed = ((raw << shift) as i64 >> shift) as u64;
    signed & width_mask(width)
}

fn width_mask(width: u8) -> u64 {
    match width {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => u64::MAX,
    }
}

/// Status flags for a group ALU. `digit` 7 is CMP. Width is 1, 2, 4, or 8.
pub fn apply_alu(digit: u8, width: u8, dst: u64, src: u64, rflags: u64) -> (u64, u64) {
    let bits = u32::from(width) * 8;
    let mask = width_mask(width);
    let a = dst & mask;
    let b = src & mask;
    let cf_in = rflags & 1 != 0;
    let sign = 1u64 << (bits - 1);
    let (result, cf, of, af) = match digit {
        0 | 2 => {
            let cin = if digit == 2 && cf_in { 1u128 } else { 0 };
            let sum = u128::from(a) + u128::from(b) + cin;
            let result = (sum as u64) & mask;
            let cf = sum > u128::from(mask);
            let of = (a ^ result) & (b ^ result) & sign != 0;
            let af = (a ^ b ^ result) & 0x10 != 0;
            (result, cf, of, af)
        }
        3 | 5 | 7 => {
            let bin = if digit == 3 && cf_in { 1u128 } else { 0 };
            let cf = u128::from(b) + bin > u128::from(a);
            let result = a.wrapping_sub(b).wrapping_sub(bin as u64) & mask;
            let of = (a ^ b) & (a ^ result) & sign != 0;
            let af = (a ^ b ^ result) & 0x10 != 0;
            (result, cf, of, af)
        }
        1 => (a | b, false, false, false),
        4 => (a & b, false, false, false),
        6 => (a ^ b, false, false, false),
        _ => (a, false, false, false),
    };
    let pf = (result as u8).count_ones() % 2 == 0;
    let zf = result == 0;
    let sf = result & sign != 0;
    (result, merge_flags(rflags, cf, pf, af, zf, sf, of))
}

pub fn extend_bits(value: u64, src_width: u8, dest_width: u8, sign: bool) -> u64 {
    let masked = value & width_mask(src_width);
    let extended = if sign {
        let shift = 64 - u32::from(src_width) * 8;
        ((masked << shift) as i64 >> shift) as u64
    } else {
        masked
    };
    extended & width_mask(dest_width)
}

/// `0x67` before the opcode. Other prefixes are skipped; ModRM is not decoded.
pub fn has_addr_override(bytes: &[u8]) -> bool {
    for &byte in bytes {
        match byte {
            0x67 => return true,
            0x66 | 0xF0 | 0xF2 | 0xF3 | 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x40..=0x4F => {}
            _ => return false,
        }
    }
    false
}

/// Advance a string index by one element. `mask` is the address-size mask.
pub fn string_step(index: u64, width: u8, df: bool, mask: u64) -> u64 {
    let step = if df {
        0u64.wrapping_sub(u64::from(width))
    } else {
        u64::from(width)
    };
    let new_low = (index & mask).wrapping_add(step) & mask;
    (index & !mask) | new_low
}

fn merge_flags(old: u64, cf: bool, pf: bool, af: bool, zf: bool, sf: bool, of: bool) -> u64 {
    const STATUS: u64 = (1 << 0) | (1 << 2) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 11);
    let mut flags = (old & !STATUS) | 2;
    if cf {
        flags |= 1;
    }
    if pf {
        flags |= 1 << 2;
    }
    if af {
        flags |= 1 << 4;
    }
    if zf {
        flags |= 1 << 6;
    }
    if sf {
        flags |= 1 << 7;
    }
    if of {
        flags |= 1 << 11;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_eax_indirect() {
        // mov %eax, (%rdx)
        let store = decode_mmio(&[0x89, 0x02], false).unwrap();
        assert_eq!(store.len, 2);
        assert_eq!(
            store.op,
            MmioOp::Store {
                src: StoreSrc::Reg(RegOp {
                    index: 0,
                    width: 4,
                    high_byte: false,
                }),
            }
        );
        // mov (%rdx), %eax
        let load = decode_mmio(&[0x8B, 0x02], false).unwrap();
        assert_eq!(
            load.op,
            MmioOp::Load {
                dest: RegOp {
                    index: 0,
                    width: 4,
                    high_byte: false,
                },
            }
        );
        // mov $imm32, (%rdx)
        let imm = decode_mmio(&[0xC7, 0x02, 0x78, 0x56, 0x34, 0x12], false).unwrap();
        assert_eq!(imm.len, 6);
        assert_eq!(
            imm.op,
            MmioOp::Store {
                src: StoreSrc::Imm {
                    value: 0x1234_5678,
                    width: 4,
                },
            }
        );
    }

    #[test]
    fn decodes_fv_header_compares_without_a_store() {
        // cmp byte [rdi+0x37], 2
        let rev = decode_mmio(&[0x80, 0x7F, 0x37, 0x02], false).unwrap();
        assert_eq!(rev.len, 4);
        assert_eq!(
            rev.op,
            MmioOp::Alu {
                digit: 7,
                width: 1,
                rhs: AluRhs::Imm(2),
                reg_dest: false,
                write: false,
            }
        );
        // cmp dword [rdi+0x28], '_FVH'
        let sig = decode_mmio(&[0x81, 0x7F, 0x28, 0x5F, 0x46, 0x56, 0x48], false).unwrap();
        assert_eq!(sig.len, 7);
        assert_eq!(
            sig.op,
            MmioOp::Alu {
                digit: 7,
                width: 4,
                rhs: AluRhs::Imm(0x4856_465F),
                reg_dest: false,
                write: false,
            }
        );
        // add dword [rdi], -1  (0x83 sign-extends)
        let add = decode_mmio(&[0x83, 0x07, 0xFF], false).unwrap();
        assert_eq!(
            add.op,
            MmioOp::Alu {
                digit: 0,
                width: 4,
                rhs: AluRhs::Imm(0xffff_ffff),
                reg_dest: false,
                write: true,
            }
        );
        // movzx r13d, word [rdi+0x30]
        let zx = decode_mmio(&[0x44, 0x0F, 0xB7, 0x6F, 0x30], false).unwrap();
        assert_eq!(zx.len, 5);
        assert_eq!(
            zx.op,
            MmioOp::Extend {
                dest: RegOp {
                    index: 13,
                    width: 4,
                    high_byte: false,
                },
                src_width: 2,
                sign: false,
            }
        );
    }

    #[test]
    fn cmp_of_matching_revision_sets_zf_and_does_not_borrow() {
        let (result, flags) = apply_alu(7, 1, 0x02, 0x02, 0x10002);
        assert_eq!(result, 0);
        assert_eq!(flags & 1, 0, "cf");
        assert_ne!(flags & (1 << 6), 0, "zf");
        assert_ne!(flags & (1 << 16), 0, "rf");
        let (result, flags) = apply_alu(7, 1, 0x00, 0x02, 2);
        assert_eq!(result, 0xFE);
        assert_ne!(flags & 1, 0, "cf");
        assert_eq!(flags & (1 << 6), 0, "zf");
        assert_ne!(flags & 2, 0);
        let (sum, flags) = apply_alu(0, 1, 0x7F, 1, 2);
        assert_eq!(sum, 0x80);
        assert_ne!(flags & (1 << 11), 0, "of");
        assert_eq!(flags & 1, 0, "cf");
        let (diff, flags) = apply_alu(3, 1, 0x00, 0x00, 0x10003);
        assert_eq!(diff, 0xFF);
        assert_ne!(flags & 1, 0, "sbb cf");
        assert_ne!(flags & (1 << 16), 0, "rf kept");
        assert_eq!(extend_bits(0x80, 1, 4, false), 0x80);
        assert_eq!(extend_bits(0x80, 1, 8, true), 0xffff_ffff_ffff_ff80);
        assert_eq!(extend_bits(0x4856, 2, 4, false), 0x4856);
    }

    #[test]
    fn decodes_rep_movsq_and_steps_the_index() {
        let op = decode_mmio(&[0xF3, 0x48, 0xA5], false).unwrap();
        assert_eq!(op.len, 3);
        assert_eq!(
            op.op,
            MmioOp::String(StringOp {
                kind: StrKind::Movs,
                width: 8,
                rep: true,
                repne: false,
                seg: MemSeg::Ds,
                addr_override: false,
            })
        );
        assert_eq!(string_step(0, 8, false, u64::MAX), 8);
        assert_eq!(string_step(8, 8, true, u64::MAX), 0);
        assert_eq!(string_step(0, 8, true, u64::MAX), 0u64.wrapping_sub(8));
    }

    #[test]
    fn ah_is_the_high_byte_of_rax() {
        // mov ah, [rdi]
        let load = decode_mmio(&[0x8A, 0x27], false).unwrap();
        let MmioOp::Load { dest } = load.op else {
            panic!("load");
        };
        assert!(dest.high_byte);
        assert_eq!(dest.index, 4);
        assert_eq!(dest.gpr_index(), 0);
        let merged = merge_reg(0x1111_2222_3333_4455, dest, 0xAB);
        assert_eq!(merged, 0x1111_2222_3333_AB55);
        assert_eq!(reg_bits(merged, dest), 0xAB);
        // mov [rdi], ah
        let store = decode_mmio(&[0x88, 0x27], false).unwrap();
        let MmioOp::Store {
            src: StoreSrc::Reg(reg),
        } = store.op
        else {
            panic!("store");
        };
        assert!(reg.high_byte);
        assert_eq!(reg.gpr_index(), 0);
        assert_eq!(reg_bits(0xAB00, reg), 0xAB);
    }

    #[test]
    fn sixteen_bit_address_does_not_store_the_displacement() {
        // mov byte [disp16], imm8. The imm is 0xAB, not the disp low byte.
        assert!(decode_mmio(&[0xC6, 0x06, 0x34, 0x12, 0xAB], true).is_err());
        let wide = decode_mmio(&[0x67, 0xC6, 0x06, 0xAB], true).unwrap();
        assert_eq!(
            wide.op,
            MmioOp::Store {
                src: StoreSrc::Imm {
                    value: 0xAB,
                    width: 1,
                },
            }
        );
        // 32-bit default (addr16 = false): 0x67 selects disp16, so 0x34 is not the imm.
        assert!(decode_mmio(&[0x67, 0xC6, 0x06, 0x34, 0x12, 0xAB], false).is_err());
        // Long mode: 0x67 is a 32-bit address. The immediate is the next byte.
        let long = decode_mmio_bits(&[0x67, 0xC6, 0x06, 0xAB], 64).unwrap();
        assert_eq!(
            long.op,
            MmioOp::Store {
                src: StoreSrc::Imm {
                    value: 0xAB,
                    width: 1,
                },
            }
        );
        assert!(has_addr_override(&[0xF3, 0x67, 0x6C]));
        assert!(!has_addr_override(&[0xF3, 0x6C]));
    }
}
