.PHONY: all build sign sign-networking test release clean

all: build sign

VMM_BIN := target/debug/vmm

build:
	cargo build

test:
	cargo test --lib

sign:
	codesign --entitlements entitlements.plist --force -s - $(VMM_BIN)

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
	cargo build --release
	codesign --entitlements entitlements.plist --force -s - target/release/vmm

clean:
	cargo clean
