# Cube Network Wire Protocol Specification (VMMNET v1)

## 1. Overview

The **VMMNET Protocol** is a lightweight, low-latency Unix domain socket wire protocol designed to connect virtual machines, emulators, and userland networking tools to **Cube's unprivileged `vmnet` service** on macOS.

By connecting to this socket, any third-party application (such as QEMU, Cloud-Hypervisor, custom hypervisors, packet generators, or testing utilities) can:
1. Obtain native, high-performance L2 Ethernet connectivity.
2. Communicate with other running virtual machines through an in-memory virtual switch.
3. Access the local network and internet via macOS NAT or physical LAN bridging without requiring `sudo` or root privileges.

---

## 2. Socket Transport & Security

### 2.1 Socket Location
The service listens on a standard local Unix domain stream socket:
```text
~/Library/Application Support/cube/vmnet.sock
```
*(Default path: `$HOME/Library/Application Support/cube/vmnet.sock`)*

### 2.2 Permissions and Isolation
- **Mode:** `0600` (`-rw-------`).
- **Ownership:** Owned by the current active macOS user (`UID` / `GID`).
- **Security Check:** During connection setup, the service inspects peer credentials via `LOCAL_PEERCRED`. Any connection originating from a different user ID (including `root`) is rejected with status code `5` (`STATUS_UNAUTHORIZED`). This guarantees strict per-user sandbox isolation.
- **Recommended Socket Options:** Clients should enable `SO_NOSIGPIPE` on macOS to avoid `SIGPIPE` signals when the remote peer disconnects.

---

## 3. Protocol State Machine

```
   Client                                                 Cube / vmnet Service
     │                                                              │
     │ ────────────── 1. Connect Unix Domain Socket ──────────────► │
     │                                                              │
     │ ────────────── 2. Send ClientHello (24 bytes) ─────────────► │
     │                                                              │
     │ ◄───────────── 3. Receive ServerHello (20 bytes) ─────────── │
     │                                                              │
     │               [ Connection Active: L2 Virtual Switch ]      │
     │                                                              │
     │ ────────────── 4. Send L2 Frame [Len (4B) + Data] ─────────► │
     │ ◄───────────── 5. Recv L2 Frame [Len (4B) + Data] ────────── │
     │                                                              │
     │ ────────────── 6. Close / Disconnect ──────────────────────► │
     ▼                                                              ▼
```

### Phase 1: Connection & Handshake
1. Client establishes a stream connection to the Unix domain socket.
2. Client sends a **`ClientHello`** (24 bytes).
3. Server validates the client's version, MAC uniqueness, and capacity.
4. Server responds with a **`ServerHello`** (20 bytes).
5. If `status == 0` (`STATUS_OK`), the connection transitions to the **Streaming** state. Otherwise, both sides close the socket.

### Phase 2: Frame Streaming
Once negotiated, communication is full-duplex. Raw L2 Ethernet frames (14 to 1514+ bytes) are exchanged using a 4-byte little-endian length prefix.

---

## 4. Message Formats

All multibyte integers are encoded in **Little-Endian** format.

### 4.1 ClientHello (24 Bytes)

Sent immediately by the client upon connecting:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                    Magic: "VMMNET\x01\x00"                    +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        Protocol Version                       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       Client MAC (Bytes 0..3)                 |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       Client MAC (4..5)       |             Flags             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      Requested Max Frame                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0..8` | **`magic`** | `[u8; 8]` | Fixed 8-byte ASCII/binary magic: `b"VMMNET\x01\x00"` (`0x56 0x4D 0x4D 0x4E 0x45 0x54 0x01 0x00`). |
| `8..12` | **`version`** | `u32` (LE) | Protocol version. Current version is `1`. |
| `12..18` | **`mac`** | `[u8; 6]` | The guest network interface's unique unicast MAC address (e.g., `52:54:00:12:34:56`). Must not be multicast or all-zeros. |
| `18..20` | **`flags`** | `u16` (LE) | Reserved for future capability negotiation. Set to `0`. |
| `20..24` | **`max_frame`** | `u32` (LE) | Maximum Ethernet frame size supported by client. Default is `1514` (standard MTU 1500) or up to `2048`. |

---

### 4.2 ServerHello (20 Bytes)

Sent by the server in response to `ClientHello`:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                    Magic: "VMMNET\x01\x00"                    +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        Protocol Version                       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Status Code                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     Negotiated Max Frame                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0..8` | **`magic`** | `[u8; 8]` | Fixed 8-byte magic: `b"VMMNET\x01\x00"`. |
| `8..12` | **`version`** | `u32` (LE) | Server protocol version (`1`). |
| `12..16` | **`status`** | `u32` (LE) | Result of connection request (see Status Codes table below). `0` indicates success. |
| `16..20` | **`max_frame`** | `u32` (LE) | Negotiated maximum frame size (typically `1514`). |

