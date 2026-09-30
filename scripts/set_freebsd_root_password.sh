#!/usr/bin/env bash
# Set root's password on a retained FreeBSD install using its SSH key.
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
if [[ ${#positional[@]} -lt 1 || ${#positional[@]} -gt 3 ]]; then
    echo "Usage: $0 INSTALL_ARTIFACT_DIR [PASSWORD_FILE] [SSH_PORT] [--sudo]" >&2
    echo "Without PASSWORD_FILE, use the password in desktop-login.txt." >&2
    echo "--sudo reads root-owned install files. The default install is yours." >&2
    exit 2
fi

INSTALL=${positional[0]}
PASSWORD_FILE=${positional[1]:-}
PORT=${positional[2]:-2222}
as_root() {
    if [[ $USE_SUDO == true ]]; then
        sudo "$@"
    else
        "$@"
    fi
}
if [[ $USE_SUDO == true ]]; then
    sudo -v
fi

if [[ -n "$PASSWORD_FILE" ]]; then
    as_root test -s "$PASSWORD_FILE"
else
    as_root test -s "$INSTALL/desktop-login.txt"
fi

read_password() {
    if [[ -n "$PASSWORD_FILE" ]]; then
        as_root cat "$PASSWORD_FILE"
    else
        as_root sed -n 's/^Password: //p' "$INSTALL/desktop-login.txt"
    fi
}

PASSWORD=$(read_password)
if [[ -z "$PASSWORD" || "$PASSWORD" == *$'\n'* || "$PASSWORD" == *$'\r'* ]]; then
    echo "Password must contain one nonempty line" >&2
    exit 2
fi

printf '%s\n' "$PASSWORD" | as_root ssh -F /dev/null -i "$INSTALL/id_ed25519" -p "$PORT" \
    -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$INSTALL/root-known-hosts" \
    root@127.0.0.1 'pw usermod root -h 0'

printf 'Username: root\nPassword: %s\n' "$PASSWORD" | \
    as_root sh -c 'umask 077; cat > "$1"' sh "$INSTALL/root-login.txt"
echo "Root password updated; credentials saved in $INSTALL/root-login.txt"
