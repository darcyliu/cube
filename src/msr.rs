//! MSR policy for exits the guest actually executes. Unknown indices are #GP(0).

/// VCNT 8 and fixed ranges supported. Not zero.
pub const MTRRCAP: u64 = 0x108;
/// Fast-strings. Writes are ignored.
pub const MISC_ENABLE: u64 = 1;

const EFER_SCE: u64 = 1 << 0;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;
const EFER_NX: u64 = 1 << 11;
const EFER_WRITABLE: u64 = EFER_SCE | EFER_LME | EFER_NX;

const APIC_BASE_ADDR: u64 = 0xFEE0_0000;
const APIC_BSP: u64 = 1 << 8;
const APIC_X2APIC: u64 = 1 << 10;
const APIC_ENABLE: u64 = 1 << 11;

#[derive(Clone, Debug)]
pub struct MsrBank {
    cpu: u32,
    var_mtrr: [u64; 16],
    mtrr_def: u64,
    mtrr_fix: [u64; 11],
    efer: u64,
    apic_base: u64,
    pat: u64,
    fs_base: u64,
    gs_base: u64,
    kernel_gs: u64,
}

#[derive(Debug)]
pub struct MsrFault;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsrWrite {
    Stored,
    Efer,
    Pat,
    FsBase,
    GsBase,
    KernelGs,
    ApicBase,
    Ignored,
}

impl MsrBank {
    pub fn new(cpu: u32) -> Self {
        let bsp = if cpu == 0 { APIC_BSP } else { 0 };
        Self {
            cpu,
            var_mtrr: [0; 16],
            mtrr_def: 0,
            mtrr_fix: [0; 11],
            efer: 0,
            apic_base: APIC_BASE_ADDR | bsp | APIC_ENABLE,
            pat: 0x0007_0406_0007_0406,
            fs_base: 0,
            gs_base: 0,
            kernel_gs: 0,
        }
    }

    pub fn efer(&self) -> u64 {
        self.efer
    }

    pub fn set_lma(&mut self, on: bool) {
        if on {
            self.efer |= EFER_LMA;
        } else {
            self.efer &= !EFER_LMA;
        }
    }

    pub fn rdmsr(&self, index: u32) -> Result<u64, MsrFault> {
        if is_x2apic(index) {
            return Err(MsrFault);
        }
        Ok(match index {
            // CpuMpPei reads these before microcode load. A #GP deadloops that image.
            0x17 | 0x8B => 0,
            0x1B => self.apic_base,
            0x3A => 0,
            0xFE => MTRRCAP,
            0x1A0 => MISC_ENABLE,
            0x277 => self.pat,
            0x2FF => self.mtrr_def,
            0x200..=0x20F => self.var_mtrr[(index - 0x200) as usize],
            idx if fix_index(idx).is_some() => self.mtrr_fix[fix_index(idx).unwrap()],
            0xC000_0080 => self.efer,
            0xC000_0100 => self.fs_base,
            0xC000_0101 => self.gs_base,
            0xC000_0102 => self.kernel_gs,
            _ => return Err(MsrFault),
        })
    }

    pub fn wrmsr(&mut self, index: u32, value: u64) -> Result<MsrWrite, MsrFault> {
        if is_x2apic(index) {
            return Err(MsrFault);
        }
        match index {
            0x1B => {
                if value & APIC_X2APIC != 0 {
                    return Err(MsrFault);
                }
                let bsp = if self.cpu == 0 { APIC_BSP } else { 0 };
                let enable = value & APIC_ENABLE;
                self.apic_base = APIC_BASE_ADDR | bsp | enable;
                Ok(MsrWrite::ApicBase)
            }
            0x3A | 0x1A0 => Ok(MsrWrite::Ignored),
            0xFE => Err(MsrFault),
            0x277 => {
                self.pat = value;
                Ok(MsrWrite::Pat)
            }
            0x2FF => {
                self.mtrr_def = value;
                Ok(MsrWrite::Stored)
            }
            0x200..=0x20F => {
                self.var_mtrr[(index - 0x200) as usize] = value;
                Ok(MsrWrite::Stored)
            }
            idx if fix_index(idx).is_some() => {
                self.mtrr_fix[fix_index(idx).unwrap()] = value;
                Ok(MsrWrite::Stored)
            }
            0xC000_0080 => {
                // LMA is owned by paging, not by the value the guest writes.
                self.efer = (value & EFER_WRITABLE) | (self.efer & EFER_LMA);
                Ok(MsrWrite::Efer)
            }
            0xC000_0100 => {
                self.fs_base = value;
                Ok(MsrWrite::FsBase)
            }
            0xC000_0101 => {
                self.gs_base = value;
                Ok(MsrWrite::GsBase)
            }
            0xC000_0102 => {
                self.kernel_gs = value;
                Ok(MsrWrite::KernelGs)
            }
            _ => Err(MsrFault),
        }
    }
}

