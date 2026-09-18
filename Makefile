.PHONY: all build sign run release clean

all: build sign

build:
	cargo build

sign:
	codesign --entitlements entitlements.plist --force -s - target/debug/vmm

run: all
	./target/debug/vmm

release:
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm
	./target/release/vmm

clean:
	cargo clean
	rm -f guest/payload.o guest/payload.bin
