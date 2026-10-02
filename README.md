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

`make sign` codesigns with `entitlements.plist`. `make test` is `cargo test --lib`.
