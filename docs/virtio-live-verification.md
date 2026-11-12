# VirtIO live verification runbook

This runbook separates checks Cube can run automatically from checks that need
an operator to observe a guest, host memory, networking, or graphical firmware
output. Run the sections independently and stop a VM with `Ctrl-]`.

Commands assume the repository root and the prepared assets already present in
this checkout:

```text
debian-cloud.img
boot/debian/cloud-vmlinuz
boot/debian/cloud-initrd.img
cidata.iso
boot/freebsd/kernel
mini-memstick.img
firmware/QEMU_EFI.fd
```

## Result checklist

| Check | Live status at 2026-11-11 | Section |
|---|---|---|
| Virtio-rng guest attachment and `/dev/hwrng` data path | Pending dedicated live qualification | 2a |
| Basic virtio-fs mount/read/read-only/coherence | Passed | 1 |
| Basic guest-to-host vsock stream | Passed | 1 |
| Virtio-fs sustained reads | Passed: 4 workers, 60 seconds, zero errors | 3 |
| Block indirect descriptors | Passed: negotiated by FreeBSD; direct-plus-final-indirect and malformed forms covered by Rust tests | 4 |
| Block multi-queue correctness under `fio` | Passed: 4 queues/jobs, depth 32, CRC32C verified | 4 |
| Net indirect descriptors | Passed implementation tests for negotiated RX/TX split chains; sustained live traffic passed | 5 |
| Net multi-queue reverse TCP under `iperf3` | Passed zero-drop: P1/P2/P4/P8 = 2.71/2.46/2.72/2.81 Gbit/s | 5 |
| Net post-optimization forward TCP and UDP regression | Pending | 5 |
| Net five-minute soak and two-VM isolation | Pending | 5 |
| Balloon inflation and guest pressure | Passed in FreeBSD: 512 MiB target held during a 1 GiB md write | 6 |
| Balloon host-memory reclamation under pressure | Passed: physical footprint fell from 2.1 GiB peak to 1.6 GiB | 6 |
| Balloon deflation in the same VM | Passed: live `512 -> 256 -> 0 -> 512 MiB` cycle and 1.25 GiB readback | 6 |
| Host-to-guest/large/half-closed vsock stream | Passed: 64 MiB with matching SHA-256 | 7 |
| UEFI `VirtioGpuDxe` GOP output | Passed: FreeBSD installer visible over VNC after firmware scanout | 8 |

## 1. Automated baseline: virtio-fs and vsock

The verifier uses no guest NIC. It clones the Debian disk, tests a
guest-to-host vsock request/reply, mounts a read-only virtio-fs export, verifies
reads and symlink metadata, rejects a write, observes a host edit, and unmounts.

```sh
make verify-vsock-virtiofs
```

Pass:

```text
[verify] Virtio-vsock passed.
[verify] Virtio-fs passed.
PASS: vsock guest-to-host echo and virtio-fs read/RO/coherence; guest networking disabled
```

This passed on 2026-11-11. The script keeps its Unix socket below `/tmp` so it
also works when `verify.py` stores logs under a long artifact pathname.

## 2. One-time disposable Debian test guest

Keep benchmarks and package installation out of the canonical cloud image.
Create an APFS clone, with an ordinary copy as a fallback:

```sh
cp -c debian-cloud.img /tmp/cube-virtio-live.img || \
  cp debian-cloud.img /tmp/cube-virtio-live.img
```

Start the network service in a dedicated host terminal and leave it running:

```sh
make all
make vmnet-service
```

In another terminal, boot the clone:

```sh
target/debug/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 ip=dhcp systemd.unit=multi-user.target" \
  --disk /tmp/cube-virtio-live.img --disk cidata.iso \
  --net vmnet-proxy --cpus 4 --ram 2048
```

At the Debian root shell:

```sh
apt-get update
apt-get install -y fio iperf3 ethtool
sync
poweroff
```

The host needs an `iperf3` server for section 5. On macOS, install it with:

```sh
brew install iperf3
```

## 2a. Virtio-rng attachment and data-path check

Cube registers virtio-rng for every VM, but it has not yet received a dedicated
live qualification in this runbook. Boot the disposable Debian guest from
section 2, then confirm that Linux selected the VirtIO hardware RNG rather than
only proving that the generic random subsystem works:

```sh
cat /sys/class/misc/hw_random/rng_available
cat /sys/class/misc/hw_random/rng_current
readlink -f /sys/class/misc/hw_random/device/driver

dd if=/dev/hwrng of=/tmp/cube-rng-a bs=1m count=16 status=progress
dd if=/dev/hwrng of=/tmp/cube-rng-b bs=1m count=16 status=progress
sha256sum /tmp/cube-rng-a /tmp/cube-rng-b
test "$(stat -c %s /tmp/cube-rng-a)" -eq 16777216
test "$(stat -c %s /tmp/cube-rng-b)" -eq 16777216
! cmp -s /tmp/cube-rng-a /tmp/cube-rng-b
dmesg | grep -Ei 'virtio.*rng|virtio_rng|hwrng|error|timeout|reset'
```

Pass requires `rng_current` or the resolved driver path to identify
`virtio_rng`, both reads to complete at the requested length, different
digests, and no VirtIO error, timeout, or reset. Record the output here before
promoting virtio-rng from **implemented** to **live-verified** in the capability
matrix. FreeBSD may be used as additional coverage when its selected kernel
contains or can load the `vtrnd(4)` driver, but a successful `/dev/random` read
alone is insufficient because another entropy provider can satisfy it.

