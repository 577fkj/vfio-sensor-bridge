# AGENTS.md

Rules for AI coding agents working in this repository.

## Project intent

Build `vfio-sensor-bridge`: a Proxmox VE sensor bridge for passthrough VM
sensor data. The first release uses a Linux VM agent, virtio-serial transport,
a PVE host daemon, and a Linux hwmon kernel module.

## Current source of truth

- Product plan: `README.md`
- hwmon reference driver: `vm/doc/dummy-hwmon-driver/klvoltage.c`
- Reference Makefile: `vm/doc/dummy-hwmon-driver/Makefile`

## Communication

- Reply to the user in Simplified Chinese.
- Keep messages concise and technical.
- Lead with the result, then include essential evidence or commands.
- Use full absolute paths when referencing workspace files in user-facing replies.
- Present one recommended path for implementation choices.
- Ask a question only when local inspection cannot resolve a decision that changes behavior.

## Repository handling

- Preserve user changes.
- Inspect `git status --short` before edits.
- Use `rg` and `rg --files` for searching.
- Use `apply_patch` for manual file edits.
- Keep generated build artifacts out of source directories covered by git.
- Use ASCII for new source files unless the file format or user request requires other text.

## Implementation priorities

1. Keep the kernel module small and testable.
2. Keep userspace policy in `hostd`.
3. Keep the kernel ABI explicit in `kernel/include/uapi/vfio_sensor_bridge.h`.
4. Keep protocol types shared in `crates/vsb-protocol`.
5. Keep install and recovery steps reproducible through `packaging`.

## Kernel module guidance

- Base the initial hwmon implementation on `vm/doc/dummy-hwmon-driver/klvoltage.c`.
- Expose one hwmon device per VM.
- Name hwmon devices as `vsb_vm_<VMID>`.
- Expose standard hwmon attributes:
  - `tempN_input`, `tempN_label`
  - `fanN_input`, `fanN_label`
  - `inN_input`, `inN_label`
  - `currN_input`, `currN_label`
  - `powerN_input`, `powerN_label`
- Store dynamic policy outside the kernel module.
- Treat ioctl payloads as untrusted input and validate sizes, counts, and enum values.

## Host daemon guidance

- Read `/etc/vfio-sensor-bridge/hostd.toml`.
- Watch sockets under `/run/vfio-sensor-bridge`.
- Decode `u32_le length + JSON` frames.
- Apply schema changes through `VSB_IOCTL_SET_SCHEMA`.
- Apply sample changes through `VSB_IOCTL_SET_VALUES`.
- Apply VM removal through `VSB_IOCTL_REMOVE_VM`.
- Treat any valid message as a heartbeat update.
- Use `heartbeat.timeout_seconds = 30` and `policy = "warn_then_remove"` as defaults.
- Run hooks after topology changes complete.
- Debounce hook execution using `hooks.debounce_seconds`.
- Log hook failures and timeouts.

## Agent guidance

- Read `/etc/vfio-sensor-bridge/agent.toml`.
- Connect to `/dev/virtio-ports/org.vfio_sensor_bridge.0`.
- Scan `/sys/class/hwmon`.
- Send `hello`, then full `schema`, then full `sample`.
- Send `sample` every `sample_seconds`.
- Send `heartbeat` every `heartbeat_seconds`.
- Rescan sensors every `rescan_seconds`.
- Send a new `schema` when a sensor is added, removed, renamed, or changes type.

## Protocol guidance

- Frame format: `u32_le length` followed by a UTF-8 JSON payload.
- Maximum frame size: 256 KiB.
- Message types: `hello`, `schema`, `sample`, `heartbeat`, `goodbye`.
- Sensor values use Linux hwmon units:
  - temperature: millidegree Celsius
  - fan: RPM
  - voltage: millivolt
  - current: milliampere
  - power: microwatt
- `schema` is complete state for a VM.
- `sample` references the active schema generation.

## Hook guidance

Default host config shape:

```toml
[hooks]
enabled = true
debounce_seconds = 5
timeout_seconds = 30

[[hooks.rule]]
events = ["device_created", "device_removed", "sensor_added", "sensor_removed", "sensor_changed", "vm_offline"]
command = ["/usr/bin/systemctl", "restart", "fancontrol.service"]
```