#### Status Codes

| Code | Name | Meaning |
| :---: | :--- | :--- |
| `0` | **`STATUS_OK`** | Connection accepted. Transition to streaming. |
| `1` | **`STATUS_DUPLICATE_MAC`** | Another client with this MAC address is already connected. |
| `2` | **`STATUS_UNSUPPORTED_VERSION`** | Client requested an unsupported protocol version. |
| `3` | **`STATUS_REJECTED_MAC`** | The supplied MAC address was invalid (e.g. multicast bit set or all zeros). |
| `4` | **`STATUS_BAD_MAX_FRAME`** | Requested MTU was invalid (must be between 1514 and 2048). |
| `5` | **`STATUS_UNAUTHORIZED`** | The connecting process UID does not match the service owner UID. |
| `6` | **`STATUS_TOO_MANY_CLIENTS`** | Switch capacity reached (maximum 16 concurrent clients). |

---

### 4.3 Streaming Frame Encapsulation

Once the handshake completes with `STATUS_OK`, both client and server transmit raw IEEE 802.3 Ethernet frames using 4-byte length prefixing:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                   Frame Length N (Bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                    Raw L2 Ethernet Frame                      +
|                     (14 to N Bytes...)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- **Length (`4 bytes`, `u32` Little-Endian):** Total byte length of the Ethernet frame that immediately follows.
- **Ethernet Frame:** Standard L2 Ethernet frame:
  - Destination MAC (`6 bytes`)
  - Source MAC (`6 bytes`)
  - EtherType (`2 bytes`, e.g. `0x0800` for IPv4, `0x0806` for ARP, `0x86DD` for IPv6)
  - Payload (`46` to `1500` bytes)

#### Frame Validation & L2 Rules
1. **Minimum Frame Size:** 14 bytes (header only). Frames under 14 bytes are considered runts and discarded.
2. **Maximum Frame Size:** 1514 bytes (or up to 2048 if negotiated). Frames larger than `MAX_DISCARD_FRAME` (65536) trigger an immediate disconnection.
3. **Source MAC Port Security:** The server enforces that the Source MAC of all outbound frames from the client matches the MAC address registered in the `ClientHello`. Spoofed frames are dropped.
4. **Local L2 Switching:** If client A sends a packet destined for client B's MAC address, the server switches the packet directly in-memory to client B without routing through the host kernel.
5. **Broadcast & Multicast:** Frames destined for broadcast (`ff:ff:ff:ff:ff:ff`) or multicast are flooded to all other connected clients as well as the upstream network.

---

## 5. Client Implementation Examples

### 5.1 Python 3 Example (Complete Client)

This standalone Python script connects to the Cube service, handshakes, and listens for broadcast packets (e.g. DHCP or ARP):

```python
#!/usr/bin/env python3
import socket
import struct
import os
import sys

SOCKET_PATH = os.path.expanduser("~/Library/Application Support/cube/vmnet.sock")
MAGIC = b"VMMNET\x01\x00"
CLIENT_MAC = bytes([0x52, 0x54, 0x00, 0x12, 0x34, 0x99])  # 52:54:00:12:34:99

def main():
    if not os.path.exists(SOCKET_PATH):
        print(f"Error: Cube vmnet socket not found at {SOCKET_PATH}")
        sys.exit(1)

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(SOCKET_PATH)

    # 1. Send ClientHello (24 bytes)
    # Format: 8s (magic), I (version=1), 6s (mac), H (flags=0), I (max_frame=1514)
    client_hello = struct.pack("<8sI6sHI", MAGIC, 1, CLIENT_MAC, 0, 1514)
    s.sendall(client_hello)

    # 2. Receive ServerHello (20 bytes)
    server_hello = s.recv(20)
    if len(server_hello) < 20:
        print("Handshake failed: server closed connection")
        sys.exit(1)

    magic, version, status, max_frame = struct.unpack("<8sIII", server_hello)
    if magic != MAGIC:
        print(f"Invalid server magic: {magic}")
        sys.exit(1)

    if status != 0:
        status_names = {
            1: "DUPLICATE_MAC",
            2: "UNSUPPORTED_VERSION",
            3: "REJECTED_MAC",
            4: "BAD_MAX_FRAME",
            5: "UNAUTHORIZED",
            6: "TOO_MANY_CLIENTS"
        }
        print(f"Connection rejected by server: {status_names.get(status, status)}")
        sys.exit(1)

    print(f"Connected to Cube vmnet switch! (Max frame: {max_frame} bytes)")

    # 3. Read loop: process length-prefixed frames
    while True:
        # Read 4-byte frame length
        len_bytes = s.recv(4)
        if not len_bytes:
            break
        (frame_len,) = struct.unpack("<I", len_bytes)

        # Read exact frame payload
        frame_data = b""
        while len(frame_data) < frame_len:
            chunk = s.recv(frame_len - len(frame_data))
            if not chunk:
                break
            frame_data += chunk

        if len(frame_data) >= 14:
            dst_mac = ":".join(f"{b:02x}" for b in frame_data[0:6])
            src_mac = ":".join(f"{b:02x}" for b in frame_data[6:12])
            ethertype = struct.unpack(">H", frame_data[12:14])[0]
            print(f"[Frame RX] {src_mac} -> {dst_mac} | EtherType: 0x{ethertype:04x} | Size: {frame_len}B")

if __name__ == "__main__":
    main()
```

---

### 5.2 Rust Client Example

```rust
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const MAGIC: &[u8; 8] = b"VMMNET\x01\x00";

pub struct VmnetClient {
    stream: UnixStream,
    max_frame: usize,
}

impl VmnetClient {
    pub fn connect(mac: [u8; 6]) -> Result<Self, Box<dyn std::error::Error>> {
        let home = std::env::var("HOME")?;
        let socket_path = PathBuf::from(home)
            .join("Library/Application Support/vmm/vmnet.sock");

        let mut stream = UnixStream::connect(socket_path)?;

        // 1. Send ClientHello
        let mut hello = [0u8; 24];
        hello[..8].copy_from_slice(MAGIC);
        hello[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
        hello[12..18].copy_from_slice(&mac);
        hello[18..20].copy_from_slice(&0u16.to_le_bytes()); // flags
        hello[20..24].copy_from_slice(&1514u32.to_le_bytes()); // max_frame
        stream.write_all(&hello)?;

        // 2. Receive ServerHello
        let mut reply = [0u8; 20];
        stream.read_exact(&mut reply)?;

        let status = u32::from_le_bytes(reply[12..16].try_into()?);
        if status != 0 {
            return Err(format!("Server rejected connection with status {status}").into());
        }

        let max_frame = u32::from_le_bytes(reply[16..20].try_into()?) as usize;
        Ok(Self { stream, max_frame })
    }

    pub fn send_frame(&mut self, frame: &[u8]) -> std::io::Result<()> {
        let len = (frame.len() as u32).to_le_bytes();
        self.stream.write_all(&len)?;
        self.stream.write_all(frame)?;
        Ok(())
    }

    pub fn recv_frame(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut len_bytes = [0u8; 4];
        self.stream.read_exact(&mut len_bytes)?;
        let frame_len = u32::from_le_bytes(len_bytes) as usize;
        if frame_len > buf.len() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Frame exceeds buffer"));
        }
        self.stream.read_exact(&mut buf[..frame_len])?;
        Ok(frame_len)
    }
}
```

---

## 6. Integration with Other Tools

### 6.1 QEMU Integration
Third-party virtual machines like QEMU can connect to Cube using a socket-based netdev helper or through a lightweight proxy daemon:
```bash
# Example QEMU invocation using a socket backend adapter
qemu-system-aarch64 \
  -M virt -cpu host -accel hvf \
  -netdev socket,id=net0,fd=3 \
  -device virtio-net-pci,netdev=net0,mac=52:54:00:12:34:57
```

### 6.2 Running Multiple VMs Concurrently
Multiple hypervisor instances can connect to the single `vmnet.sock` concurrently. The internal virtual switch manages all inter-VM packet routing automatically. Ensure each instance specifies a distinct MAC address (`--mac <unique-mac>`).
