#!/usr/bin/env sh
set -eu

PREFIX="/usr/local"
SYSCONFDIR="/etc/shuttle-gateway"
STATE_DIR="/var/lib/shuttle-gateway"
SERVICE_DIR="/etc/systemd/system"
SERVICE_USER="shuttle-gateway"
SERVICE_GROUP="shuttle-gateway"

die() {
  printf '%s\n' "$*" >&2
  exit 1
}

copy_if_missing() {
  src="$1"
  dst="$2"
  if [ -e "$dst" ]; then
    printf 'Keeping existing %s\n' "$dst"
  else
    install -m 0644 "$src" "$dst"
    printf 'Installed %s\n' "$dst"
  fi
}

if [ "$(id -u)" -ne 0 ]; then
  die "install.sh must run as root"
fi

if ! command -v systemctl >/dev/null 2>&1; then
  die "systemctl is required on the target host"
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

[ -x "$SCRIPT_DIR/bin/shuttle-gateway" ] || die "missing bin/shuttle-gateway in archive"
[ -x "$SCRIPT_DIR/bin/stl" ] || die "missing bin/stl in archive"
[ -f "$SCRIPT_DIR/systemd/shuttle-gateway.service" ] || die "missing systemd/shuttle-gateway.service in archive"
[ -f "$SCRIPT_DIR/etc/projects.toml.example" ] || die "missing etc/projects.toml.example in archive"
[ -f "$SCRIPT_DIR/etc/shuttle-gateway.env.example" ] || die "missing etc/shuttle-gateway.env.example in archive"

if ! getent group "$SERVICE_GROUP" >/dev/null 2>&1; then
  groupadd --system "$SERVICE_GROUP"
fi

if ! id "$SERVICE_USER" >/dev/null 2>&1; then
  useradd --system \
    --gid "$SERVICE_GROUP" \
    --home-dir "$STATE_DIR" \
    --shell /usr/sbin/nologin \
    "$SERVICE_USER"
fi

install -d -m 0755 "$PREFIX/bin"
install -m 0755 "$SCRIPT_DIR/bin/shuttle-gateway" "$PREFIX/bin/shuttle-gateway"
install -m 0755 "$SCRIPT_DIR/bin/stl" "$PREFIX/bin/stl"

install -d -m 0755 "$SYSCONFDIR"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_GROUP" "$STATE_DIR"

copy_if_missing "$SCRIPT_DIR/etc/projects.toml.example" "$SYSCONFDIR/projects.toml"
copy_if_missing "$SCRIPT_DIR/etc/shuttle-gateway.env.example" "$SYSCONFDIR/shuttle-gateway.env"
chmod 0600 "$SYSCONFDIR/shuttle-gateway.env"

install -m 0644 "$SCRIPT_DIR/systemd/shuttle-gateway.service" "$SERVICE_DIR/shuttle-gateway.service"
systemctl daemon-reload

cat <<EOF
Installed shuttle-gateway LXC files.

Next steps:
  1. Edit $SYSCONFDIR/projects.toml
  2. Edit $SYSCONFDIR/shuttle-gateway.env with runtime-injected or secret-managed values
  3. Run: systemctl enable --now shuttle-gateway
  4. Check: systemctl status shuttle-gateway
EOF
