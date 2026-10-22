# vmm-intel

Intel-only. Guest is x86_64. Host is macOS 10.15 or newer on x86_64, where Hypervisor.framework exports the VMX API (`hv_vcpu_run`, `hv_vcpu_run_until`, `hv_vcpu_interrupt`, `hv_vmx_vcpu_set_apic_address`). This fork does not build or run on Apple Silicon, does not boot AArch64, and must not be merged back into the AArch64 VMM.

Provenance: copied from `dcf06d946a30f1159a6ab258b6626563bb8a45ed` on 2026-10-01. That revision is also in `UPSTREAM`. Shared-looking files (COW, vmnet, virtio queues) were copied once and are allowed to diverge. There is no upstream merge workflow.

The `vmm` binary compiles only for `x86_64-apple-darwin`. Before any `hv_*` call it allows marketing major >= 10. On 10.15, where `kern.osproductversion` may be absent, Darwin 19 is macOS 10.15. It refuses Rosetta (`sysctl.proc_translated` or `hw.optional.arm64`). Missing sysctls mean the process is not translated. With no arguments it prints help and exits 1. It does not call `hv_vm_create`.

`cargo test --lib` does not link guest code against Hypervisor entry points. The portable library covers COW, GPT, TCP forward, vmnet protocol, the terminal, fw_cfg items, CFI pflash, `MmioDevice`, and a host-buffer `GuestMemory`.

## Build

```bash
make build
make sign
make test
```

`make sign` codesigns with `entitlements.plist`. `make test` is `cargo +1.89.0 test --lib` and does not change the rustup default.

## Quick start

On an Intel Mac running macOS 10.15 or newer, sign the binary before it starts a guest:

```bash
make build
make sign
make fetch-uefi
make fetch-iso
```

Boot the FreeBSD 14.4 amd64 installer. The disk image must already exist. EFI variables default to `<disk>.vars.fd`. See [docs/freebsd_amd64.md](docs/freebsd_amd64.md).

```bash
mkdir -p "$HOME/VMs/freebsd-amd64"
truncate -s 16g "$HOME/VMs/freebsd-amd64/disk.img"
make vmnet-service
./target/debug/vmm \
  --uefi firmware/OVMF_CODE.fd \
  --cdrom FreeBSD-14.4-RELEASE-amd64-disc1.iso \
  --disk "$HOME/VMs/freebsd-amd64/disk.img" \
  --cpus 2 \
  --ram 1024 \
  --net vmnet-proxy \
  --serial-log "$HOME/VMs/freebsd-amd64/install.log"
```

If the guest stops at `mountroot>`, type `cd9660:/dev/vtbd0` and only that line. After install, drop `--cdrom` and keep the disk, the vars file, and `--cpus 2`.

`--uefi` is the firmware boot (`firmware/OVMF_CODE.fd` unless a path is given). EFI variables persist in `disk.img.vars.fd`; `--efi-vars PATH` overrides that. The first boot copies `firmware/OVMF_VARS.fd` (540672 bytes) into the store, and later boots open the same file. `--cow` attaches each `--disk` through a `PATH.cow` overlay. Do not combine `cow` and `ro`. With no arguments, `vmm` prints help and exits 1.

`python3 -B scripts/verify.py` runs library tests plus the payload and OVMF smokes. `--suite full` adds the FreeBSD installer smoke. See [docs/verification.md](docs/verification.md).
