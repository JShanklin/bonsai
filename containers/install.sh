#!/usr/bin/env bash
# Sets up a virtual board once; systemd keeps it running and starts it at boot.
#   sudo ./install.sh <board>          install, or update after editing the Containerfile
#   sudo ./install.sh <board> remove   stop it and delete everything it made
# Boards are the files in boards/: pi5, zero-2w, zero-w.
# Overrides: VIRTUAL_PI_LAN_DEV (network interface), VIRTUAL_PI_LAN_IP (its LAN address).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
units=/etc/containers/systemd

say() { echo "virtual-$board: $*"; }
die() { echo "virtual-${board:-board}: $*" >&2; exit 1; }

board="${1:-}"
boards="$(cd "$here/boards" && ls ./*.conf | sed 's|^\./||; s|\.conf$||' | tr '\n' ' ')"
[ -n "$board" ] || die "which board? one of: $boards"
[ -f "$here/boards/$board.conf" ] || die "no board '$board'; one of: $boards"
# shellcheck source=/dev/null
. "$here/boards/$board.conf"
name="virtual-$board"

[ "$(id -u)" = 0 ] || die "run it with sudo"
user="${SUDO_USER:-}"
[ -n "$user" ] && [ "$user" != root ] || die "run it with sudo from your own account (it uses your ssh key)"
home="$(getent passwd "$user" | cut -d: -f6)"
command -v podman >/dev/null || die "install podman first"
command -v systemctl >/dev/null || die "needs systemd, which runs the virtual board"

if [ "${2:-}" = remove ]; then
    systemctl stop "$name.service" 2>/dev/null || true
    rm -f "$units/$name.container"
    # The shared networks go with the last board.
    if ! ls "$units"/virtual-*.container >/dev/null 2>&1; then
        rm -f "$units/bonsai-host.network" "$units/bonsai-lan.network"
    fi
    systemctl daemon-reload
    podman rm -f "$name" >/dev/null 2>&1 || true
    podman volume rm -f "$name-home" >/dev/null 2>&1 || true
    podman rmi -f "localhost/$name" >/dev/null 2>&1 || true
    if [ ! -e "$units/bonsai-host.network" ]; then
        podman network rm -f systemd-bonsai-host systemd-bonsai-lan >/dev/null 2>&1 || true
    fi
    say "removed. The $name entry in ~/.ssh/config is left in place."
    exit 0
fi

# 1. Emulation for the board's CPU, with the C flag so sudo works inside.
reg="/proc/sys/fs/binfmt_misc/$QEMU"
[ -e "$reg" ] || die "no emulation for $PLATFORM yet. Install it, then run this again:
  Arch, CachyOS:  sudo pacman -S qemu-user-static qemu-user-static-binfmt
  Fedora:         sudo dnf install qemu-user-static
  Debian, Ubuntu: sudo apt install qemu-user-static"
if ! grep -q '^flags:.*C' "$reg"; then
    conf="$(grep -l "^:$QEMU:" /usr/lib/binfmt.d/*.conf 2>/dev/null | head -1)"
    [ -n "$conf" ] || die "$QEMU lacks the C flag, and no /usr/lib/binfmt.d file to fix it from"
    mkdir -p /etc/binfmt.d
    sed "/^:$QEMU:/s/:\([A-Z]*\)\$/:\1C/" "$conf" > "/etc/binfmt.d/$(basename "$conf")"
    systemctl restart systemd-binfmt
    grep -q '^flags:.*C' "$reg" || die "couldn't turn on the C flag for $QEMU"
    say "emulation now runs sudo (C flag, /etc/binfmt.d/$(basename "$conf"))"
fi

# 2. Your LAN: the interface and router from the default route.
dev="${VIRTUAL_PI_LAN_DEV:-$(ip -4 route show default | awk '{print $5; exit}')}"
[ -n "$dev" ] || die "no default route; set VIRTUAL_PI_LAN_DEV to your network interface"
gateway="$(ip -4 route show default dev "$dev" | awk '{print $3; exit}')"
subnet="$(ip -4 route show dev "$dev" scope link | awk '/proto kernel/ {print $1; exit}')"
[ -n "$gateway" ] && [ -n "$subnet" ] || die "can't read the network on $dev"
if [ -z "${VIRTUAL_PI_LAN_IP:-}" ]; then
    [ "${subnet#*/}" = 24 ] || die "set VIRTUAL_PI_LAN_IP to a free address in $subnet"
    VIRTUAL_PI_LAN_IP="${subnet%.*/24}.$LAN_HOST"
fi
lan_ip="$VIRTUAL_PI_LAN_IP"
if [ -e "/sys/class/net/$dev/wireless" ]; then
    say "warning: $dev is Wi-Fi. Most access points drop the board's LAN traffic; use a wired interface if the phone can't see it."
fi

# 3. The image, with your ssh key. Host networking, so a firewall can't block apt.
key=""
for k in id_ed25519.pub id_ecdsa.pub id_rsa.pub; do
    [ -f "$home/.ssh/$k" ] && { key="$home/.ssh/$k"; break; }
done
[ -n "$key" ] || die "no ssh key in $home/.ssh; make one with: ssh-keygen -t ed25519"
ctx="$(mktemp -d)"
trap 'rm -rf "$ctx"' EXIT
cp -r "$here/Containerfile" "$here/rootfs" "$ctx/"
cp "$key" "$ctx/authorized_keys"
say "building the image (emulated, so a few minutes the first time)"
podman build --network host --platform "$PLATFORM" -t "localhost/$name" "$ctx"

# 4. The units: systemd starts the networks and the board, now and at boot.
mkdir -p "$units"
cp "$here/bonsai-host.network" "$units/"
sed -e "s|@PARENT@|$dev|" -e "s|@SUBNET@|$subnet|" -e "s|@GATEWAY@|$gateway|" \
    "$here/bonsai-lan.network" > "$units/bonsai-lan.network"
sed -e "s|@BOARD@|$board|g" -e "s|@HOST_IP@|$HOST_IP|" -e "s|@LAN_IP@|$lan_ip|" \
    -e "s|@MEMORY@|$MEMORY|" -e "s|@CPUS@|$CPUS|" \
    "$here/board.container" > "$units/$name.container"
systemctl daemon-reload
systemctl restart "$name.service"

# 5. ssh: a `virtual-<board>` host, and its key trusted so nothing asks.
cfg="$home/.ssh/config"
if ! grep -qx "Host $name" "$cfg" 2>/dev/null; then
    printf '\nHost %s\n    HostName %s\n    User pi\n' "$name" "$HOST_IP" >> "$cfg"
    chown "$user:" "$cfg"
fi
for _ in $(seq 90); do
    ssh-keyscan -T 2 -t ed25519 "$HOST_IP" > "$ctx/hostkey" 2>/dev/null && [ -s "$ctx/hostkey" ] && break
    sleep 1
done
[ -s "$ctx/hostkey" ] || die "the board didn't answer on $HOST_IP; see: journalctl -u $name"
sudo -u "$user" ssh-keygen -R "$HOST_IP" >/dev/null 2>&1 || true
sudo -u "$user" sh -c "cat >> '$home/.ssh/known_hosts'" < "$ctx/hostkey"

say "ready: ssh $name, or BONSAI_PI=$name cargo run --release"
say "host link $HOST_IP, LAN address $lan_ip on $dev"
