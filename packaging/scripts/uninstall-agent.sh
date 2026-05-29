#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
	echo "run as root" >&2
	exit 1
fi

systemctl disable --now vfio-sensor-bridge-agent.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/vfio-sensor-bridge-agent.service
rm -f /usr/sbin/agent-linux
systemctl daemon-reload

echo "agent uninstall complete"

