#!/usr/bin/env bash
# Sets up a virtual board once; systemd keeps it running and starts it at boot.
#   sudo ./install.sh <board>          install, or update after editing the Containerfile
#   sudo ./install.sh <board> remove   stop it and delete everything it made
# Boards are the files in boards/: pi5, zero-2w, zero-w.
# Overrides: VIRTUAL_PI_LAN_DEV (network interface), VIRTUAL_PI_LAN_IP (its LAN address),
# VIRTUAL_PI_LAN_DRIVER (macvlan or ipvlan; default: ipvlan on Wi-Fi, macvlan on Ethernet),
# VIRTUAL_PI_RELAY (on Wi-Fi: multicast groups to relay to the board, "239.2.3.2:6969 …";
# kept across installs, empty to remove).
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

# The Wi-Fi relays of this board (bonsai-relay@<board>-<n>).
drop_relays() {
    for env in /etc/bonsai/relay-"$name"-*.env; do
        [ -e "$env" ] || continue
        inst="$(basename "$env" .env)"
        inst="${inst#relay-}"
        systemctl disable --now "bonsai-relay@$inst.service" 2>/dev/null || true
        rm -f "$env"
    done
}

if [ "${2:-}" = remove ]; then
    drop_relays
    systemctl stop "$name.service" 2>/dev/null || true
    rm -f "$units/$name.container"
    # The shared networks go with the last board. Stop their units too, or
    # systemd keeps them "active" and won't recreate the networks next install.
    if ! ls "$units"/virtual-*.container >/dev/null 2>&1; then
        systemctl stop bonsai-host-network.service bonsai-lan-network.service 2>/dev/null || true
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
# Wi-Fi access points drop frames from any MAC but the one that joined, so on
# Wi-Fi the board shares your computer's MAC (ipvlan) instead of having its own
# (macvlan, better on Ethernet). VIRTUAL_PI_LAN_DRIVER picks one by hand.
if [ -e "/sys/class/net/$dev/wireless" ]; then
    driver="${VIRTUAL_PI_LAN_DRIVER:-ipvlan}"
    say "$dev is Wi-Fi: the LAN link uses $driver"
else
    driver="${VIRTUAL_PI_LAN_DRIVER:-macvlan}"
fi
case "$driver" in macvlan | ipvlan) ;; *) die "VIRTUAL_PI_LAN_DRIVER is macvlan or ipvlan, not $driver" ;; esac
# Podman's netavark learned ipvlan in 1.5; an older one fails at start.
backend="$(podman info --format '{{.Host.NetworkBackendInfo.Version}}' 2>/dev/null | awk '{print $NF}')"
if [ "$driver" = ipvlan ] && [ -n "$backend" ] &&
    [ "$(printf '%s\n1.5.0\n' "$backend" | sort -V | head -1)" != 1.5.0 ]; then
    say "warning: netavark $backend has no ipvlan (1.5 or newer does); using macvlan, which Wi-Fi may drop"
    driver=macvlan
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
sed -e "s|@DRIVER@|$driver|" -e "s|@PARENT@|$dev|" -e "s|@SUBNET@|$subnet|" -e "s|@GATEWAY@|$gateway|" \
    "$here/bonsai-lan.network" > "$units/bonsai-lan.network"
sed -e "s|@BOARD@|$board|g" -e "s|@HOST_IP@|$HOST_IP|" -e "s|@LAN_IP@|$lan_ip|" \
    -e "s|@MEMORY@|$MEMORY|" -e "s|@CPUS@|$CPUS|" \
    "$here/board.container" > "$units/$name.container"
systemctl daemon-reload
# The LAN network made by an earlier install may no longer fit: another driver
# (moved between Wi-Fi and Ethernet), interface or subnet. Podman can't change a
# network in place, so stop the boards on it, delete it, and let the loop below
# make it again; the boards restart at the end.
restart_boards=""
want="$driver|$dev|$subnet"
have="$(podman network inspect systemd-bonsai-lan \
    --format '{{.Driver}}|{{.NetworkInterface}}|{{range .Subnets}}{{.Subnet}}{{end}}' 2>/dev/null || true)"
if [ -n "$have" ] && [ "$have" != "$want" ]; then
    say "the LAN link changes ($have → $want): restarting the boards on it"
    for unit in "$units"/virtual-*.container; do
        other="$(basename "$unit" .container)"
        [ "$other" = "$name" ] && continue
        systemctl is-active --quiet "$other.service" && restart_boards="$restart_boards $other"
    done
    systemctl stop 'virtual-*.service' 2>/dev/null || true
    podman network rm -f systemd-bonsai-lan >/dev/null
fi
# A network unit runs once and stays "active", so if its network was deleted
# since (a remove, or by hand) systemd won't make it again: rerun it. Only when
# missing: restarting it would also restart every board that uses it.
for net in host lan; do
    podman network exists "systemd-bonsai-$net" || systemctl restart "bonsai-$net-network.service"
done
systemctl reset-failed "$name.service" 2>/dev/null || true
systemctl restart "$name.service"
for other in $restart_boards; do
    systemctl restart "$other.service"
done

# 5. On Wi-Fi, multicast from the LAN often never reaches the board: the Wi-Fi
# driver drops it on the way into ipvlan. So this computer joins each group in
# VIRTUAL_PI_RELAY itself and forwards every packet to the board's host link,
# where the board's socket (bound to 0.0.0.0:<port>) receives it. Rebuilt on
# every install; installing on Ethernet, or with VIRTUAL_PI_RELAY= (empty),
# removes them.
# Unset keeps the groups from the last install; set (even empty) replaces them.
previous=""
for env in /etc/bonsai/relay-"$name"-*.env; do
    [ -e "$env" ] || continue
    previous="$previous $(. "$env"; echo "$GROUP:$PORT")"
done
relays="${VIRTUAL_PI_RELAY-$previous}"
drop_relays
if [ -e "/sys/class/net/$dev/wireless" ] && [ -n "$relays" ]; then
    command -v socat >/dev/null || die "the Wi-Fi relay needs socat: install it (pacman/dnf/apt install socat), then run this again"
    mkdir -p /etc/bonsai
    cp "$here/bonsai-relay@.service" /etc/systemd/system/
    systemctl daemon-reload
    n=0
    for entry in $relays; do
        group="${entry%:*}"
        port="${entry##*:}"
        [[ "$group" =~ ^2(2[4-9]|3[0-9])\.[0-9]+\.[0-9]+\.[0-9]+$ ]] && [[ "$port" =~ ^[0-9]+$ ]] ||
            die "VIRTUAL_PI_RELAY entries are group:port, like 239.2.3.2:6969, not $entry"
        n=$((n + 1))
        printf 'GROUP=%s\nPORT=%s\nDEV=%s\nTARGET=%s\n' "$group" "$port" "$dev" "$HOST_IP" \
            > "/etc/bonsai/relay-$name-$n.env"
        systemctl enable --now "bonsai-relay@$name-$n.service"
        say "relaying $group:$port from $dev to the board"
    done
    if command -v ufw >/dev/null && ufw status 2>/dev/null | grep -q "Status: active"; then
        say "ufw is on: let the relayed ports in, e.g. sudo ufw allow ${port}/udp"
    fi
elif [ -e "/sys/class/net/$dev/wireless" ]; then
    say "tip: if multicast from the LAN (discovery, telemetry) doesn't reach the board, relay its groups:"
    say "     sudo VIRTUAL_PI_RELAY=\"239.2.3.2:6969\" containers/install.sh $board"
fi

# 6. ssh: a `virtual-<board>` host, and its key trusted so nothing asks.
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
