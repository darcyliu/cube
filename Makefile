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

release:
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm

clean:
	cargo clean
	rm -f guest/payload.o guest/payload.bin