## 3. Virtio-fs sustained-read test

On the host, prepare a read-only export and record its digest:

```sh
mkdir -p /tmp/cube-virtiofs-export
dd if=/dev/urandom of=/tmp/cube-virtiofs-export/blob bs=1m count=512
shasum -a 256 /tmp/cube-virtiofs-export/blob
```

Boot the disposable Debian clone offline:

```sh
target/debug/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 systemd.unit=multi-user.target" \
  --disk /tmp/cube-virtio-live.img \
  --net none --cpus 4 --ram 2048 \
  --share cubecheck /tmp/cube-virtiofs-export \
  --share-transport virtiofs
```

Inside Debian:

```sh
mkdir -p /mnt/cubecheck
mount -t virtiofs -o ro cubecheck /mnt/cubecheck
sha256sum /mnt/cubecheck/blob

fio --name=virtiofs-read \
  --filename=/mnt/cubecheck/blob \
  --readonly --rw=randread --bs=4k \
  --ioengine=psync --numjobs=4 \
  --time_based=1 --runtime=60 \
  --group_reporting

! touch /mnt/cubecheck/must-fail
dmesg | grep -Ei 'virtio|fuse|error|timeout'
umount /mnt/cubecheck
```

Pass requires the guest SHA-256 to match the host, `fio` to report zero I/O
errors, the write to fail, and no FUSE/virtqueue timeout in `dmesg`.

### Confirmed result: 2026-11-11

This test passed on a live Debian 13 guest:

- The host and guest SHA-256 matched:
  `30bf9ed06d47744193550bcca2b5785b3525a056b567199709ebb42ec43a06ae`.
- Four `psync` workers completed 60 seconds of random 4 KiB reads with
  `err=0`, no short I/O, and no dropped I/O.
- The run read 4,732 MiB at 78.9 MiB/s (82.7 MB/s) and 20.2k IOPS. These are
  observations from this run, not a performance guarantee.
- Creating `must-fail` was rejected with `Read-only file system`.
- No FUSE or virtqueue timeout appeared, and the filesystem unmounted cleanly.
- `virtio_fs_setup_dax: No cache capability` is expected because Cube's
  virtio-fs implementation is deliberately non-DAX.

The `psync` engine has queue depth 1 per worker, so this result confirms four
concurrent synchronous readers rather than deep asynchronous queueing.

## 4. Block multi-queue `fio` correctness

This test overwrites its entire target. Create and use only this disposable
scratch disk:

```sh
truncate -s 4g /tmp/cube-fio-scratch.raw
```

Boot Debian with four block queues. Do not attach `cidata.iso`, so the scratch
disk is unambiguously `/dev/vdb`:

```sh
target/debug/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 systemd.unit=multi-user.target" \
  --disk /tmp/cube-virtio-live.img \
  --disk /tmp/cube-fio-scratch.raw \
  --blk-queues 4 --net none --cpus 4 --ram 2048
```

Inside Debian, verify the device identity and queue count before the destructive
command:

```sh
lsblk -o NAME,SIZE,MODEL
test "$(find /sys/block/vdb/mq -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 4

fio --name=blk-mq-verify \
  --filename=/dev/vdb \
  --ioengine=libaio --direct=1 --iodepth=32 \
  --rw=randwrite --bs=4k \
  --size=512m --numjobs=4 --offset_increment=768m \
  --serialize_overlap=1 \
  --verify=crc32c --do_verify=1 --verify_fatal=1 \
  --group_reporting

dmesg | grep -Ei 'virtio|blk|error|timeout|reset'
```

Pass requires four queue directories, all four jobs completing, zero verify
errors, and no block/virtqueue timeout.

### Confirmed result: 2026-11-11

This test passed on a live Debian 13 guest:

- `/dev/vdb` was the expected disposable 4 GiB disk.
- Linux reported `virtio_blk virtio1: 4/0/0 default/read/poll queues`, and four
  `/sys/block/vdb/mq` queue directories were present.
- Four `libaio` jobs ran at queue depth 32. The CRC32C verification phase
  completed with `err=0`, no short I/O, and no dropped I/O.
- The run wrote 2,048 MiB at 75.2 MiB/s (78.9 MB/s), then verified by reading
  2,048 MiB at 275 MiB/s (288 MB/s). These are observations from this run, not
  a performance guarantee.
- No block or virtqueue timeout appeared in `dmesg`.

Fio printed its conservative warning about multiple writers sharing one raw
device. The configured 512 MiB job size and 768 MiB offset increment give the
four jobs disjoint ranges; the readback verification also completed without an
error. `--serialize_overlap=1` is included above to make that invariant explicit
for future runs.

Repeated `hvc0` login/password messages are output from a second serial getty,
not block-I/O failures. They can be silenced for a cleaner benchmark with:

```sh
systemctl stop serial-getty@hvc0.service
```

Remove the scratch image afterward:

```sh
rm -f /tmp/cube-fio-scratch.raw
```

## 5. Network multi-queue `iperf3`

Keep `make vmnet-service` running. In another host terminal:

```sh
iperf3 -s
```

Boot Debian with four network queue pairs:

```sh
target/debug/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 ip=dhcp systemd.unit=multi-user.target" \
  --disk /tmp/cube-virtio-live.img \
  --net vmnet-proxy --net-queue-pairs 4 \
  --cpus 4 --ram 2048
```

Inside Debian:

```sh
ethtool -L eth0 combined 4
ethtool -l eth0

RXQ=$(find /sys/class/net/eth0/queues -name 'rx-*' | wc -l | tr -d ' ')
TXQ=$(find /sys/class/net/eth0/queues -name 'tx-*' | wc -l | tr -d ' ')
printf 'RX queues: %s; TX queues: %s\n' "$RXQ" "$TXQ"
test "$RXQ" -eq 4 && test "$TXQ" -eq 4

HOST=$(ip -4 route show default | awk '{print $3; exit}')
iperf3 -c "$HOST" -P 8 -t 60
iperf3 -c "$HOST" -P 8 -t 60 -R
iperf3 -c "$HOST" -u -b 500M -t 30

ip -s link show eth0
dmesg | grep -Ei 'virtio|net|error|timeout|reset'
```

Pass requires four RX and four TX queues, successful forward and reverse TCP
runs, a completed UDP run with reported loss rather than a hang, and no queue
timeout. Throughput by itself does not prove multi-queue negotiation.

### Confirmed result: 2026-11-11

This test passed on a live Debian 13 guest connected through `vmnet-proxy`:

- `ethtool` reported a maximum and current `Combined: 4`, with four RX and four
  TX queue directories in sysfs.
- Eight parallel iperf3 TCP streams connected, and the forward and reverse runs
  completed. The retained reverse result transferred 4.14 GiB in 30 seconds at
  1.18 Gbit/s, with matching sender and receiver totals.
- The reverse sender reported 104,402 TCP retransmissions. That does not negate
  data-path correctness, but it is high enough that this is not a clean
  performance-quality sign-off.
- The completed tests moved 4,688,912,409 bytes into the guest and
  5,680,939,474 bytes out of the guest.
- The 30-second 500 Mbit/s UDP request delivered 1.36 GiB at 391 Mbit/s, with
  0.017 ms reported jitter and 1,846/1,012,892 datagrams lost (0.18%).
- The guest reported zero RX/TX errors, four RX drops under the combined load,
  and zero TX drops. No virtio-net timeout or reset appeared in `dmesg`.

This confirms queue negotiation and sustained traffic in both directions. It
does not establish a performance SLA or prove perfectly even traffic
distribution across all four queue pairs; that would require per-queue byte or
packet counters. Before using the reverse rate as a benchmark, compare `-P 1`,
`-P 2`, `-P 4`, and `-P 8` and inspect the vmnet-service counter line for
`drop_slow_client`, `vmnet_err`, or oversize frames.

The follow-up isolated the retransmissions to the proxy service. A one-stream
reverse run reached 1.44 Gbit/s with 6,764 retransmissions; two streams reached
1.42 Gbit/s with 31,319 retransmissions. During the load, Cube reported zero
`rx_drop`, `rx_nomem`, `vmnet_err`, `sock_drop`, and backlog, while the service
reported `drop_slow_client=32659` with all frame-validation and vmnet-error
counters at zero. The service was draining an entire vmnet burst before
flushing its 64-frame per-client output queue. An initial change drained in
32-frame batches and flushed between batches, but a live retest still reached
`drop_slow_client=36238`: during sustained load the Unix socket returned
`EAGAIN`, and subsequent batches refilled the bounded queue. The revised fix
stops reading vmnet whenever any live client's output remains pending, records
that vmnet still has unread packets, and resumes the drain after `POLLOUT`.
This revision passed all 12 focused `vmnet_service` tests and the following
fresh-service live scale test.

### Confirmed post-fix result: 2026-11-11

The revised backpressure fix passed its live retest:

- Two reverse TCP streams transferred 3.01 GiB in 30 seconds at 861 Mbit/s.
- The sender reported only 11 retransmissions: 3 on one stream and 8 on the
  other. Sender and receiver byte totals matched.
- After 4,237,879 host-to-guest frames and 425,047 guest-to-host frames, the
  service reported zero `drop_slow_client`, `vmnet_err`, oversize, runt,
  unclaimed, reflection, and spoof counters.
- Compared with the pre-fix P2 run, retransmissions fell from 31,319 to 11 and
  service drops fell from 36,238 to zero. Throughput fell from 1.42 Gbit/s to
  861 Mbit/s because the proxy now propagates socket backpressure instead of
  discarding bursts.

Together with the previously confirmed four RX/TX queues, this is a clean
functional and sustained-load pass for virtio-net multi-queue through
`vmnet-proxy`. The measured rate is an observation, not a performance SLA, and
further throughput tuning must preserve the zero-drop behavior.

### Throughput optimization retest

Cube now requests 1 MiB Unix-socket buffers at both endpoints, reads proxy data
in 64 KiB chunks, delivers decoded records in bounded batches, and flushes up
to 32 service records with one vectored write.

The first optimized release build produced these reverse-TCP results:

| Streams | Bitrate | Retransmissions |
|---:|---:|---:|
| 1 | 4.77 Gbit/s | 200 |
| 2 | 2.40 Gbit/s | 165 |
| 4 | 3.19 Gbit/s | 336 |
| 8 | 2.52 Gbit/s | 973 |

