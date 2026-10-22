# Verification

Host is macOS 10.15 or newer on Intel. `scripts/verify.py` uses the same gate
as the `vmm` binary. The marketing major comes from `kern.osproductversion`
when that sysctl exists. Otherwise Darwin 19 is macOS 10.15: Darwin 5 through
19 map to marketing 10, and Darwin 20 and later map to darwin − 9. The check
is marketing major >= 10. It does not compare the Darwin major to 26.

Rosetta is refused when `sysctl.proc_translated` is 1 or `hw.optional.arm64`
is 1. A missing sysctl means the process is not translated. `uname -m` is not
the check.

```sh
python3 -B scripts/verify.py
python3 -B scripts/verify.py --suite full
```

`--suite quick` (the default) runs `cargo +1.89.0 test --lib`, builds and
codesigns `target/debug/vmm`, then runs the payload smoke and the OVMF smoke.
`--suite full` adds `scripts/check_freebsd.py`. That smoke boots the FreeBSD
14.4 amd64 disc1 ISO with `--net vmnet-proxy` and does not fall back to
`--net none`. Start the service first (`make vmnet-service`) and run the full
suite as your user: the proxy rejects root.

`make fetch-uefi` and `make fetch-iso` download firmware and the ISO. Neither
file is committed. See [freebsd_amd64.md](freebsd_amd64.md).
