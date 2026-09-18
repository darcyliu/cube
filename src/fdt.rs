//! Device Tree (FDT) generator for ARM64 virtual machines.

#![allow(dead_code)]

use std::io::Write;
use std::process::{Command, Stdio};

pub fn generate_dtb(
    ram_base: u64,
    ram_size: u64,
    bootargs: &str,
    num_cpus: usize,
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

    let dts = format!(
        r#"/dts-v1/;

/ {{
    #address-cells = <2>;
    #size-cells = <2>;
    compatible = "freebsd,arm64", "linux,dummy-virt";
    model = "macOS Hypervisor Virtual Machine";

    chosen {{
        bootargs = "{bootargs}";
        stdout-path = "/pl011@09000000";
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
        reg = <0x{ram_base_hi:x} 0x{ram_base_lo:x} 0x{ram_size_hi:x} 0x{ram_size_lo:x}>;
    }};

    intc: interrupt-controller@08000000 {{
        compatible = "arm,gic-v3";
        #interrupt-cells = <3>;
        interrupt-controller;
        reg = <0x0 0x08000000 0x0 0x10000>,
              <0x0 0x080a0000 0x0 0x{redist_size:x}>;
    }};

    timer {{
        compatible = "arm,armv8-timer";
        interrupt-parent = <&intc>;
        interrupts = <1 13 0xf08>,
                     <1 14 0xf08>,
                     <1 11 0xf08>,
                     <1 10 0xf08>;
        always-on;
    }};

    pl011@09000000 {{
        compatible = "arm,pl011", "arm,primecell";
        reg = <0x0 0x09000000 0x0 0x1000>;
        interrupt-parent = <&intc>;
        interrupts = <0 1 4>; /* SPI 1 (interrupt ID 33) */
    }};

    virtio_mmio@0a000000 {{
        compatible = "virtio,mmio";
        reg = <0x0 0x0a000000 0x0 0x200>;
        interrupt-parent = <&intc>;
        interrupts = <0 2 4>; /* SPI 2 (interrupt ID 34) */
    }};

    virtio_mmio@0a000200 {{
        compatible = "virtio,mmio";
        reg = <0x0 0x0a000200 0x0 0x200>;
        interrupt-parent = <&intc>;
        interrupts = <0 3 4>; /* SPI 3 (interrupt ID 35) */
    }};
}};
"#,
        bootargs = bootargs,
        ram_base = ram_base,
        ram_base_hi = (ram_base >> 32) as u32,
        ram_base_lo = (ram_base & 0xffff_ffff) as u32,
        ram_size_hi = (ram_size >> 32) as u32,
        ram_size_lo = (ram_size & 0xffff_ffff) as u32,
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