The service accepted effective buffers of 1,048,596 send bytes and 1,048,576
receive bytes. Across the retained cumulative counter line, it handled
36,719,132 frames with 1,570,141 writes, or about 23.4 frames per write, with
zero service drops, errors, or blocked writes. Cube decoded 36,719,097 records
in 1,474,525 reads, or about 24.9 records per read.

That run also exposed the next boundary: Cube delivered 36,710,040 records and
dropped 9,057, exactly the difference from the decoded-record count, while
`rx_nomem` reached 619,113. The proxy RX worker now retains an undeliverable
record in a bounded thread-local queue and stops reading the socket until the
guest replenishes descriptors.

The first zero-drop refinement then produced 2.71, 2.46, 2.72, and 2.81 Gbit/s
for P1/P2/P4/P8. All proxy and Cube drop/error counters remained zero;
`rx_nomem` fell to 38,499 and the local pending high-water mark was 46 records.
The attempted 32-record/no-sleep tuning raised P1 slightly to 2.88 Gbit/s but
reduced P4/P8 to 1.26/1.04 Gbit/s. It retried empty virtqueues 35,729,897 times
and drove service blocked writes to 958,223, showing that the proxy worker was
contending continuously with the vCPU that needed to replenish descriptors.
The selected configuration is therefore the measured zero-drop 16-record
batch with a 1 ms no-progress backoff: 2.71/2.46/2.72/2.81 Gbit/s for
P1/P2/P4/P8, with only 38,499 empty-descriptor probes. This favors sustained
multi-stream throughput and bounded CPU use over the small P1 gain.

### Remaining network release checks

The retained zero-drop optimization results above exercise reverse TCP, where
the host sends and the guest receives. Before release, rerun the other traffic
directions against the final binary and a freshly restarted service. These are
regression checks, not new throughput targets.

Start the server on the host and leave it running:

```sh
iperf3 -s -i 5
```

Inside the Debian guest, run forward TCP at each stream count followed by UDP
in both directions. Keep the ASCII `$3` exactly as shown:

```sh
HOST=$(ip -4 route show default | awk '{print $3; exit}')
test -n "$HOST"

for P in 1 2 4 8; do
  printf '\n===== forward TCP: %s stream(s) =====\n' "$P"
  iperf3 -c "$HOST" -P "$P" -t 30 -O 3
done | tee /tmp/iperf3-forward-scale.log

iperf3 -c "$HOST" -u -b 500M -t 30 | tee /tmp/iperf3-udp-forward.log
iperf3 -c "$HOST" -u -b 500M -t 30 -R | tee /tmp/iperf3-udp-reverse.log

ip -s -s link show dev eth0
dmesg | grep -Ei 'virtio|net|error|timeout|reset'
```

All six runs must finish. Sender and receiver totals must be consistent, UDP
must report loss rather than hang, and the guest must not report a virtio-net
timeout or reset. Retain the next service counter line and Cube's final counter
line. Require `drop_slow_client=0`, `sock_drop=0`, `rx_drop=0`, `vmnet_err=0`,
and zero frame-validation errors. `rx_nomem` may increase transiently because
it counts an empty guest receive ring, but the retained-record path must
eventually deliver the frames and keep the actual drop counters at zero.

Next, run a five-minute reverse-TCP soak while checking control-traffic
latency. In guest terminal A:

```sh
HOST=$(ip -4 route show default | awk '{print $3; exit}')
iperf3 -c "$HOST" -P 4 -t 300 -O 3 -R | tee /tmp/iperf3-reverse-soak.log
```

In guest terminal B, or over SSH to the same guest:

```sh
HOST=$(ip -4 route show default | awk '{print $3; exit}')
ping -i 0.1 "$HOST" | tee /tmp/ping-during-soak.log
```

Stop `ping` after the load finishes. Pass requires the full 300-second transfer,
continued ping responses, no device reset or deadlock, zero actual drop/error
counters, and a bounded `proxy_rx_pending_max` rather than growth for the whole
run. Record CPU use and retransmissions for comparison, but neither is a
correctness failure by itself when all delivery counters remain clean.

Finally verify service isolation with two VMs. Use separate disk clones and
distinct MAC addresses; never boot the same writable image twice:

```sh
cp -c debian-cloud.img /tmp/cube-net-a.img || cp debian-cloud.img /tmp/cube-net-a.img
cp -c debian-cloud.img /tmp/cube-net-b.img || cp debian-cloud.img /tmp/cube-net-b.img
```

Start each command in its own terminal, using the same Linux boot arguments as
section 2:

```sh
target/release/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 ip=dhcp systemd.unit=multi-user.target" \
  --disk /tmp/cube-net-a.img --disk cidata.iso,ro \
  --net vmnet-proxy --net-queue-pairs 4 \
  --mac 52:54:00:12:34:a1 --cpus 4 --ram 2048
```

```sh
target/release/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 ip=dhcp systemd.unit=multi-user.target" \
  --disk /tmp/cube-net-b.img --disk cidata.iso,ro \
  --net vmnet-proxy --net-queue-pairs 4 \
  --mac 52:54:00:12:34:b1 --cpus 4 --ram 2048
```

