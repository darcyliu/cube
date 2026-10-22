//! Synthetic CPUID leaves. No host passthrough and no Hypervisor.framework.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuidRegs {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

const LEAF1_EAX: u32 = 0x0009_06e0;
/// FPU VME DE PSE TSC MSR PAE CX8 APIC SEP MTRR PGE CMOV PAT CLFSH MMX FXSR SSE SSE2.
const LEAF1_EDX: u32 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 8)
    | (1 << 9)
    | (1 << 11)
    | (1 << 12)
    | (1 << 13)
    | (1 << 15)
    | (1 << 16)
    | (1 << 19)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26);
/// SSE3 SSSE3 CX16 SSE4.1 SSE4.2 POPCNT XSAVE. VMX, AVX, and x2APIC stay clear.
const LEAF1_ECX: u32 =
    (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20) | (1 << 23) | (1 << 26) | (1 << 31);

const BRAND: &[u8] = b"vmm-intel x86_64";

/// `cr4_osxsave` is guest CR4 bit 18, not the hardware VMXE bit.
pub fn cpuid(leaf: u32, subleaf: u32, cpu: u32, ncpus: u32, cr4_osxsave: bool) -> CpuidRegs {
    let zero = CpuidRegs {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
    };
    match leaf {
        0 => CpuidRegs {
            eax: 0x16,
            ebx: u32::from_le_bytes(*b"Genu"),
            edx: u32::from_le_bytes(*b"ineI"),
            ecx: u32::from_le_bytes(*b"ntel"),
        },
        1 => {
            let n = ncpus.clamp(1, 255);
            let mut ecx = LEAF1_ECX;
            if cr4_osxsave {
                ecx |= 1 << 27;
            }
            CpuidRegs {
                eax: LEAF1_EAX,
                ebx: (cpu << 24) | (n << 16) | (8 << 8),
                ecx,
                edx: LEAF1_EDX,
            }
        }
        4 | 7 => zero,
        0xB => leaf_b(subleaf, cpu, ncpus),
        0xD => match subleaf {
            0 => CpuidRegs {
                eax: 3,
                ebx: 0x240,
                ecx: 0x240,
                edx: 0,
            },
            _ => zero,
        },
        // Fixed ratio. Do not read the host TSC.
        0x15 => CpuidRegs {
            eax: 1,
            ebx: 1,
            ecx: 0,
            edx: 0,
        },
        0x16 => zero,
        0x4000_0000 => CpuidRegs {
            eax: 0x4000_0001,
            ebx: u32::from_le_bytes(*b"vmm-"),
            ecx: u32::from_le_bytes(*b"inte"),
            edx: u32::from_le_bytes(*b"l\0\0\0"),
        },
        0x4000_0001 => zero,
        0x8000_0000 => CpuidRegs {
            eax: 0x8000_0008,
            ..zero
        },
        0x8000_0001 => CpuidRegs {
            eax: 0,
            ebx: 0,
            ecx: 1,                                 // LAHF/SAHF
            edx: (1 << 11) | (1 << 20) | (1 << 29), // SYSCALL NX LM
        },
        0x8000_0002..=0x8000_0004 => brand_leaf(leaf - 0x8000_0002),
        0x8000_0007 => CpuidRegs {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 1 << 8, // invariant TSC
        },
        0x8000_0008 => CpuidRegs {
            eax: 0x0000_3024, // 36 physical, 48 linear
            ..zero
        },
        _ => zero,
    }
}

/// XCR0 is x87 or x87+SSE. Any other index or value is #GP(0).
pub fn xsetbv_allowed(index: u32, value: u64) -> bool {
    index == 0 && (value == 1 || value == 3)
}

