# Offline COW overlay management

Build the standalone tool with `cargo build --locked --bin vmm-overlay`. It does
not launch a VM or require Hypervisor.framework. Stop every VM using the overlay
before inspecting or exporting it; the tool checks the existing file locks and
fails if a writer still holds either input.

```sh
target/debug/vmm-overlay inspect freebsd.img freebsd.img.cow
target/debug/vmm-overlay export freebsd.img freebsd.img.cow freebsd-merged.img
```

`inspect` opens both inputs read-only and reports the base size, 64 KiB cluster
geometry, allocated mappings, physical clusters, and unreferenced clusters. It
rejects a missing or empty overlay, truncated data, a wrong magic/version,
reserved header flags, a base-size mismatch, or invalid, repeated, and
out-of-range cluster offsets. It never initializes or repairs an overlay.
Unreferenced clusters can result from an interrupted allocation before the
mapping was published; inspection reports them without treating them as an
error. The VMMCOW01 header stores only the base size, not a base path or digest.
Matching geometry does **not** prove that the supplied base is the one used when
the overlay was created. Keep each base/overlay pair together and verify its
provenance before export.

`export` uses the same validation and locks, copies unchanged regions from the
base, then writes mapped overlay data to a new standalone raw image. It copies
only valid bytes from the final partial cluster. The destination must not
exist. The tool writes a hidden temporary file beside it, syncs the complete
image, and publishes it with a no-overwrite hard link. A normal failure removes
the temporary file and leaves both inputs and any existing destination alone.
If the process or host stops abruptly, a hidden `.NAME.vmm-export-*` temporary
file may remain; the named output is either absent or complete. Remove a stale
temporary file only after confirming no export is running. This workflow does
not alter the base or overlay.

There is intentionally no in-place `commit` command. A future base replacement
must first bind the overlay to an identified base (the current format cannot),
hold exclusive locks, materialize and sync a new image, preserve the old base
for recovery, atomically switch the named base, and sync the parent directory.
After a crash, recovery must decide from the preserved old base and a recorded
transaction state which image is authoritative before accepting further VM
writes. Until that protocol exists, use `export` and explicitly choose the new
image in the VM configuration. Never write overlay data into the original base
in place.
