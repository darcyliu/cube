//! Device Tree (FDT) generator for ARM64 virtual machines.

#![allow(dead_code)]

use std::io::Write;
use std::process::{Command, Stdio};

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
) -> Result<Vec<u8>, String> {
    let cpus_count = num_cpus.max(1);
    let mut cpus_dts = String::new();
    for i in 0..cpus_count {
        cpus_dts.push_str(&format!(
            r#"        cpu@{i} {{
            device_type = "cpu";
            compatible = "arm,arm-v8";
            reg = <0x{i:x}>;
            enable-method = "psci";
        }};
"#
        ));
    }

    // In GICv3, each vCPU has a 128KB (0x20000) Redistributor region (RD_base + SGI_base)
    let redist_size = (cpus_count as u64) * 0x20000;

    let mut virtio_dts = String::new();
    for dev in virtio_devices {
        let spi_num = dev.irq.saturating_sub(32);
        virtio_dts.push_str(&format!(
            r#"    virtio_mmio@{base:08x} {{
        compatible = "virtio,mmio";
        reg = <0x0 0x{base:08x} 0x0 0x{size:x}>;
        interrupt-parent = <&intc>;
        interrupts = <0 {spi_num} 4>;
        dma-coherent;
    }};
"#,
            base = dev.base,
            size = dev.size,
            spi_num = spi_num,
        ));
    }

    let xhci_dts = if let Some(x) = xhci {
        let spi_num = x.irq.saturating_sub(32);
        format!(
            r#"    usb@{base:08x} {{
        compatible = "generic-xhci";
        reg = <0x0 0x{base:08x} 0x0 0x{size:x}>;
        interrupt-parent = <&intc>;
        interrupts = <0 {spi_num} 4>;
        dma-coherent;
        status = "okay";
    }};
