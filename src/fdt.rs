//! Device Tree (FDT) generator for ARM64 virtual machines.

#![allow(dead_code)]

use vm_fdt::FdtWriter;

const PHANDLE_GIC: u32 = 1;
const PHANDLE_PCLK: u32 = 2;

#[derive(Clone, Copy)]
pub struct VirtioMmioDevice {
    pub base: u64,
    pub size: u64,
    pub irq: u32,
}

#[derive(Clone, Copy)]
pub struct XhciMmioDevice {
    pub base: u64,
    pub size: u64,
    pub irq: u32,
}

pub fn generate_dtb(
    ram_base: u64,
    ram_size: u64,
    bootargs: &str,
    num_cpus: usize,
    virtio_devices: &[VirtioMmioDevice],
    framebuffer: Option<crate::devices::display::FramebufferConfig>,
    xhci: Option<XhciMmioDevice>,
    initrd: Option<(u64, u64)>,
) -> Result<Vec<u8>, String> {
    generate_dtb_ext(
        ram_base, ram_size, bootargs, num_cpus, virtio_devices, framebuffer, xhci, initrd, false,
    )
}

/// Like [`generate_dtb`]. With `single_memory_node` (UEFI boot), the memory node
/// stays one contiguous range because EDK2's QemuVirtMemInfo cannot parse a split
/// one; the framebuffer is excluded from guest RAM via `/reserved-memory`.
#[allow(clippy::too_many_arguments)]
pub fn generate_dtb_ext(
    ram_base: u64,
    ram_size: u64,
    bootargs: &str,
    num_cpus: usize,
    virtio_devices: &[VirtioMmioDevice],
    framebuffer: Option<crate::devices::display::FramebufferConfig>,
    xhci: Option<XhciMmioDevice>,
    initrd: Option<(u64, u64)>,
    single_memory_node: bool,
) -> Result<Vec<u8>, String> {
    let mut fdt = FdtWriter::new().map_err(|e| format!("FDT init error: {:?}", e))?;

    // Root node
    let root = fdt.begin_node("").map_err(|e| format!("FDT root error: {:?}", e))?;
    fdt.property_u32("#address-cells", 2).map_err(|e| e.to_string())?;
    fdt.property_u32("#size-cells", 2).map_err(|e| e.to_string())?;
    fdt.property_string("compatible", "linux,dummy-virt").map_err(|e| e.to_string())?;
    fdt.property_string("model", "linux,dummy-virt").map_err(|e| e.to_string())?;
    fdt.property_u32("interrupt-parent", PHANDLE_GIC).map_err(|e| e.to_string())?;
    fdt.property_null("dma-coherent").map_err(|e| e.to_string())?;

    // chosen node
    let chosen = fdt.begin_node("chosen").map_err(|e| e.to_string())?;
    fdt.property_string("bootargs", bootargs).map_err(|e| e.to_string())?;
    fdt.property_string("stdout-path", "serial0").map_err(|e| e.to_string())?;
    if let Some((start, end)) = initrd {
        fdt.property_u64("linux,initrd-start", start).map_err(|e| e.to_string())?;
        fdt.property_u64("linux,initrd-end", end).map_err(|e| e.to_string())?;
    }
    if let Some(fb) = framebuffer {
        fdt.property_u32("#address-cells", 2).map_err(|e| e.to_string())?;
        fdt.property_u32("#size-cells", 2).map_err(|e| e.to_string())?;
        let fb_node = fdt
            .begin_node(&format!("framebuffer@{:x}", fb.gpa))
            .map_err(|e| e.to_string())?;
        fdt.property_string("compatible", "simple-framebuffer").map_err(|e| e.to_string())?;
        fdt.property_array_u64("reg", &[fb.gpa, fb.reg_size]).map_err(|e| e.to_string())?;
        fdt.property_u32("width", fb.width).map_err(|e| e.to_string())?;
        fdt.property_u32("height", fb.height).map_err(|e| e.to_string())?;
        fdt.property_u32("stride", fb.stride).map_err(|e| e.to_string())?;
        fdt.property_string("format", "a8r8g8b8").map_err(|e| e.to_string())?;
        fdt.property_string("status", "okay").map_err(|e| e.to_string())?;
        fdt.end_node(fb_node).map_err(|e| e.to_string())?;
    }
    fdt.end_node(chosen).map_err(|e| e.to_string())?;

    // aliases node
    let aliases = fdt.begin_node("aliases").map_err(|e| e.to_string())?;
    fdt.property_string("serial0", "/pl011@09000000").map_err(|e| e.to_string())?;
    fdt.end_node(aliases).map_err(|e| e.to_string())?;

    // apb-pclk node
    let pclk = fdt.begin_node("apb-pclk").map_err(|e| e.to_string())?;
    fdt.property_string("compatible", "fixed-clock").map_err(|e| e.to_string())?;
    fdt.property_u32("#clock-cells", 0).map_err(|e| e.to_string())?;
    fdt.property_u32("clock-frequency", 24_000_000).map_err(|e| e.to_string())?;
    fdt.property_string("clock-output-names", "clk24mhz").map_err(|e| e.to_string())?;
    fdt.property_u32("phandle", PHANDLE_PCLK).map_err(|e| e.to_string())?;
    fdt.end_node(pclk).map_err(|e| e.to_string())?;

    // flash@0 node
    let flash = fdt.begin_node("flash@0").map_err(|e| e.to_string())?;
    fdt.property_string("compatible", "cfi-flash").map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &[0x0, 0x4_000_000, 0x4_000_000, 0x4_000_000])
        .map_err(|e| e.to_string())?;
    fdt.property_u32("bank-width", 4).map_err(|e| e.to_string())?;
    fdt.end_node(flash).map_err(|e| e.to_string())?;

    // cpus node
    let cpus_count = num_cpus.max(1);
    let cpus = fdt.begin_node("cpus").map_err(|e| e.to_string())?;
    fdt.property_u32("#address-cells", 1).map_err(|e| e.to_string())?;
    fdt.property_u32("#size-cells", 0).map_err(|e| e.to_string())?;
    for i in 0..cpus_count {
        let cpu = fdt.begin_node(&format!("cpu@{i}")).map_err(|e| e.to_string())?;
        fdt.property_string("device_type", "cpu").map_err(|e| e.to_string())?;
        fdt.property_string("compatible", "arm,arm-v8").map_err(|e| e.to_string())?;
        fdt.property_u32("reg", i as u32).map_err(|e| e.to_string())?;
        fdt.property_string("enable-method", "psci").map_err(|e| e.to_string())?;
        fdt.end_node(cpu).map_err(|e| e.to_string())?;
    }
    fdt.end_node(cpus).map_err(|e| e.to_string())?;

    // psci node
    let psci = fdt.begin_node("psci").map_err(|e| e.to_string())?;
    fdt.property_string_list(
        "compatible",
        vec![
            "arm,psci-1.0".to_string(),
            "arm,psci-0.2".to_string(),
            "arm,psci".to_string(),
        ],
    )
    .map_err(|e| e.to_string())?;
    fdt.property_string("method", "hvc").map_err(|e| e.to_string())?;
    fdt.end_node(psci).map_err(|e| e.to_string())?;

    // memory node
    let memory_ranges: Vec<u64> = if let Some(fb) = framebuffer {
        let end = ram_base.checked_add(ram_size).ok_or("RAM range overflow")?;
        let fb_end = fb.gpa.checked_add(fb.reg_size).ok_or("Framebuffer range overflow")?;
        if fb.gpa < ram_base || fb_end > end {
            return Err("Framebuffer is outside guest RAM".into());
        }
        if single_memory_node {
            vec![ram_base, ram_size]
        } else {
            let mut ranges = Vec::new();
            if fb.gpa > ram_base {
                ranges.push(ram_base);
                ranges.push(fb.gpa - ram_base);
            }
            if fb_end < end {
                ranges.push(fb_end);
                ranges.push(end - fb_end);
            }
            ranges
        }
    } else {
        vec![ram_base, ram_size]
    };
    let memory = fdt.begin_node("memory").map_err(|e| e.to_string())?;
    fdt.property_string("device_type", "memory").map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &memory_ranges).map_err(|e| e.to_string())?;
    fdt.end_node(memory).map_err(|e| e.to_string())?;

    if let (Some(fb), true) = (framebuffer, single_memory_node) {
        let rm = fdt.begin_node("reserved-memory").map_err(|e| e.to_string())?;
        fdt.property_u32("#address-cells", 2).map_err(|e| e.to_string())?;
        fdt.property_u32("#size-cells", 2).map_err(|e| e.to_string())?;
        fdt.property_null("ranges").map_err(|e| e.to_string())?;
        let rm_fb = fdt
            .begin_node(&format!("framebuffer@{:x}", fb.gpa))
            .map_err(|e| e.to_string())?;
        fdt.property_array_u64("reg", &[fb.gpa, fb.reg_size]).map_err(|e| e.to_string())?;
        fdt.property_null("no-map").map_err(|e| e.to_string())?;
        fdt.end_node(rm_fb).map_err(|e| e.to_string())?;
        fdt.end_node(rm).map_err(|e| e.to_string())?;
    }

    // In GICv3, each vCPU has a 128KB (0x20000) Redistributor region (RD_base + SGI_base)
    let redist_size = (cpus_count as u64) * 0x20000;
    let intc = fdt.begin_node("interrupt-controller@08000000").map_err(|e| e.to_string())?;
    fdt.property_string("compatible", "arm,gic-v3").map_err(|e| e.to_string())?;
    fdt.property_u32("#interrupt-cells", 3).map_err(|e| e.to_string())?;
    fdt.property_u32("#address-cells", 2).map_err(|e| e.to_string())?;
    fdt.property_u32("#size-cells", 2).map_err(|e| e.to_string())?;
    fdt.property_null("interrupt-controller").map_err(|e| e.to_string())?;
    fdt.property_u32("#redistributor-regions", 1).map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &[0x08000000, 0x10000, 0x080a0000, redist_size])
        .map_err(|e| e.to_string())?;
    fdt.property_u32("phandle", PHANDLE_GIC).map_err(|e| e.to_string())?;
    fdt.end_node(intc).map_err(|e| e.to_string())?;

    // timer node
    let timer = fdt.begin_node("timer").map_err(|e| e.to_string())?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,armv8-timer".to_string(), "arm,armv7-timer".to_string()],
    )
    .map_err(|e| e.to_string())?;
    fdt.property_array_u32(
        "interrupts",
        &[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4],
    )
    .map_err(|e| e.to_string())?;
    fdt.property_null("always-on").map_err(|e| e.to_string())?;
    fdt.end_node(timer).map_err(|e| e.to_string())?;

    // pl011@09000000 node
    let pl011 = fdt.begin_node("pl011@09000000").map_err(|e| e.to_string())?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,pl011".to_string(), "arm,primecell".to_string()],
    )
    .map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &[0x09000000, 0x1000]).map_err(|e| e.to_string())?;
    fdt.property_u32("interrupt-parent", PHANDLE_GIC).map_err(|e| e.to_string())?;
    fdt.property_array_u32("interrupts", &[0, 1, 4]).map_err(|e| e.to_string())?;
    fdt.property_array_u32("clocks", &[PHANDLE_PCLK, PHANDLE_PCLK]).map_err(|e| e.to_string())?;
    fdt.property_string_list(
        "clock-names",
        vec!["uartclk".to_string(), "apb_pclk".to_string()],
    )
    .map_err(|e| e.to_string())?;
    fdt.end_node(pl011).map_err(|e| e.to_string())?;

    // rtc@09010000 node
    let rtc = fdt.begin_node("rtc@09010000").map_err(|e| e.to_string())?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,pl031".to_string(), "arm,primecell".to_string()],
    )
    .map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &[0x09010000, 0x1000]).map_err(|e| e.to_string())?;
    fdt.property_u32("interrupt-parent", PHANDLE_GIC).map_err(|e| e.to_string())?;
    fdt.property_array_u32("interrupts", &[0, 4, 4]).map_err(|e| e.to_string())?;
    fdt.property_array_u32("clocks", &[PHANDLE_PCLK]).map_err(|e| e.to_string())?;
    fdt.property_string_list("clock-names", vec!["apb_pclk".to_string()]).map_err(|e| e.to_string())?;
    fdt.end_node(rtc).map_err(|e| e.to_string())?;

    // fw-cfg@09020000 node
    let fw_cfg = fdt.begin_node("fw-cfg@09020000").map_err(|e| e.to_string())?;
    fdt.property_string("compatible", "qemu,fw-cfg-mmio").map_err(|e| e.to_string())?;
    fdt.property_array_u64("reg", &[0x09020000, 0x18]).map_err(|e| e.to_string())?;
    fdt.property_null("dma-coherent").map_err(|e| e.to_string())?;
    fdt.end_node(fw_cfg).map_err(|e| e.to_string())?;

    // Virtio MMIO devices
    for dev in virtio_devices {
        let spi_num = dev.irq.saturating_sub(32);
        let virtio_node = fdt
            .begin_node(&format!("virtio_mmio@{:08x}", dev.base))
            .map_err(|e| e.to_string())?;
        fdt.property_string("compatible", "virtio,mmio").map_err(|e| e.to_string())?;
        fdt.property_array_u64("reg", &[dev.base, dev.size]).map_err(|e| e.to_string())?;
        fdt.property_u32("interrupt-parent", PHANDLE_GIC).map_err(|e| e.to_string())?;
        fdt.property_array_u32("interrupts", &[0, spi_num, 4]).map_err(|e| e.to_string())?;
        fdt.property_null("dma-coherent").map_err(|e| e.to_string())?;
        fdt.end_node(virtio_node).map_err(|e| e.to_string())?;
    }

    // XHCI MMIO device
    if let Some(x) = xhci {
        let spi_num = x.irq.saturating_sub(32);
        let xhci_node = fdt
            .begin_node(&format!("usb@{:08x}", x.base))
            .map_err(|e| e.to_string())?;
        fdt.property_string("compatible", "generic-xhci").map_err(|e| e.to_string())?;
        fdt.property_array_u64("reg", &[x.base, x.size]).map_err(|e| e.to_string())?;
        fdt.property_u32("interrupt-parent", PHANDLE_GIC).map_err(|e| e.to_string())?;
        fdt.property_array_u32("interrupts", &[0, spi_num, 4]).map_err(|e| e.to_string())?;
        fdt.property_null("dma-coherent").map_err(|e| e.to_string())?;
        fdt.property_string("status", "okay").map_err(|e| e.to_string())?;
        fdt.end_node(xhci_node).map_err(|e| e.to_string())?;
    }

    // End root node
    fdt.end_node(root).map_err(|e| e.to_string())?;

    fdt.finish().map_err(|e| format!("FDT finish error: {:?}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn disassemble_dtb_if_possible(dtb: &[u8]) -> Option<String> {
        let mut child = Command::new("dtc")
            .args(["-I", "dtb", "-O", "dts", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(dtb).ok()?;
        let output = child.wait_with_output().ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    }

    #[test]
    fn framebuffer_is_excluded_from_guest_ram() {
        let fb = crate::devices::display::FramebufferConfig::default();
        let dtb = generate_dtb(0x40000000, 0x40000000, "", 1, &[], Some(fb), None, None).unwrap();
        // Check standard FDT magic 0xd00dfeed
        assert_eq!(&dtb[0..4], &[0xd0, 0x0d, 0xfe, 0xed]);

        if let Some(dts) = disassemble_dtb_if_possible(&dtb) {
            assert!(dts.contains("reg = <0x00 0x40000000 0x00 0x10000000 0x00 0x50800000 0x00 0x2f800000>"), "{dts}");
            assert!(dts.contains("framebuffer@50000000"));
        }
        assert!(generate_dtb(0x40000000, 0x10000000, "", 1, &[], Some(fb), None, None).is_err());
        let headless = generate_dtb(0x40000000, 0x40000000, "", 1, &[], None, None, None).unwrap();
        assert!(!headless.windows(b"simple-framebuffer".len()).any(|w| w == b"simple-framebuffer"));
    }

    #[test]
    fn test_generate_dtb_with_initrd() {
        let dtb = generate_dtb(
            0x40000000,
            0x40000000,
            "console=ttyAMA0 earlycon",
            1,
            &[],
            None,
            None,
            Some((0x49000000, 0x49800000)),
        )
        .unwrap();
        // Check standard FDT magic 0xd00dfeed
        assert_eq!(&dtb[0..4], &[0xd0, 0x0d, 0xfe, 0xed]);

        if let Some(dts) = disassemble_dtb_if_possible(&dtb) {
            assert!(dts.contains("linux,initrd-start = <0x00 0x49000000>"), "{dts}");
            assert!(dts.contains("linux,initrd-end = <0x00 0x49800000>"), "{dts}");
            assert!(dts.contains("bootargs = \"console=ttyAMA0 earlycon\""), "{dts}");
        }
    }
}

