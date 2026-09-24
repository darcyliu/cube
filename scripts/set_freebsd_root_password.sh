#!/usr/bin/env bash
# Set root's password on a retained FreeBSD install using its SSH key.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 3 ]]; then
    echo "Usage: $0 INSTALL_ARTIFACT_DIR [PASSWORD_FILE] [SSH_PORT]" >&2
    echo "Without PASSWORD_FILE, use the password in desktop-login.txt." >&2
    exit 2
fi

INSTALL=$1
PASSWORD_FILE=${2:-}
PORT=${3:-2222}
sudo -v

if [[ -n "$PASSWORD_FILE" ]]; then
    sudo test -s "$PASSWORD_FILE"
else
    sudo test -s "$INSTALL/desktop-login.txt"
fi

read_password() {
    if [[ -n "$PASSWORD_FILE" ]]; then
        sudo cat "$PASSWORD_FILE"
    else
        sudo sed -n 's/^Password: //p' "$INSTALL/desktop-login.txt"
    fi
}

PASSWORD=$(read_password)
if [[ -z "$PASSWORD" || "$PASSWORD" == *$'\n'* || "$PASSWORD" == *$'\r'* ]]; then
    echo "Password must contain one nonempty line" >&2
    exit 2
fi

printf '%s\n' "$PASSWORD" | sudo ssh -F /dev/null -i "$INSTALL/id_ed25519" -p "$PORT" \
    -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$INSTALL/root-known-hosts" \
    root@127.0.0.1 'pw usermod root -h 0'

printf 'Username: root\nPassword: %s\n' "$PASSWORD" | \
    sudo sh -c 'umask 077; cat > "$1"' sh "$INSTALL/root-login.txt"
echo "Root password updated; credentials saved in $INSTALL/root-login.txt"