In each guest, record `ip -4 address show eth0`; the DHCP addresses must be
different. Both guests must ping the gateway and guest A must ping guest B's
address. While guest A runs a four-stream, five-minute `iperf3` load, guest B
must continue pinging the gateway and guest A. The service must retain both
clients and report no `drop_slow_client`, spoof, reflection, unclaimed, or
vmnet errors. As a final negative check, start a third disposable clone with
one of the same MAC addresses and require Cube/service to reject the duplicate
before that guest starts. This is the live counterpart to the local-switch,
demultiplexing, and duplicate-MAC unit tests.

Rebuild and restart both processes, preferably using the release binary:

```sh
make DIST=1 all
# Stop the old service with Ctrl-C.
make DIST=1 vmnet-service
```

Start the VM with `target/release/cube`, then repeat the P1/P2/P4/P8 reverse
sequence below. Retain the service counter line and the last Cube counter line:

```sh
tail -n 1 /tmp/vmm-net.log
```

Require `drop_slow_client=0`, `sock_drop=0`, `rx_drop=0`, and `vmnet_err=0`.
Compare `socket_write_calls` with `rx_frames`: successful batching should make
write calls materially lower than frames. Likewise, `proxy_rx_records` divided
by `proxy_rx_reads` shows how many records Cube handles per read. Report the
effective `sndbuf` and `rcvbuf` values printed at attach time, since macOS may
cap the requested sizes.

### Follow-up: attribute reverse TCP retransmissions

Run the host server with five-second interval reports:

```sh
iperf3 -s -i 5
```

In a second host terminal, follow Cube's per-second network counters:

```sh
tail -f /tmp/vmm-net.log
```

Also retain the `[vmnet]` counter line emitted by the `make vmnet-service`
terminal every 60 seconds. Inside the Debian guest, capture the baseline and
then scale only the reverse TCP workload:

```sh
HOST=$(ip -4 route show default | awk '{print $3; exit}')

ip -s -s link show dev eth0
ethtool -S eth0 | grep -Ei 'queue|drop|error|packet|byte'
grep -Ei 'virtio|eth0' /proc/interrupts
nstat -az | grep -E 'TcpRetransSegs|TcpExtTCPLostRetransmit|TcpExtTCPTimeouts'

for P in 1 2 4 8; do
  printf '\n===== reverse TCP: %s stream(s) =====\n' "$P"
  iperf3 -c "$HOST" -P "$P" -t 30 -O 3 -R
done | tee /tmp/iperf3-reverse-scale.log

ip -s -s link show dev eth0
ethtool -S eth0 | grep -Ei 'queue|drop|error|packet|byte'
grep -Ei 'virtio|eth0' /proc/interrupts
nstat -az | grep -E 'TcpRetransSegs|TcpExtTCPLostRetransmit|TcpExtTCPTimeouts'
```

Use the final `SUM` bitrate and `Retr` column from each run. Interpret the
combined evidence as follows:

| Evidence | Likely boundary |
|---|---|
| Retransmits rise sharply with `-P 4`/`-P 8`, and `sock_drop`, `rx_drop`, `rx_nomem`, backlog, or `drop_slow_client` rises | Cube/proxy receive backpressure |
| `vmnet_err`, `rx_oversize`, or `frame_oversize` rises | vmnet write failure or frame-size handling |
| Cube and service drop counters stay at zero, but host iperf still reports high retransmits | macOS vmnet/NAT or host TCP scheduling; not evidence of lost virtqueue descriptors |
| Per-queue packet/interrupt counters remain concentrated on one queue with eight flows | Multi-queue flow steering needs investigation |
| All four queues advance, drops remain zero, and rates scale without a retransmit spike | Clean multi-queue performance pass |

For the cleanest comparison, reboot the guest before this sequence and avoid
running the forward and UDP loads first. If proxy counters rise, repeat the
same scale test using the in-process `--net vmnet` backend in a separate run;
an improvement there isolates the Unix-socket proxy/backpressure path.

After applying the batching fix, rebuild and restart the privileged service;
an already-running service still contains the old loop:

```sh
make all
# Stop the old make vmnet-service process with Ctrl-C, then restart it:
make vmnet-service
```

For future regression checks, start a fresh VM, rerun at least the `-P 1` and
`-P 2` reverse cases, and wait for the next 60-second service counter line.
Require `drop_slow_client` to remain zero (or not increase from its startup
value). Record bitrate, retransmissions, and transferred GiB for comparison;
TCP retransmissions need not be literally zero on a NAT path.

## 6. FreeBSD balloon inflation and pressure

Start the VM with 2 GiB preallocated RAM and a 512 MiB target:

```sh
make freebsd-gpu-balloon NET=none RAM_MB=2048 BALLOON_MB=512
```

Inside FreeBSD:

```sh
kldload virtio_balloon
kldstat -n virtio_balloon.ko

while :; do
  sysctl dev.vtballoon.0.desired dev.vtballoon.0.current
  sleep 1
done
```

For 512 MiB, both values must converge to `131072` 4 KiB pages. Stop the loop
with `Ctrl-C`. This was confirmed live on 2026-11-11: the driver module loaded
and both `dev.vtballoon.0.desired` and `dev.vtballoon.0.current` repeatedly
reported `131072`.

On the host, record Cube's resident size, physical footprint, and reusable
memory while the target converges:

```sh
PID=$(pgrep -n cube)
ps -o pid,rss,vsz,command -p "$PID"
vmmap -summary "$PID" | grep -Ei 'physical footprint|reusable|resident|TOTAL'
footprint "$PID" | grep -Ei 'physical footprint|reusable|resident'
```

If macOS denies task inspection, repeat `vmmap` or `footprint` with `sudo`.
Cube uses `MADV_FREE_REUSABLE` for fully inflated 16 KiB host pages. Such pages
may remain in the resident set until macOS needs them, so an unchanged `ps` RSS
does not by itself mean that reclamation failed. In the confirmed run, RSS
remained near 2.15 GiB after guest inflation; host reclamation therefore remains
unconfirmed until the footprint/reusable values are captured under pressure.

Apply 1 GiB of kernel-memory pressure inside FreeBSD without extra packages:

```sh
UNIT=$(mdconfig -a -t malloc -s 1024m)
dd if=/dev/zero of=/dev/$UNIT bs=1m status=progress
sysctl dev.vtballoon.0.desired dev.vtballoon.0.current
mdconfig -d -u "${UNIT#md}"
```

### Confirmed guest result: 2026-11-11

The 1 GiB pressure write completed in a live FreeBSD guest while the balloon
remained fully inflated:

- `dd` wrote all 1,024 MiB (`1073741824` bytes).
- The final `dd: /dev/md0: end of device` diagnostic is expected after filling
  the fixed-size md device and is not an I/O failure.
- After the write, `desired == current == 131072` pages (512 MiB).
- The md device detached successfully.

This confirms guest operation under pressure. By itself it does not prove that
macOS reclaimed the pages marked reusable, so the host result is recorded
separately below.

### Confirmed host result: 2026-11-11

The corresponding macOS measurements confirmed host reclamation:

- Cube's physical footprint was 1.6 GiB after inflation and pressure, compared
  with a reported peak of 2.1 GiB: a reduction of approximately 512 MiB.
- Guest `desired` and `current` remained at 131,072 pages during this test, so
  the footprint reduction matches the configured 512 MiB balloon target.
- Process RSS remained near 2.15 GiB, as expected for memory marked with
  `MADV_FREE_REUSABLE`; RSS is therefore not used as the pass/fail metric.
- The larger aggregate resident totals printed by `vmmap` include mappings and
  shared regions and do not override the task's physical-footprint result.

Together with the completed FreeBSD pressure write, this passes inflation and
host-memory reclamation. Same-VM deflation is covered separately below using
the runtime target-update interface and a complete deflate/reinflate cycle.

Capture `vmmap -summary` or `footprint` before inflation, after inflation, and
while the write is active. For a clean A/B comparison, repeat the run with
`BALLOON_MB=0`. Pass requires `current == desired`, completion of the
memory-pressure write, no guest panic or balloon timeout, and either a lower
physical footprint or more reusable memory than the otherwise identical
zero-balloon run. Record RSS as supporting data, but do not require it to fall
immediately.

### Handling same-VM balloon deflation

Cube's balloon device implements both VirtIO inflate and deflate queues.
On deflation it calls `MADV_FREE_REUSE` before completing the guest's deflate
descriptor, which is the ordering required by the negotiated
`VIRTIO_BALLOON_F_MUST_TELL_HOST` feature. `--balloon-control PATH` now exposes
a runtime Unix socket that updates `set_target_pages` after the device boots.
Do not simulate deflation by restarting the VM with a smaller target: that does
not exercise the deflate queue or page-reactivation ordering.

The implemented control path has these properties:

1. It registers `VirtioBalloon` through a typed `Arc<Mutex<VirtioBalloon>>` and
   retains a clone as its control handle. The MMIO bus and runtime controller
   must use the same mutex; do not mutate device state through a second copy.
2. It creates a mode-0600, user-owned per-VM Unix socket, for example
   `--balloon-control /tmp/cube-balloon.sock`. It accepts bounded newline-delimited
   JSON requests such as `{"set_mib":0}` and `{"status":true}`, and returns the
   requested target, currently advertised target (`num_pages`), guest-reported
   `actual`, and host-held page count. Reject unknown fields, oversized input,
   and negative or non-integral values. Clamp a target above reclaimable guest
   RAM and report both the requested and applied values in the response.
3. It treats the requested target separately from the currently advertised target.
   Move toward it in the existing 64 MiB steps, and do not advertise the next
   step until the guest's `actual` value has converged to the current step (or a
   bounded timeout has produced a visible error). This prevents rapid commands
   from bypassing the rate limit and avoids inflate/deflate thrashing.
4. When the target changes, it updates `num_pages`, raises the VirtIO configuration
   interrupt, kicks a vCPU, and lets the guest choose PFNs through the normal
   inflate or deflate queue. Never reclaim or reactivate arbitrary pages solely
   from the host command.
5. Socket creation follows the vsock control socket model: reject a live existing
   listener, replace only a safely identified stale socket, restrict access to
   its owner, remove it on clean shutdown, and terminate the controller when
   the VM exits.

Unit tests cover target clamping, 64 MiB stepping, waiting for `actual`, the
bounded JSON protocol, socket cleanup, duplicate and partial 16 KiB host-page
groups, invalid/protected PFNs, and repeated PFNs. The live cycle below remains
required because it verifies the FreeBSD driver and Darwin reclamation together.

Run the following to create `/tmp/cube-balloon.sock` and boot FreeBSD with
2 GiB RAM and a 512 MiB target:

