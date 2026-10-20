.PHONY: all build sign sign-networking run freebsd freebsd-zfs freebsd-zfs-uefi fetch-freebsd fetch-uefi release clean clean-vm install-iso install-seed verify alpine debian debian-cloud app cube run-cube debug-app sign-cube-networking debug-run-cube install install-cli

all: build sign

install: install-cli

CUBE_BIN := target/debug/cube
CUBECTL_BIN := target/debug/cubectl
CUBE_APP ?= dist/Cube.app

install-cli:
	@mkdir -p $(HOME)/.local/bin
	@ln -sf $(CURDIR)/$(CUBE_BIN) $(HOME)/.local/bin/cube
	@ln -sf $(CURDIR)/$(CUBECTL_BIN) $(HOME)/.local/bin/cubectl
	@echo "[Cube] Linked CLI: $(HOME)/.local/bin/cube → $(CURDIR)/$(CUBE_BIN)"
	@echo "[Cube] Linked CLI: $(HOME)/.local/bin/cubectl → $(CURDIR)/$(CUBECTL_BIN)"

# VERIFY_ARGS can select components, offline/full suites, and media paths.
verify:
	python3 -B scripts/verify.py $(VERIFY_ARGS)

.PHONY: verify-cube verify-cubectl
verify-cube:
	python3 -B scripts/verify.py --component cube $(VERIFY_ARGS)

verify-cubectl:
	python3 -B scripts/verify.py --component cubectl $(VERIFY_ARGS)

