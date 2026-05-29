#!/bin/sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
VERSION="0.1.0"
SRC_DIR="/usr/src/vfio-sensor-bridge-${VERSION}"
CONFIG_DIR="/etc/vfio-sensor-bridge"
HOSTD_SERVICE="vfio-sensor-bridge-hostd.service"
KERNEL_MODULE="vsb_hwmon"

need_root() {
	if [ "$(id -u)" -ne 0 ]; then
		echo "run as root" >&2
		exit 1
	fi
}

need_root

if command -v systemctl >/dev/null 2>&1; then
	systemctl stop "$HOSTD_SERVICE" >/dev/null 2>&1 || true
fi

if grep -q "^${KERNEL_MODULE} " /proc/modules; then
	if ! modprobe -r "$KERNEL_MODULE"; then
		echo "failed to unload $KERNEL_MODULE; stop readers of /sys/class/hwmon vsb devices or reboot before reinstalling" >&2
		exit 1
	fi
fi

install -d -m 0755 "$SRC_DIR" "$SRC_DIR/kernel" "$SRC_DIR/kernel/include/uapi"
install -m 0644 "$ROOT/kernel/Makefile" "$SRC_DIR/kernel/Makefile"
install -m 0644 "$ROOT/kernel/vsb_hwmon.c" "$SRC_DIR/kernel/vsb_hwmon.c"
install -m 0644 "$ROOT/kernel/include/uapi/vfio_sensor_bridge.h" \
	"$SRC_DIR/kernel/include/uapi/vfio_sensor_bridge.h"
install -m 0644 "$ROOT/packaging/dkms/dkms.conf" "$SRC_DIR/dkms.conf"

if command -v dkms >/dev/null 2>&1; then
	dkms remove -m vfio-sensor-bridge -v "$VERSION" --all >/dev/null 2>&1 || true
	dkms add -m vfio-sensor-bridge -v "$VERSION"
	dkms build -m vfio-sensor-bridge -v "$VERSION"
	dkms install -m vfio-sensor-bridge -v "$VERSION"
else
	make -C "$ROOT/kernel"
	install -d -m 0755 "/lib/modules/$(uname -r)/extra"
	install -m 0644 "$ROOT/kernel/vsb_hwmon.ko" "/lib/modules/$(uname -r)/extra/vsb_hwmon.ko"
	depmod -a
fi

if [ ! -x "$ROOT/target/release/hostd" ] || [ ! -x "$ROOT/target/release/vsbctl" ]; then
	cargo build --release --workspace --bin hostd --bin vsbctl
fi
install -m 0755 "$ROOT/target/release/hostd" /usr/sbin/hostd
install -m 0755 "$ROOT/target/release/vsbctl" /usr/sbin/vsbctl

install -d -m 0755 "$CONFIG_DIR"
[ -f "$CONFIG_DIR/hostd.toml" ] || install -m 0644 "$ROOT/packaging/hostd.toml" "$CONFIG_DIR/hostd.toml"
install -m 0644 "$ROOT/packaging/systemd/vfio-sensor-bridge-hostd.service" \
	/etc/systemd/system/vfio-sensor-bridge-hostd.service
install -m 0644 "$ROOT/packaging/modules-load.conf" \
	/etc/modules-load.d/vfio-sensor-bridge.conf

modprobe "$KERNEL_MODULE"
systemctl daemon-reload
systemctl enable --now "$HOSTD_SERVICE"

echo "host install complete"
echo "run: vsbctl smoke"