Hook environment variables:

- `VSB_EVENTS`
- `VSB_VMID`
- `VSB_HWMON_NAME`
- `VSB_HWMON_PATH`
- `VSB_SENSOR_COUNT`

## Test environment

PVE host:

```sh
ssh root@172.18.138.214
```

Debian VM tunnel:

```sh
ssh -L 2222:172.18.138.215:22 root@172.18.138.214
```

Debian VM login:

```sh
ssh -p 2222 root@127.0.0.1
```

PVE VM commands:

```sh
qm list
qm start <VMID>
qm stop <VMID>
qm reboot <VMID>
```

Hyper-V recovery for the PVE host VM:

```powershell
Get-VM -Name PVE
Restart-VM -Name PVE -Force
```

## Validation checklist

- Kernel module builds against the current PVE kernel headers.
- `/dev/vfio-sensor-bridge` appears after module load.
- ioctl can create temp, fan, voltage, current, and power sensors.
- `/sys/class/hwmon` exposes expected files and values.
- VM agent sends schema, sample, and heartbeat messages.
- Full path works from VM agent to `sensors` on the PVE host.
- Sensor add, remove, and rename events update host hwmon state.
- Hooks run once after debounce for grouped topology changes.
- Agent stop triggers the configured heartbeat policy.
- hostd, VM, and PVE reboot recovery works.

## Fake hwmon test

Use this to test the full path without real hardware sensors inside the VM.

Create a temporary sensor directory inside the VM:

```sh
mkdir -p /tmp/vsb-fake-hwmon/hwmon0
printf 'fakechip\n' >/tmp/vsb-fake-hwmon/hwmon0/name
printf '5500\n' >/tmp/vsb-fake-hwmon/hwmon0/temp1_input
printf 'vm_fake_temp\n' >/tmp/vsb-fake-hwmon/hwmon0/temp1_label
printf '1450\n' >/tmp/vsb-fake-hwmon/hwmon0/fan1_input
printf 'reboot_fan\n' >/tmp/vsb-fake-hwmon/hwmon0/fan1_label
printf '11800\n' >/tmp/vsb-fake-hwmon/hwmon0/in0_input
printf 'reboot_12v\n' >/tmp/vsb-fake-hwmon/hwmon0/in0_label
printf '2300\n' >/tmp/vsb-fake-hwmon/hwmon0/curr1_input
printf 'reboot_current\n' >/tmp/vsb-fake-hwmon/hwmon0/curr1_label
printf '6800000\n' >/tmp/vsb-fake-hwmon/hwmon0/power1_input
printf 'reboot_power\n' >/tmp/vsb-fake-hwmon/hwmon0/power1_label
```

Point the agent at the fake root via `/etc/vfio-sensor-bridge/agent.toml`:

```toml
[agent]
virtio_port = "/dev/virtio-ports/org.vfio_sensor_bridge.0"
scan_root = "/tmp/vsb-fake-hwmon"
hwmon_name_template = "gpu_{hostname}"
rescan_seconds = 10
sample_seconds = 1
heartbeat_seconds = 5
```

Restart the agent:

```sh
systemctl restart vfio-sensor-bridge-agent.service
```

Expected on the PVE host:

```text
/sys/class/hwmon/hwmonX/name = gpu_<hostname>
temp1_input = 5500
fan1_input = 1450
in0_input = 11800
curr1_input = 2300
power1_input = 6800000
```

## Development smoke test

From the source tree on the PVE test host:

```sh
make -C kernel
sudo sh packaging/scripts/pve-smoke.sh
```

Expected files after load:

```text
/dev/vfio-sensor-bridge
/sys/class/hwmon/hwmonX/name = vsb_vm_9000
/sys/class/hwmon/hwmonX/temp1_input
/sys/class/hwmon/hwmonX/fan1_input
/sys/class/hwmon/hwmonX/in0_input
/sys/class/hwmon/hwmonX/curr1_input
/sys/class/hwmon/hwmonX/power1_input
```