.PHONY: test-vm-management
test-vm-management:
	@mkdir -p target/swift-cache
	swiftc -module-cache-path target/swift-cache \
		$(filter-out app/Cube/Sources/main.swift,$(wildcard app/Cube/Sources/*.swift)) \
		app/Cube/Tests/VMManagementTests.swift -o target/vm-management-tests
	target/vm-management-tests

build:
	cargo build

sign:
	codesign --entitlements entitlements.plist --force -s - $(CUBE_BIN)
	codesign --entitlements entitlements.plist --force -s - $(CUBECTL_BIN)

BUNDLE_ID ?= net.neuralengine.cube
TEAM_ID   ?= ZV8TTD4Z45

# Restricted com.apple.vm.networking — needs Apple-granted App ID + provisioning profile.
# Example:
#   IDENTITY="Apple Development: Your Name (ZV8TTD4Z45)" \
#   PROFILE="$$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles/….provisionprofile" \
#   make sign-networking
sign-networking: build
	IDENTITY="$(IDENTITY)" PROFILE="$(PROFILE)" BUNDLE_ID="$(BUNDLE_ID)" TEAM_ID="$(TEAM_ID)" \
		CUBE_BIN="$(CUBE_BIN)" ./scripts/sign-networking.sh

# Native macOS Menu Bar app to host vmnet-service / vmnet-service-bridge
app: build sign
	@mkdir -p "$(CUBE_APP)"/Contents/MacOS "$(CUBE_APP)"/Contents/Resources target/swift-cache
	swiftc -O -module-cache-path target/swift-cache \
		app/Cube/Sources/ServiceManager.swift \
		app/Cube/Sources/SystemMetrics.swift \
		app/Cube/Sources/VMManager.swift \
		app/Cube/Sources/ImageCacheManager.swift \
		app/Cube/Sources/NewVMWindowController.swift \
		app/Cube/Sources/DashboardWindowController.swift \
		app/Cube/Sources/VMCommandsSheetController.swift \
		app/Cube/Sources/VMLogWindowController.swift \
		app/Cube/Sources/MenuController.swift \
		app/Cube/Sources/PreferencesWindowController.swift \
		app/Cube/Sources/AppDelegate.swift \
		app/Cube/Sources/main.swift \
		-o "$(CUBE_APP)"/Contents/MacOS/CubeBar
	cp app/Cube/Resources/Info.plist "$(CUBE_APP)"/Contents/Info.plist
	@BUILD_NUM=$$(git rev-list --count HEAD 2>/dev/null || echo 1); \
	sed -i '' "s/<string>33<\/string>/<string>$$BUILD_NUM<\/string>/g" "$(CUBE_APP)"/Contents/Info.plist 2>/dev/null || true
	cp app/Cube/Resources/AppIcon.icns "$(CUBE_APP)"/Contents/Resources/AppIcon.icns
	[ -f app/Cube/Resources/Credits.rtf ] && cp app/Cube/Resources/Credits.rtf "$(CUBE_APP)"/Contents/Resources/Credits.rtf || true
	./scripts/embed-cube-helper.sh "$(CUBE_BIN)" "$(CUBE_APP)"
	@if [ -n "$$PROFILE" ] || ls *.provisionprofile >/dev/null 2>&1; then \
		echo "[Cube] Provisioning profile detected, signing with Developer identity & com.apple.vm.networking..."; \
		IDENTITY="$(IDENTITY)" PROFILE="$(PROFILE)" BUNDLE_ID="$(BUNDLE_ID)" TEAM_ID="$(TEAM_ID)" \
			CUBE_BIN="$(CUBE_BIN)" APP_DIR="$(CUBE_APP)" ./scripts/sign-cube-networking.sh || { \
				echo "[Cube] Developer signing failed, falling back to ad-hoc signature..."; \
				codesign --force --deep --sign - --entitlements app/Cube/Resources/Cube.entitlements "$(CUBE_APP)"; \
			}; \
	else \
		codesign --force --deep --sign - --entitlements app/Cube/Resources/Cube.entitlements "$(CUBE_APP)"; \
		echo "[Cube] Built ad-hoc macOS Menu Bar App → $(CUBE_APP)"; \
	fi

# Build debug version with debug symbols (-g) and get-task-allow for lldb
debug-app: build
	@mkdir -p "$(CUBE_APP)"/Contents/MacOS "$(CUBE_APP)"/Contents/Resources target/swift-cache
	swiftc -g -module-cache-path target/swift-cache \
		app/Cube/Sources/ServiceManager.swift \
		app/Cube/Sources/SystemMetrics.swift \
		app/Cube/Sources/VMManager.swift \
		app/Cube/Sources/ImageCacheManager.swift \
		app/Cube/Sources/NewVMWindowController.swift \
		app/Cube/Sources/DashboardWindowController.swift \
		app/Cube/Sources/VMCommandsSheetController.swift \
		app/Cube/Sources/VMLogWindowController.swift \
		app/Cube/Sources/MenuController.swift \
		app/Cube/Sources/PreferencesWindowController.swift \
		app/Cube/Sources/AppDelegate.swift \
		app/Cube/Sources/main.swift \
		-o "$(CUBE_APP)"/Contents/MacOS/CubeBar
	cp app/Cube/Resources/Info.plist "$(CUBE_APP)"/Contents/Info.plist
	cp app/Cube/Resources/AppIcon.icns "$(CUBE_APP)"/Contents/Resources/AppIcon.icns
	[ -f app/Cube/Resources/Credits.rtf ] && cp app/Cube/Resources/Credits.rtf "$(CUBE_APP)"/Contents/Resources/Credits.rtf || true
	./scripts/embed-cube-helper.sh "$(CUBE_BIN)" "$(CUBE_APP)"
	codesign --force --deep --sign - --entitlements app/Cube/Resources/Cube.debug.entitlements "$(CUBE_APP)"
	@echo "[Cube] Built Debug macOS Menu Bar App (debugger-attachable) → $(CUBE_APP)"

# Sign Cube with Developer identity + com.apple.vm.networking profile
sign-cube-networking: app
	IDENTITY="$(IDENTITY)" PROFILE="$(PROFILE)" BUNDLE_ID="$(BUNDLE_ID)" TEAM_ID="$(TEAM_ID)" \
	CUBE_BIN="$(CUBE_BIN)" APP_DIR="$(CUBE_APP)" ./scripts/sign-cube-networking.sh

cube: app

run-cube: cube
	open "$(CUBE_APP)"

# Run directly in terminal for debug logs (Control-C to exit)
debug-run-cube: debug-app
	"$(CUBE_APP)"/Contents/MacOS/CubeBar

run: all
	$(CUBE_BIN)

fetch-freebsd:
	@mkdir -p boot/freebsd
	@if [ ! -f boot/freebsd/kernel ] || [ ! -f boot/freebsd/zfs.ko ]; then \
		echo "[Cube] Downloading official FreeBSD 14.4-RELEASE ARM64 kernel.txz..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/14.4-RELEASE/kernel.txz | tar -xf - boot/kernel/kernel boot/kernel/zfs.ko; \
		mv boot/kernel/kernel boot/freebsd/ && mv boot/kernel/zfs.ko boot/freebsd/ && rmdir boot/kernel 2>/dev/null || true; \
		echo "[Cube] Extracted boot/freebsd/kernel and boot/freebsd/zfs.ko"; \
	else \
		echo "[Cube] boot/freebsd/kernel and boot/freebsd/zfs.ko already present."; \
	fi

CPUS ?= 2
NET  ?= vmnet-proxy

freebsd: all fetch-freebsd
	$(CUBE_BIN) --freebsd boot/freebsd/kernel --net $(NET)

fetch-image:
	@if [ ! -f mini-memstick.img ]; then \
		echo "[Cube] Downloading FreeBSD 14.4-RELEASE ARM64 mini-memstick image..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.4/FreeBSD-14.4-RELEASE-arm64-aarch64-mini-memstick.img.xz | xz -d > mini-memstick.img; \
		echo "[Cube] Download complete: mini-memstick.img"; \
	else \
		echo "[Cube] mini-memstick.img already present."; \
	fi

freebsd-root: all fetch-freebsd fetch-image
	$(CUBE_BIN) --freebsd boot/freebsd/kernel --disk mini-memstick.img --net $(NET)

# Long-running vmnet proxy. Build as your user first (`make all`), then `make vmnet-service`.
# The recipe sudo's only the service. Guests use `--net vmnet-proxy` without sudo.
# `sudo make vmnet-service` is refused: it would record SUDO_UID as root.
# SUBNET=192.168.18.0/24 asks NAT mode for that RFC1918 range.
vmnet-service:
	@test "$$(id -u)" -ne 0 || { echo "[Cube] Run 'make vmnet-service' as your user, not 'sudo make'."; exit 1; }
	@test -x $(CUBE_BIN) || { echo "[Cube] Run 'make all' as your user first."; exit 1; }
	sudo $(CUBE_BIN) --vmnet-service $(if $(SUBNET),--subnet $(SUBNET),)

# Same service, bridged onto the host's default NIC (or IFACE=en7). Guests use LAN DHCP.
# Stop the NAT service first: one socket, one mode.
vmnet-service-bridge:
	@test "$$(id -u)" -ne 0 || { echo "[Cube] Run 'make vmnet-service-bridge' as your user, not 'sudo make'."; exit 1; }
	@test -x $(CUBE_BIN) || { echo "[Cube] Run 'make all' as your user first."; exit 1; }
	sudo $(CUBE_BIN) --vmnet-service --bridge $(IFACE)

freebsd-net: all fetch-freebsd fetch-image
	$(CUBE_BIN) --freebsd boot/freebsd/kernel --disk mini-memstick.img --net $(NET) --cpus $(CPUS)

install-iso: all
	./scripts/install-from-iso.sh

install-seed: all
	./scripts/unattended-install.sh

alpine: all
	./scripts/alpine.sh

debian: all
	./scripts/debian.sh

debian-cloud: all
	./scripts/debian-cloud.sh

release:
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/cube



# EDK2 / UEFI firmware for Strategy 2 (root-on-ZFS via loader.efi).
# Downloads QEMU aarch64 EDK2 code flash + empty vars store.
fetch-uefi:
	@mkdir -p firmware
	@if [ ! -f firmware/QEMU_EFI.fd ]; then \
		echo "[Cube] Fetching QEMU edk2-aarch64-code.fd..."; \
		curl -fSL --progress-bar -o /tmp/edk2-aarch64-code.fd.bz2 \
			https://github.com/qemu/qemu/raw/v9.2.0/pc-bios/edk2-aarch64-code.fd.bz2; \
		bunzip2 -c /tmp/edk2-aarch64-code.fd.bz2 > firmware/QEMU_EFI.fd; \
		echo "[Cube] Wrote firmware/QEMU_EFI.fd ($$(wc -c < firmware/QEMU_EFI.fd) bytes)"; \
	else \
		echo "[Cube] firmware/QEMU_EFI.fd already present."; \
	fi
	@if [ ! -f firmware/QEMU_VARS.fd ]; then \
		dd if=/dev/zero of=firmware/QEMU_VARS.fd bs=1m count=64 status=none; \
		echo "[Cube] Created empty firmware/QEMU_VARS.fd (64 MiB)"; \
	else \
		echo "[Cube] firmware/QEMU_VARS.fd already present."; \
	fi

# Strategy 2: UEFI → ESP loader.efi → ZFS (no ELF kernel / zfs.ko preload).
# Example: make freebsd-zfs-uefi DISK=freebsd-installed.img
freebsd-zfs-uefi: all fetch-uefi
	@if [ ! -f "$(DISK)" ]; then \
		echo "[Cube] Missing $(DISK). Run: make install-iso   (choose Auto ZFS in bsdinstall)"; \
		exit 1; \
	fi
	$(CUBE_BIN) --uefi --disk "$(DISK)" --net $(NET) --cpus $(CPUS)

# Strategy 1 (experimental): direct ELF + zfs.ko preload — often hangs after MODINFO.
# Prefer freebsd-zfs-uefi for root-on-ZFS.
# Example after install-from-iso with Auto (ZFS):
#   make freebsd-zfs DISK=freebsd-installed.img
DISK ?= freebsd-installed.img
CPUS ?= 2

freebsd-zfs: all fetch-freebsd
	@if [ ! -f "$(DISK)" ]; then 		echo "[Cube] Missing $(DISK). Run: make install-iso   (choose Auto ZFS in bsdinstall)"; 		exit 1; 	fi
	$(CUBE_BIN) --freebsd boot/freebsd/kernel --disk "$(DISK)" --net $(NET) --cpus $(CPUS)

clean:
	cargo clean
	rm -f guest/payload.o guest/payload.bin guest/linux_payload.o guest/linux_payload.bin

clean-vm:
	./scripts/clean-vms.sh
