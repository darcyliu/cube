.PHONY: all build sign run freebsd fetch-freebsd release clean

all: build sign

build:
	cargo build

sign:
	codesign --entitlements entitlements.plist --force -s - target/debug/vmm

run: all
	./target/debug/vmm

fetch-freebsd:
	@mkdir -p boot
	@if [ ! -f boot/kernel/kernel ]; then \
		echo "[VMM] Downloading official FreeBSD 14.4-RELEASE ARM64 kernel..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/14.4-RELEASE/kernel.txz | tar -xf - boot/kernel/kernel; \
		echo "[VMM] Download complete: boot/kernel/kernel"; \
	else \
		echo "[VMM] boot/kernel/kernel already present."; \
	fi

freebsd: all fetch-freebsd
	./target/debug/vmm --freebsd boot/kernel/kernel

fetch-image:
	@if [ ! -f mini-memstick.img ]; then \
		echo "[VMM] Downloading FreeBSD 14.4-RELEASE ARM64 mini-memstick image..."; \
		curl -fSL https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.4/FreeBSD-14.4-RELEASE-arm64-aarch64-mini-memstick.img.xz | xz -d > mini-memstick.img; \
		echo "[VMM] Download complete: mini-memstick.img"; \
	else \
		echo "[VMM] mini-memstick.img already present."; \
	fi

freebsd-root: all fetch-freebsd fetch-image
	./target/debug/vmm --freebsd boot/kernel/kernel --disk mini-memstick.img

CPUS ?= 2

freebsd-net: all fetch-freebsd fetch-image
	./target/debug/vmm --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus $(CPUS)

sudo-freebsd-net: all fetch-freebsd fetch-image
	@IF="$$(route get default 2>/dev/null | awk '/interface:/{print $$2}')"; \
	if [ -n "$$IF" ]; then \
		echo "[VMM] Setting up macOS PF NAT forwarding on $$IF..."; \
		sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null; \
		printf 'nat on %s from 192.168.18.0/24 to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$$IF" "$$IF" "$$IF" | sudo pfctl -a "com.apple/vmm" -f - 2>/dev/null || true; \
	fi
	sudo ./target/debug/vmm --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus $(CPUS)

release:
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm

clean:
	cargo clean
	rm -f guest/payload.o guest/payload.bin
