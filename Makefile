.PHONY: all build sign sign-networking test release clean fetch-uefi vmnet-service vmnet-service-bridge

all: build sign

CARGO := cargo +1.89.0
VMM_BIN := target/debug/vmm

build:
	$(CARGO) build

test:
	$(CARGO) test --lib

# QEMU 9.2.0 stores these as bz2. The uncompressed sizes are still required.
fetch-uefi:
	mkdir -p firmware
	@if [ ! -f firmware/OVMF_CODE.fd ]; then \
		curl -fsSL -o firmware/OVMF_CODE.fd.bz2 https://github.com/qemu/qemu/raw/v9.2.0/pc-bios/edk2-x86_64-code.fd.bz2 && \
		bunzip2 -c firmware/OVMF_CODE.fd.bz2 > firmware/OVMF_CODE.fd && \
		rm -f firmware/OVMF_CODE.fd.bz2; \
	fi
	@if [ ! -f firmware/OVMF_VARS.fd ]; then \
		curl -fsSL -o firmware/OVMF_VARS.fd.bz2 https://github.com/qemu/qemu/raw/v9.2.0/pc-bios/edk2-i386-vars.fd.bz2 && \
		bunzip2 -c firmware/OVMF_VARS.fd.bz2 > firmware/OVMF_VARS.fd && \
		rm -f firmware/OVMF_VARS.fd.bz2; \
	fi
	@code_sz=$$(wc -c < firmware/OVMF_CODE.fd | tr -d ' '); \
	if [ "$$code_sz" != "3653632" ]; then \
		echo "firmware/OVMF_CODE.fd is $$code_sz bytes, want 3653632"; \
		rm -f firmware/OVMF_CODE.fd; \
		exit 1; \
	fi
	@vars_sz=$$(wc -c < firmware/OVMF_VARS.fd | tr -d ' '); \
	if [ "$$vars_sz" != "540672" ]; then \
		echo "firmware/OVMF_VARS.fd is $$vars_sz bytes, want 540672"; \
		rm -f firmware/OVMF_VARS.fd; \
		exit 1; \
	fi

sign:
	codesign --entitlements entitlements.plist --force -s - $(VMM_BIN)

# Long-running vmnet proxy. Build as your user first (`make all`), then `make vmnet-service`.
# `sudo make vmnet-service` is refused: it would record SUDO_UID as root.
vmnet-service:
	@test "$$(id -u)" -ne 0 || { echo "[VMM] Run 'make vmnet-service' as your user, not 'sudo make'."; exit 1; }
	@test -x $(VMM_BIN) || { echo "[VMM] Run 'make all' as your user first."; exit 1; }
	sudo $(VMM_BIN) --vmnet-service $(if $(SUBNET),--subnet $(SUBNET),)

vmnet-service-bridge:
	@test "$$(id -u)" -ne 0 || { echo "[VMM] Run 'make vmnet-service-bridge' as your user, not 'sudo make'."; exit 1; }
	@test -x $(VMM_BIN) || { echo "[VMM] Run 'make all' as your user first."; exit 1; }
	sudo $(VMM_BIN) --vmnet-service --bridge $(IFACE)

# Restricted com.apple.vm.networking — needs Apple-granted App ID + provisioning profile.
# Example:
#   IDENTITY="Apple Development: You (TEAMID)" \
#   PROFILE="$$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles/….provisionprofile" \
#   BUNDLE_ID=com.example.vmm \
#   make sign-networking
sign-networking: build
	IDENTITY="$(IDENTITY)" PROFILE="$(PROFILE)" BUNDLE_ID="$(BUNDLE_ID)" \
		VMM_BIN="$(VMM_BIN)" ./scripts/sign-networking.sh

release:
	$(CARGO) build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm

clean:
	$(CARGO) clean