fn leaf_b(subleaf: u32, cpu: u32, ncpus: u32) -> CpuidRegs {
    let edx = cpu;
    match subleaf {
        0 => CpuidRegs {
            eax: 0,
            ebx: 1,
            ecx: 1 << 8,
            edx,
        },
        1 => CpuidRegs {
            eax: 4,
            ebx: ncpus.max(1),
            ecx: (2 << 8) | 1,
            edx,
        },
        n => CpuidRegs {
            eax: 0,
            ebx: 0,
            ecx: n & 0xff,
            edx,
        },
    }
}

fn brand_leaf(index: u32) -> CpuidRegs {
    let mut raw = [0u8; 48];
    let n = BRAND.len().min(raw.len());
    raw[..n].copy_from_slice(&BRAND[..n]);
    let start = (index as usize) * 16;
    let chunk = &raw[start..start + 16];
    CpuidRegs {
        eax: u32::from_le_bytes(chunk[0..4].try_into().unwrap()),
        ebx: u32::from_le_bytes(chunk[4..8].try_into().unwrap()),
        ecx: u32::from_le_bytes(chunk[8..12].try_into().unwrap()),
        edx: u32::from_le_bytes(chunk[12..16].try_into().unwrap()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf1_hides_vmx_avx_and_keeps_cx16() {
        let r = cpuid(1, 0, 2, 4, false);
        assert_eq!(r.eax, 0x0009_06e0);
        assert_eq!(r.ebx & 0xff, 0);
        assert_eq!((r.ebx >> 8) & 0xff, 8);
        assert_eq!((r.ebx >> 16) & 0xff, 4);
        assert_eq!((r.ebx >> 24) & 0xff, 2);
        assert_ne!(r.ecx & (1 << 13), 0);
        assert_eq!(r.ecx & (1 << 5), 0);
        assert_eq!(r.ecx & (1 << 21), 0);
        assert_eq!(r.ecx & (1 << 27), 0);
        assert_eq!(r.ecx & (1 << 28), 0);
        assert_ne!(r.edx & (1 << 9), 0);
        assert_ne!(r.edx & (1 << 19), 0);
        assert_eq!(r.edx, LEAF1_EDX);
        let saved = cpuid(1, 0, 0, 1, true);
        assert_ne!(saved.ecx & (1 << 27), 0);
        assert_eq!(
            cpuid(4, 0, 0, 1, false),
            CpuidRegs {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            }
        );
        assert_eq!(cpuid(7, 0, 0, 1, false).ebx, 0);
    }

    #[test]
    fn leaf_b_is_smt_then_core_without_a_package() {
        let smt = cpuid(0xB, 0, 3, 8, false);
        assert_eq!(smt.eax & 0x1f, 0);
        assert_eq!(smt.ebx & 0xffff, 1);
        assert_eq!(smt.ecx, 1 << 8);
        assert_eq!(smt.edx, 3);
        let core = cpuid(0xB, 1, 3, 8, false);
        assert_eq!(core.eax & 0x1f, 4);
        assert_eq!(core.ebx & 0xffff, 8);
        assert_eq!(core.ecx, (2 << 8) | 1);
        assert_eq!(core.edx, 3);
        let done = cpuid(0xB, 2, 3, 8, false);
        assert_eq!(done.ecx & 0xff00, 0);
        assert_eq!(done.edx, 3);
        assert_ne!((done.ecx >> 8) & 0xff, 1);
        assert_ne!((done.ecx >> 8) & 0xff, 2);
    }

    #[test]
    fn xsave_area_is_x87_sse_only() {
        let r = cpuid(0xD, 0, 0, 1, true);
        assert_eq!(r.eax, 3);
        assert_eq!(r.ebx, 0x240);
        assert_eq!(r.ecx, 0x240);
        assert_eq!(cpuid(0xD, 1, 0, 1, true).eax, 0);
        assert!(xsetbv_allowed(0, 1));
        assert!(xsetbv_allowed(0, 3));
        assert!(!xsetbv_allowed(0, 7));
        assert!(!xsetbv_allowed(1, 1));
    }
}
