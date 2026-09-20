.PHONY: all build sign run freebsd freebsd-zfs sudo-freebsd-zfs freebsd-zfs-uefi sudo-freebsd-zfs-uefi fetch-freebsd fetch-uefi release clean clean-vm install-iso install-seed

all: build sign

# Avoid root-owned ./target from earlier sudo cargo runs.
CARGO_TARGET_DIR ?= /tmp/vmm-target-$(USER)
VMM_BIN := $(CARGO_TARGET_DIR)/debug/vmm

build:
	CARGO_TARGET_DIR=$(CARGO_TARGET_DIR) cargo build

sign:
	codesign --entitlements entitlements.plist --force -s - $(VMM_BIN)

run: all
	$(VMM_BIN)

fetch-freebsd:
	@mkdir -p boot
	@NEED=""; \
	if [ ! -f boot/kernel/kernel ]; then NEED="$$NEED boot/kernel/kernel"; fi; \
	if [ ! -f boot/kernel/zfs.ko ]; then NEED="$$NEED boot/kernel/zfs.ko"; fi; \
	if [ -n "$$NEED" ]; then \
		echo "[VMM] Downloading official FreeBSD 14.4-RELEASE ARM64 kernel.txz..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/14.4-RELEASE/kernel.txz | tar -xf - $$NEED; \
		echo "[VMM] Extracted:$$NEED"; \
	else \
		echo "[VMM] boot/kernel/kernel and boot/kernel/zfs.ko already present."; \
	fi

freebsd: all fetch-freebsd
	$(VMM_BIN) --freebsd boot/kernel/kernel

fetch-image:
	@if [ ! -f mini-memstick.img ]; then \
		echo "[VMM] Downloading FreeBSD 14.4-RELEASE ARM64 mini-memstick image..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.4/FreeBSD-14.4-RELEASE-arm64-aarch64-mini-memstick.img.xz | xz -d > mini-memstick.img; \
		echo "[VMM] Download complete: mini-memstick.img"; \
	else \
		echo "[VMM] mini-memstick.img already present."; \
	fi

freebsd-root: all fetch-freebsd fetch-image
	$(VMM_BIN) --freebsd boot/kernel/kernel --disk mini-memstick.img

CPUS ?= 2

freebsd-net: all fetch-freebsd fetch-image
	$(VMM_BIN) --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus $(CPUS)

sudo-freebsd-net: all fetch-freebsd fetch-image
	@IF="$$(route get default 2>/dev/null | awk '/interface:/{print $$2}')"; \
	if [ -n "$$IF" ]; then \
		echo "[VMM] Setting up macOS PF NAT forwarding on $$IF..."; \
		sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null; \
		printf 'nat on %s from 192.168.18.0/24 to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$$IF" "$$IF" "$$IF" | sudo pfctl -a "com.apple/vmm" -f - 2>/dev/null || true; \
	fi
	sudo $(VMM_BIN) --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus $(CPUS)

install-iso: all
	./scripts/install-from-iso.sh

install-seed: all
	./scripts/unattended-install.sh

release:
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm



# EDK2 / UEFI firmware for Strategy 2 (root-on-ZFS via loader.efi).
# Downloads QEMU aarch64 EDK2 code flash + empty vars store.
fetch-uefi:
	@mkdir -p firmware
	@if [ ! -f firmware/QEMU_EFI.fd ]; then \
		echo "[VMM] Fetching QEMU edk2-aarch64-code.fd..."; \
		curl -fSL --progress-bar -o /tmp/edk2-aarch64-code.fd.bz2 \
			https://github.com/qemu/qemu/raw/v9.2.0/pc-bios/edk2-aarch64-code.fd.bz2; \
		bunzip2 -c /tmp/edk2-aarch64-code.fd.bz2 > firmware/QEMU_EFI.fd; \
		echo "[VMM] Wrote firmware/QEMU_EFI.fd ($$(wc -c < firmware/QEMU_EFI.fd) bytes)"; \
	else \
		echo "[VMM] firmware/QEMU_EFI.fd already present."; \
	fi
	@if [ ! -f firmware/QEMU_VARS.fd ]; then \
		dd if=/dev/zero of=firmware/QEMU_VARS.fd bs=1m count=64 status=none; \
		echo "[VMM] Created empty firmware/QEMU_VARS.fd (64 MiB)"; \
	else \
		echo "[VMM] firmware/QEMU_VARS.fd already present."; \
	fi

# Strategy 2: UEFI → ESP loader.efi → ZFS (no ELF kernel / zfs.ko preload).
# Example: make freebsd-zfs-uefi DISK=freebsd-installed.img
freebsd-zfs-uefi: all fetch-uefi
	@if [ ! -f "$(DISK)" ]; then \
		echo "[VMM] Missing $(DISK). Run: make install-iso   (choose Auto ZFS in bsdinstall)"; \
		exit 1; \
	fi
	$(VMM_BIN) --uefi --disk "$(DISK)" --cpus $(CPUS)

sudo-freebsd-zfs-uefi: all fetch-uefi
	@if [ ! -f "$(DISK)" ]; then echo "[VMM] Missing $(DISK)"; exit 1; fi
	sudo $(VMM_BIN) --uefi --disk "$(DISK)" --net vmnet --cpus $(CPUS)

# Strategy 1 (experimental): direct ELF + zfs.ko preload — often hangs after MODINFO.
# Prefer freebsd-zfs-uefi for root-on-ZFS.
# Example after install-from-iso with Auto (ZFS):
#   make freebsd-zfs DISK=freebsd-installed.img
DISK ?= freebsd-installed.img
CPUS ?= 2

freebsd-zfs: all fetch-freebsd
	@if [ ! -f "$(DISK)" ]; then 		echo "[VMM] Missing $(DISK). Run: make install-iso   (choose Auto ZFS in bsdinstall)"; 		exit 1; 	fi
	$(VMM_BIN) --freebsd boot/kernel/kernel --disk "$(DISK)" --cpus $(CPUS)

sudo-freebsd-zfs: all fetch-freebsd
	@if [ ! -f "$(DISK)" ]; then echo "[VMM] Missing $(DISK)"; exit 1; fi
	sudo $(VMM_BIN) --freebsd boot/kernel/kernel --disk "$(DISK)" --net vmnet --cpus $(CPUS)

clean:
	cargo clean
	rm -f guest/payload.o guest/payload.bin

clean-vm:
	./scripts/clean-vms.sh
