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

## Known issues: updating Cube.app with running VMs

Cube.app does not currently support a zero-downtime application update while
managed VMs are running:

- Quitting Cube.app does not stop managed VMs. Their manager and VMM processes
  run independently and continue using the version of the executable with which
  they were started.
- A normal quit stops the network service owned by Cube.app. Running guests that
  use `vmnet-proxy` remain powered on but lose network connectivity.
- A running VMM does not reconnect to a restarted `vmnet-proxy` service. Starting
  the updated app and its service therefore does not restore networking to those
  guests; the VMs must be restarted.
- The updated app does not adopt a network service launched by the previous app
  process. Service ownership is intentionally limited to the process that
  launched it.

Until service handoff or VMM reconnection is implemented, update Cube.app as
follows:

1. Run `cubectl shutdown <name>` for every running VM. Prefer guest shutdown to
   `cubectl stop`, which is a host-side forced stop.
2. Run `cubectl list` and confirm that no VMs remain running.
3. Quit Cube.app normally, replace or update the application, and launch the new
   version.
4. Confirm that the network service has started, then start the VMs again.

An updater should offer **Shut Down VMs and Update** and **Update Later** when it
detects running VMs. If a guest cannot shut down, the updater should cancel or
defer the update rather than force-stop it without explicit confirmation.

## Installing a FreeBSD desktop

The **New Virtual Machine** window includes a **Desktop** selector with server,
XFCE, and KDE Plasma choices. Desktop installation requires **Automated
Unattended Installation** and guest networking. Selecting a desktop raises CPU
and memory choices below the recommended baseline to 4 vCPUs and 4 GiB.

Cube first installs the FreeBSD base system, then boots the managed VM and uses
an automatically generated per-VM SSH key to install the desktop packages. It
creates the graphical user `cube`, using the root password entered in the form,
configures LightDM or SDDM, installs the SCFB Xorg configuration and `cube_hid`
service, and powers the VM off when provisioning finishes. The default desktop
framebuffer is 1280×800. Package download time depends on the FreeBSD mirror and
can be substantially longer than the base installation.

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