"#,
            base = x.base,
            size = x.size,
            spi_num = spi_num,
        )
    } else {
        String::new()
    };


    // FreeBSD vt_simplefb only probes simple-framebuffer nodes under /chosen
    // (it is a VT early console driver, not an ofwbus leaf driver).
    let framebuffer_dts = if let Some(fb) = framebuffer {
        format!(
            r#"
        #address-cells = <2>;
        #size-cells = <2>;
        framebuffer@{gpa:x} {{
            compatible = "simple-framebuffer";
            reg = <0x0 0x{gpa:08x} 0x0 0x{reg_size:x}>;
            width = <{width}>;
            height = <{height}>;
            stride = <{stride}>;
            format = "a8r8g8b8";
            status = "okay";
        }};
"#,
            gpa = fb.gpa,
            reg_size = fb.reg_size,
            width = fb.width,
            height = fb.height,
            stride = fb.stride,
        )
    } else {
        String::new()
    };

    // Exclude framebuffer backing from allocatable RAM, including kernels that
    // do not consume /reserved-memory. The host still maps the entire region.
    let cells = |base: u64, size: u64| format!(
        "0x{:x} 0x{:x} 0x{:x} 0x{:x}", base >> 32, base as u32, size >> 32, size as u32);
    let memory_reg = if let Some(fb) = framebuffer {
        let end = ram_base.checked_add(ram_size).ok_or("RAM range overflow")?;
        let fb_end = fb.gpa.checked_add(fb.reg_size).ok_or("Framebuffer range overflow")?;
        if fb.gpa < ram_base || fb_end > end {
            return Err("Framebuffer is outside guest RAM".into());
        }
        let mut ranges = Vec::new();
        if fb.gpa > ram_base { ranges.push(cells(ram_base, fb.gpa - ram_base)); }
        if fb_end < end { ranges.push(cells(fb_end, end - fb_end)); }
        ranges.join(" ")
    } else { cells(ram_base, ram_size) };

    let dts = format!(
        r#"/dts-v1/;

/ {{
    #address-cells = <2>;
    #size-cells = <2>;
    compatible = "linux,dummy-virt";
    model = "linux,dummy-virt";
    interrupt-parent = <&intc>;
    dma-coherent;

    chosen {{
        bootargs = "{bootargs}";
        stdout-path = "serial0";
{framebuffer_dts}    }};

    aliases {{
        serial0 = "/pl011@09000000";
    }};

    clocks {{
        apb_pclk: apb-pclk {{
            compatible = "fixed-clock";
            #clock-cells = <0>;
            clock-frequency = <24000000>;
            clock-output-names = "clk24mhz";
        }};
    }};

    flash@0 {{
        compatible = "cfi-flash";
        reg = <0x0 0x0 0x0 0x4000000 0x0 0x4000000 0x0 0x4000000>;
        bank-width = <4>;
    }};

    cpus {{
        #address-cells = <1>;
        #size-cells = <0>;
{cpus_dts}    }};

    psci {{
        compatible = "arm,psci-1.0", "arm,psci-0.2", "arm,psci";
        method = "hvc";
    }};

    memory@{ram_base:x} {{
        device_type = "memory";
        reg = <{memory_reg}>;
    }};

    intc: interrupt-controller@08000000 {{
        compatible = "arm,gic-v3";
        #interrupt-cells = <3>;
        #address-cells = <2>;
        #size-cells = <2>;
        interrupt-controller;
        #redistributor-regions = <1>;
        reg = <0x0 0x08000000 0x0 0x10000>,
              <0x0 0x080a0000 0x0 0x{redist_size:x}>;
    }};

    timer {{
        compatible = "arm,armv8-timer", "arm,armv7-timer";
        interrupts = <1 13 4>,
                     <1 14 4>,
                     <1 11 4>,
                     <1 10 4>;
        always-on;
    }};

    pl011@09000000 {{
        compatible = "arm,pl011", "arm,primecell";
        reg = <0x0 0x09000000 0x0 0x1000>;
        interrupts = <0 1 4>; /* SPI 1 (interrupt ID 33) */
        clocks = <&apb_pclk>, <&apb_pclk>;
        clock-names = "uartclk", "apb_pclk";
    }};

    rtc@09010000 {{
        compatible = "arm,pl031", "arm,primecell";
        reg = <0x0 0x09010000 0x0 0x1000>;
        interrupt-parent = <&intc>;
        interrupts = <0 4 4>; /* SPI 4 (interrupt ID 36) */
    }};

    fw-cfg@09020000 {{
        compatible = "qemu,fw-cfg-mmio";
        reg = <0x0 0x09020000 0x0 0x18>;
        dma-coherent;
    }};

{virtio_dts}{xhci_dts}}};
"#,
        bootargs = bootargs,
        ram_base = ram_base,
        virtio_dts = virtio_dts,
        xhci_dts = xhci_dts,
        framebuffer_dts = framebuffer_dts,
    );

    // Invoke dtc
    let mut child = Command::new("dtc")
        .args(["-I", "dts", "-O", "dtb", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn dtc (device tree compiler): {}. Is dtc installed?", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(dts.as_bytes())
            .map_err(|e| format!("Failed to pipe DTS to dtc: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("dtc compilation failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("dtc returned error: {}", stderr));
    }

    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framebuffer_is_excluded_from_guest_ram() {
        let fb = crate::devices::display::FramebufferConfig::default();
        let dtb = generate_dtb(0x40000000, 0x40000000, "", 1, &[], Some(fb), None).unwrap();
        let mut child = Command::new("dtc").args(["-I", "dtb", "-O", "dts", "-"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&dtb).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let dts = String::from_utf8(output.stdout).unwrap();
        assert!(dts.contains("reg = <0x00 0x40000000 0x00 0x10000000 0x00 0x50800000 0x00 0x2f800000>"), "{dts}");
        assert!(dts.contains("framebuffer@50000000"));
        assert!(generate_dtb(0x40000000, 0x10000000, "", 1, &[], Some(fb), None).is_err());
        let headless = generate_dtb(0x40000000, 0x40000000, "", 1, &[], None, None).unwrap();
        assert!(!headless.windows(b"simple-framebuffer".len()).any(|w| w == b"simple-framebuffer"));
    }
}