```sh
make freebsd-gpu-balloon NET=none RAM_MB=2048 BALLOON_MB=512
```

After `desired` and `current` converge at 131,072 pages, record the host
footprint and lower the live target first to 256 MiB and then to zero:

```sh
python3 scripts/balloonctl.py set 256 --wait
python3 scripts/balloonctl.py status
python3 scripts/balloonctl.py set 0 --wait
```

For a non-default socket, put `--socket PATH` before the command, for example
`python3 scripts/balloonctl.py --socket /tmp/my-balloon.sock status`. The helper
uses `shutdown(SHUT_WR)` explicitly and avoids incompatible `nc` option meanings
across macOS releases. Do not issue the next target until the preceding command
reports `"driver_ok": true` and `"converged": true`. If `driver_ok` is false,
load `virtio_balloon` in the guest before continuing.

In FreeBSD, watch convergence throughout:

```sh
while :; do
  sysctl dev.vtballoon.0.desired dev.vtballoon.0.current
  sleep 1
done
```

The intermediate values should converge to 65,536 pages and then zero. After
zero is reached, allocate and verify most of the returned memory rather than
expecting old balloon-page contents to survive; ballooned pages are owned by
the driver and their previous contents are not a guest ABI guarantee:

```sh
UNIT=$(mdconfig -a -t malloc -s 1280m)
newfs -U /dev/$UNIT
mkdir -p /mnt/balloon-check
mount /dev/$UNIT /mnt/balloon-check
dd if=/dev/urandom of=/mnt/balloon-check/blob bs=1m count=1280 status=progress
DIGEST=$(sha256 -q /mnt/balloon-check/blob)
sync
test "$DIGEST" = "$(sha256 -q /mnt/balloon-check/blob)"
rm /mnt/balloon-check/blob
umount /mnt/balloon-check
mdconfig -d -u "${UNIT#md}"
```

Pass requires `desired == current == 0`, completion of the allocation/readback,
no guest panic or balloon timeout, and no Cube error. Host physical footprint
may rise lazily as returned pages are touched, so footprint growth is supporting
evidence, not an immediate pass condition. Also repeat `512 -> 0 -> 512 MiB` to
exercise a complete deflate/reinflate cycle and require `held_pages` to track
the guest's `actual` value without underflow or leaked reusable host-page
groups.

### Confirmed deflation result: 2026-11-11

The runtime controller and FreeBSD 14.4 balloon driver completed the live test:

- The initial 512 MiB target converged at `desired=current=131072`, with Cube
  reporting `target_pages=actual_pages=held_pages=131072`.
- A 256 MiB request was advertised in 64 MiB steps and converged at
  `desired=current=65536`; Cube reported all three counters at 65,536.
- A zero target converged at `desired=current=0`; the subsequent reinflation to
  512 MiB returned all three Cube counters to 131,072.
- After a second deflation to zero, FreeBSD allocated a 1,280 MiB malloc-backed
  md device, wrote its full capacity, and produced the same SHA-256 on two full
  readbacks: `b857ea49e0b2bcc4eb7a44e44b1d24a62cbd386a3c20c5ffe0c4e9772022d082`.
- No guest panic, balloon timeout, device reset, or Cube error occurred. Cube
  removed the control socket during clean shutdown.

This passes same-VM deflation, returned-memory usability, and a complete
deflate/reinflate cycle. A longer repeated-cycle soak remains useful robustness
coverage but is no longer a basic feature blocker.

The public `balloonctl.py` sequence was repeated after adding explicit
convergence reporting. Each `512 -> 256 -> 0 -> 512 MiB` endpoint returned
`"ok": true`, `"driver_ok": true`, `"converged": true`, and equal
`requested_pages`, `target_pages`, `actual_pages`, and `held_pages`. The page
counts were respectively 131,072, 65,536, zero, and 131,072. This independently
confirms that `--wait` observes guest completion rather than merely accepting a
host target request.

## 7. Extended bidirectional vsock stream

Boot Debian with a short host control socket:

```sh
rm -f /tmp/cube-vsock-control.sock
target/debug/cube \
  --linux boot/debian/cloud-vmlinuz \
  --initrd boot/debian/cloud-initrd.img \
  --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 systemd.unit=multi-user.target" \
  --disk /tmp/cube-virtio-live.img \
  --net none --cpus 2 --ram 1024 \
  --vsock-cid 42 --vsock-listen /tmp/cube-vsock-control.sock
```

Inside Debian, start a one-shot guest server:

```sh
python3 - <<'PY'
import hashlib, socket
s = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
s.bind((socket.VMADDR_CID_ANY, 6000))
s.listen(1)
c, peer = s.accept()
h = hashlib.sha256()
while True:
    data = c.recv(65536)
    if not data:
        break
    h.update(data)
c.sendall(h.hexdigest().encode() + b'\n')
c.shutdown(socket.SHUT_WR)
c.close()
s.close()
print('GUEST_VSOCK_OK', peer, h.hexdigest(), flush=True)
PY
```

While that command waits, run this on the host:

