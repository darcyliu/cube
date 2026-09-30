#!/usr/bin/env bash
# Repair font/greeter and USB tablet setup on a retained FreeBSD XFCE guest.
set -euo pipefail

USE_SUDO=false
positional=()
for arg in "$@"; do
    if [[ $arg == "--sudo" ]]; then
        USE_SUDO=true
    else
        positional+=("$arg")
    fi
done
if [[ ${#positional[@]} -lt 1 || ${#positional[@]} -gt 2 ]]; then
    echo "Usage: $0 INSTALL_ARTIFACT_DIR [SSH_PORT] [--sudo]" >&2
    echo "--sudo is for a root-owned install. The default install is yours." >&2
    exit 2
fi

INSTALL=${positional[0]}
PORT=${positional[1]:-2222}
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
if [[ $USE_SUDO == true ]]; then
    sudo -v
    SSH=(sudo ssh -F /dev/null -i "$INSTALL/id_ed25519" -p "$PORT"
         -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new root@127.0.0.1)
else
    SSH=(ssh -F /dev/null -i "$INSTALL/id_ed25519" -p "$PORT"
         -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new root@127.0.0.1)
fi
"${SSH[@]}" 'env ASSUME_ALWAYS_YES=yes pkg install -y dejavu adwaita-icon-theme &&
    install -d /usr/local/etc/lightdm &&
    printf "%s\n" "[greeter]" "theme-name=Adwaita" \
        "icon-theme-name=Adwaita" "font-name=DejaVu Sans 11" \
        >/usr/local/etc/lightdm/lightdm-gtk-greeter.conf &&
    fc-cache -f && test -r /usr/local/share/fonts/dejavu/DejaVuSans.ttf'

"${SSH[@]}" 'cat > /usr/local/etc/rc.d/vmm_hid &&
    chmod 555 /usr/local/etc/rc.d/vmm_hid &&
    sysrc vmm_hid_enable=YES && service vmm_hid onestart' \
    < "$SCRIPT_DIR/freebsd-vmm-hid.rc"

"${SSH[@]}" 'service lightdm restart &&
    test "$(sysctl -n hw.usb.usbhid.enable)" = 1 &&
    test "$(sysctl -n kern.evdev.rcpt_mask)" = 6 &&
    test -r /usr/local/share/fonts/dejavu/DejaVuSans.ttf &&
    fc-match -f "%{family}\n" "DejaVu Sans" &&
    sleep 2 && usbconfig show_ifdrv | grep -E "hms[0-9]+"'
echo "Repair applied. Reconnect VNC at 127.0.0.1:5901 and log in as vmm."
