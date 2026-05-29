#!/bin/sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
CONFIG_DIR="/etc/vfio-sensor-bridge"

if [ "$(id -u)" -ne 0 ]; then
	echo "run as root" >&2
	exit 1
fi

if [ -x "$ROOT/target/release/agent-linux" ]; then
	install -m 0755 "$ROOT/target/release/agent-linux" /usr/sbin/agent-linux
else
	cargo build --release --workspace --bin agent-linux
	install -m 0755 "$ROOT/target/release/agent-linux" /usr/sbin/agent-linux
fi

install -d -m 0755 "$CONFIG_DIR"
[ -f "$CONFIG_DIR/agent.toml" ] || install -m 0644 "$ROOT/packaging/agent.toml" "$CONFIG_DIR/agent.toml"
install -m 0644 "$ROOT/packaging/systemd/vfio-sensor-bridge-agent.service" \
	/etc/systemd/system/vfio-sensor-bridge-agent.service

systemctl daemon-reload
systemctl enable --now vfio-sensor-bridge-agent.service

echo "agent install complete"
