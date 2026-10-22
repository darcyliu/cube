//! Enough of an x86 decoder to emulate LAPIC and I/O APIC MOV accesses.
//! The fault GPA comes from the VMCS; this only names the operand and length.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegOp {
    pub index: u8,
    pub width: u8,
    /// AH/CH/DH/BH (no REX, byte register 4..=7).
    pub high_byte: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSrc {
    Reg(RegOp),
    Imm { value: u64, width: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioOp {
    Load { dest: RegOp },
    Store { src: StoreSrc },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedMmio {
    pub len: usize,
    pub op: MmioOp,
}

pub fn decode_mmio(bytes: &[u8]) -> Result<DecodedMmio, String> {
    let mut i = 0;
    let mut operand_16 = false;
    let mut rex = 0u8;
    let mut saw_rex = false;
    while i < bytes.len() {
        match bytes[i] {
            0x66 => operand_16 = true,
            0x67 | 0xF0 | 0xF2 | 0xF3 | 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 => {}
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

    let (is_load, is_imm, byte_op) = match opcode {
        0x88 => (false, false, true),
        0x89 => (false, false, false),
        0x8A => (true, false, true),
        0x8B => (true, false, false),
        0xC6 => (false, true, true),
        0xC7 => (false, true, false),
        other => return Err(format!("not a MOV MMIO opcode {other:#x}")),
    };
    if i >= bytes.len() {
        return Err("truncated ModRM".into());
    }
    let modrm = bytes[i];
    i += 1;
    let mod_bits = modrm >> 6;
    let reg = ((modrm >> 3) & 7) | if rex_r { 8 } else { 0 };
    if mod_bits == 3 {
        return Err("MOV register form is not MMIO".into());
    }
    if is_imm && (modrm >> 3) & 7 != 0 {
        return Err("MOV imm /r is not /0".into());
    }
    let mut sib_base_5 = false;
    if (modrm & 7) == 4 {
        if i >= bytes.len() {
            return Err("truncated SIB".into());
        }
        let sib = bytes[i];
        i += 1;
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
    if i + disp_len > bytes.len() {
        return Err("truncated displacement".into());
    }
    i += disp_len;

    let width: u8 = if byte_op {
        1
    } else if rex_w {
        8
    } else if operand_16 {
        2
    } else {
        4
    };
    let high_byte = byte_op && !saw_rex && reg >= 4;
    if is_load {
        return Ok(DecodedMmio {
            len: i,
            op: MmioOp::Load {
                dest: RegOp {
                    index: reg,
                    width,
                    high_byte,
                },
            },
        });
    }
    if is_imm {
        let imm_len = if byte_op {
            1
        } else if operand_16 && !rex_w {
            2
        } else {
            4
        };
        if i + imm_len > bytes.len() {
            return Err("truncated immediate".into());
        }
        let mut raw = 0u64;
        for b in 0..imm_len {
            raw |= (bytes[i + b] as u64) << (8 * b);
        }
        i += imm_len;
        if rex_w && imm_len == 4 {
            raw = raw as u32 as i32 as i64 as u64;
        }
        return Ok(DecodedMmio {
            len: i,
            op: MmioOp::Store {
                src: StoreSrc::Imm { value: raw, width },
            },
        });
    }
    Ok(DecodedMmio {
        len: i,
        op: MmioOp::Store {
            src: StoreSrc::Reg(RegOp {
                index: reg,
                width,
                high_byte,
            }),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_eax_indirect() {
        // mov %eax, (%rdx)
        let store = decode_mmio(&[0x89, 0x02]).unwrap();
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
        let load = decode_mmio(&[0x8B, 0x02]).unwrap();
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
        let imm = decode_mmio(&[0xC7, 0x02, 0x78, 0x56, 0x34, 0x12]).unwrap();
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
}
