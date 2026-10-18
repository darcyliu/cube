# Cube: unprivileged macOS VM networking

Cube is a menu bar app that starts its bundled `cube --vmnet-service`
helper as the logged-in user. Guests connect using `--net vmnet-proxy`.
Cube never invokes sudo, installs a root daemon, or falls back to an
administrator launch.

## Build and sign

`make app` and `make debug-app` both embed a fresh helper app. These builds
are ad-hoc signed for development; vmnet requires an Apple-granted
`com.apple.vm.networking` entitlement and matching provisioning profile.
A normal rebuild replaces the helper and clears previous provisioning, so
sign again before using networking:

```sh
IDENTITY="Apple Development: Your Name (TEAMID)" \
PROFILE="/path/to/YourProfile.provisionprofile" \
make sign-cube-networking
```

The signing script signs the embedded helper and Cube with the networking
entitlement. Keep the app bundle intact when moving it. Start failures appear
in an alert; **Open Service Log** provides the helper's diagnostics.

## Lifecycle and configuration

- Apple Default NAT uses vmnet's selected shared subnet without PF setup.
- Bridged LAN attaches to the selected host interface.
- Custom subnets are unavailable in Cube because the current CLI implementation
  requires root to configure PF and IPv4 forwarding. The separately administered
  `sudo cube --vmnet-service --subnet ...` CLI reports setup failures as errors.
- Enable **Launch Cube at login** and automatic service startup in Preferences
  to start networking after login using the latest saved settings.
- Cube owns only the helper it launched. It does not adopt or stop another
  service listening at the same socket.
- Quit stops the owned helper. Stop and restart wait for process exit without
  removing a live socket. A failed stop prevents a replacement launch.
- Startup waits up to 30 seconds. A timeout terminates the child; if termination
  fails, Cube retains ownership and reports the failure so Stop can be retried.

The socket and log are under `~/Library/Application Support/cube/`.
Stopping or restarting networking disconnects attached guests.

## Regression checks

```sh
swiftc app/Cube/Sources/ServiceManager.swift \
  app/Cube/Tests/ServiceManagerTests.swift -o /tmp/cube-service-tests
/tmp/cube-service-tests
cargo test vmnet_service --locked
```

The Swift checks use a fake socket server and require no vmnet entitlement.
They cover slow startup, ownership isolation, failed shutdown/restart, startup
timeout cleanup, and rejection of custom NAT in the unprivileged app.
