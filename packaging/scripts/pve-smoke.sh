#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
	echo "run as root" >&2
	exit 1
fi

TMP_BIN="$(mktemp /tmp/vsb-smoke.XXXXXX)"
trap 'rm -f "$TMP_BIN"' EXIT HUP INT TERM

modprobe vsb_hwmon 2>/dev/null || insmod kernel/vsb_hwmon.ko
test -c /dev/vfio-sensor-bridge
gcc -I kernel/include/uapi -Wall -Wextra -Werror -o "$TMP_BIN" packaging/smoke/vsb-smoke.c
"$TMP_BIN"
sensors | grep -A8 'vsb_vm_9000' || true