fn is_x2apic(index: u32) -> bool {
    (0x800..=0x8FF).contains(&index)
}

fn fix_index(index: u32) -> Option<usize> {
    match index {
        0x250 => Some(0),
        0x258 => Some(1),
        0x259 => Some(2),
        0x268..=0x26F => Some(3 + (index - 0x268) as usize),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtrrcap_is_not_zero_and_unknown_is_gp() {
        let mut bank = MsrBank::new(0);
        assert_eq!(bank.rdmsr(0xFE).unwrap(), 0x108);
        assert_eq!(bank.rdmsr(0xFE).unwrap() & 0xff, 8);
        assert_ne!(bank.rdmsr(0xFE).unwrap() & (1 << 8), 0);
        assert_eq!(bank.rdmsr(0x17).unwrap(), 0);
        assert_eq!(bank.rdmsr(0x8B).unwrap(), 0);
        assert!(bank.wrmsr(0x17, 1).is_err());
        assert!(bank.wrmsr(0x8B, 1).is_err());
        assert!(bank.rdmsr(0x1234).is_err());
        assert!(bank.wrmsr(0x1234, 0).is_err());
        assert!(bank.rdmsr(0x808).is_err());
        assert!(bank.wrmsr(0x830, 0).is_err());
        assert!(bank.wrmsr(0xFE, 0).is_err());
        bank.wrmsr(0x200, 0x111).unwrap();
        assert_eq!(bank.rdmsr(0x200).unwrap(), 0x111);
        bank.wrmsr(0x2FF, 0x800).unwrap();
        assert_eq!(bank.rdmsr(0x2FF).unwrap(), 0x800);
        assert_eq!(bank.rdmsr(0x3A).unwrap(), 0);
        bank.wrmsr(0x3A, 0x5).unwrap();
        assert_eq!(bank.rdmsr(0x3A).unwrap(), 0);
        assert_eq!(bank.rdmsr(0x1A0).unwrap(), 1);
        bank.wrmsr(0x1A0, 0).unwrap();
        assert_eq!(bank.rdmsr(0x1A0).unwrap(), 1);
    }

    #[test]
    fn apic_base_rejects_x2apic_and_keeps_the_address() {
        let mut bsp = MsrBank::new(0);
        assert_ne!(bsp.rdmsr(0x1B).unwrap() & APIC_BSP, 0);
        assert!(
            bsp.wrmsr(0x1B, APIC_BASE_ADDR | APIC_X2APIC | APIC_ENABLE)
                .is_err()
        );
        bsp.wrmsr(0x1B, 0xDEAD_0000 | APIC_ENABLE).unwrap();
        assert_eq!(
            bsp.rdmsr(0x1B).unwrap(),
            APIC_BASE_ADDR | APIC_BSP | APIC_ENABLE
        );
        let mut ap = MsrBank::new(1);
        assert_eq!(ap.rdmsr(0x1B).unwrap() & APIC_BSP, 0);
        ap.wrmsr(0xC000_0080, EFER_LME | EFER_LMA | EFER_NX | EFER_SCE)
            .unwrap();
        assert_eq!(ap.efer() & EFER_LMA, 0);
        assert_ne!(ap.efer() & EFER_LME, 0);
        ap.set_lma(true);
        assert_ne!(ap.efer() & EFER_LMA, 0);
    }
}