```sh
python3 - <<'PY'
import hashlib, socket

def line(sock):
    data = b''
    while not data.endswith(b'\n'):
        part = sock.recv(1)
        if not part:
            raise RuntimeError('unexpected EOF')
        data += part
    return data

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect('/tmp/cube-vsock-control.sock')
s.sendall(b'connect 6000\n')
assert line(s) == b'OK\n'
chunk = bytes(range(256)) * 256
h = hashlib.sha256()
for _ in range(1024):
    s.sendall(chunk)
    h.update(chunk)
s.shutdown(socket.SHUT_WR)
actual = line(s).strip().decode()
assert actual == h.hexdigest(), (actual, h.hexdigest())
assert s.recv(1) == b''
print('HOST_VSOCK_64M_HALF_CLOSE_OK', actual)
PY
```

Pass requires both `HOST_VSOCK_64M_HALF_CLOSE_OK` and `GUEST_VSOCK_OK`. This
adds host-to-guest connection initiation, a 64 MiB transfer, integrity checking,
and a half-close to the basic automated guest-to-host test.

### Confirmed result: 2026-11-11

The extended stream passed on a live Debian guest:

- The host initiated the connection and sent the complete 64 MiB stream.
- Host and guest produced the same SHA-256:
  `281e519df3077b557c6b03f5da83c4e8d397219259615dd7c3308f89cae8f2a6`.
- The guest identified its peer as host CID 2, with peer port 1073741824.
- The host received the digest after shutting down its write side and then
  observed EOF, confirming half-close behavior in both directions.
- Both required completion markers, `HOST_VSOCK_64M_HALF_CLOSE_OK` and
  `GUEST_VSOCK_OK`, were emitted.

## 8. UEFI `VirtioGpuDxe` A/B test

This check needs a viewer. Install TigerVNC if no VNC client is available:

```sh
brew install --cask tigervnc
```

First run the control without a virtio-gpu device. Use `--gpu simple`, not
`--gpu none`: VNC requires a framebuffer, while `simple` supplies the VNC
fallback without registering a virtio-gpu device.

```sh
target/debug/cube \
  --uefi firmware/QEMU_EFI.fd \
  --gpu simple --vnc 127.0.0.1:5998 \
  --display 1280x720 --no-reboot \
  --serial-log /tmp/cube-uefi-simple-control.log
```

Connect to `127.0.0.1::5998`, record whether firmware output appears, then stop
Cube. Run the virtio-gpu case:

```sh
target/debug/cube \
  --uefi firmware/QEMU_EFI.fd \
  --gpu virtio --vnc 127.0.0.1:5999 \
  --display 1280x720 --no-reboot \
  --serial-log /tmp/cube-uefi-virtio-gpu.log
```

Connect to `127.0.0.1::5999` before any operating-system kernel starts. Both
runs have VNC fallback memory, but only the second registers a virtio-gpu
device. The diskless firmware may mode-set and clear GOP without routing a
visible text console to it, so a black virtio-gpu framebuffer is not sufficient
to decide whether `VirtioGpuDxe` bound.

Cube emits definitive device-side evidence after a successful `SET_SCANOUT`
and first `RESOURCE_FLUSH`:

```text
[VirtIO-GPU] scanout active: 1280x720 resource 2 received first RESOURCE_FLUSH
```

Pass for driver binding and scanout initialization requires this diagnostic and
a shutdown line with nonzero, matching `notifies` and `completions`, with
`malformed 0`. Visible non-black pixels are a separate firmware-console test.

### Confirmed result: 2026-11-11

The simple-framebuffer control remained black. Initially, the virtio-gpu run
also stalled before its first command because this EDK2 `VirtioGpuDxe` modern
MMIO path wrote `QueueNum=0` before enabling an otherwise valid control ring.
Cube now applies a GPU-only compatibility rule that retains the advertised
queue maximum for this specific write; all other VirtIO devices keep strict
queue-size validation.

After the fix:

- EDK2 negotiated VirtIO version 1 and configured the control ring.
- Cube reported a 1280×720 scanout on resource 2 and received its first flush.
- Shutdown diagnostics reported 10 notifications, 10 completions, 624 response
  bytes, zero drops, and zero malformed requests.
- An automated full-frame VNC read contained only black pixels. This confirms
  that the diskless firmware did not draw visible console content; it does not
  negate the successful `VirtioGpuDxe` binding and scanout sequence.

### Confirmed visible GOP result: 2026-11-11

The same UEFI virtio-gpu configuration was then booted with the FreeBSD 16
ARM64 installer ISO attached as a read-only CD-ROM. Cube reported successful
1280×720 scanout flushes on resource 2 and, after the boot-path reinitialization,
resource 1. VNC displayed the FreeBSD installation screen. This confirms the
complete path from `VirtioGpuDxe`/GOP rendering through Cube's virtio-gpu
resource transfer and scanout publication to the VNC frontend.

The verified command form is:

```sh
target/debug/cube \
  --uefi firmware/QEMU_EFI.fd \
  --cdrom <FreeBSD-16-ARM64-disc1.iso> \
  --gpu virtio --vnc 127.0.0.1:5999 \
  --display 1280x720 --no-reboot \
  --serial-log /tmp/cube-uefi-freebsd-gpu.log
```

## Cleanup

After all sections:

```sh
rm -f /tmp/cube-virtio-live.img
rm -rf /tmp/cube-virtiofs-export
rm -f /tmp/cube-fio-scratch.raw /tmp/cube-vsock-control.sock
```

Stop the foreground vmnet service with `Ctrl-C`.
