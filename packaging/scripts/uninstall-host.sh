#!/bin/sh
set -eu

VERSION="0.1.0"

if [ "$(id -u)" -ne 0 ]; then
	echo "run as root" >&2
	exit 1
fi

systemctl disable --now vfio-sensor-bridge-hostd.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/vfio-sensor-bridge-hostd.service
rm -f /etc/modules-load.d/vfio-sensor-bridge.conf
rm -f /usr/sbin/hostd /usr/sbin/vsbctl

rmmod vsb_hwmon >/dev/null 2>&1 || true
if command -v dkms >/dev/null 2>&1; then
	dkms remove -m vfio-sensor-bridge -v "$VERSION" --all >/dev/null 2>&1 || true
fi
rm -rf "/usr/src/vfio-sensor-bridge-${VERSION}"
depmod -a || true
systemctl daemon-reload

echo "host uninstall complete"

