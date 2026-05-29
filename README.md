# vfio-sensor-bridge

Bridge guest VM hardware sensor readings into a Proxmox VE host as standard Linux
hwmon devices.

[中文说明](README.zh.md) | [Install guide](INSTALL.md) | [安装文档](INSTALL.zh.md)

## Overview

Targets PCI passthrough scenarios where temperature, fan, voltage, current, or
power sensors are visible only inside a VM. The VM agent reports those readings
to the PVE host through a dedicated virtio-serial channel. The host daemon
updates a kernel hwmon module so PVE, `sensors`, and fan-control software can
read them through `/sys/class/hwmon`.

## Components

| Component | Role |
|-----------|------|
| `vsb_hwmon.ko` | PVE host kernel module. Exposes `/dev/vfio-sensor-bridge` and registers hwmon devices under `/sys/class/hwmon`. |
| `hostd` | PVE host daemon. Accepts framed JSON from VM agents over QEMU sockets, drives ioctl updates on the kernel module, and runs hooks on topology changes. |
| `agent-linux` | Linux guest agent. Scans `/sys/class/hwmon`, optionally polls LSI/Broadcom HBA temperatures and smartctl disk temperatures, and publishes readings over virtio-serial. |
| `vsbctl` | PVE helper CLI. Manages virtio-serial channel attachment and runs smoke tests. |

## Architecture

```text
Linux VM
  agent-linux
    scans /sys/class/hwmon
    writes framed JSON
      |
      | virtio-serial
      v
PVE host
  hostd
    receives schema, samples, heartbeats
    calls /dev/vfio-sensor-bridge ioctl
      |
      v
  vsb_hwmon.ko
    registers /sys/class/hwmon/hwmonX
      |
      v
  sensors / fancontrol / PVE monitoring
```

Key paths:

| Path | Description |
|------|-------------|
| `/dev/virtio-ports/org.vfio_sensor_bridge.0` | Guest virtio port |
| `/run/vfio-sensor-bridge/vm-{vmid}.sock` | Host socket per VM |
| `/dev/vfio-sensor-bridge` | Host kernel device |
| `kernel/include/uapi/vfio_sensor_bridge.h` | Public ioctl ABI header |

## Protocol

Frame format: `u32_le length` + UTF-8 JSON payload. Maximum frame size: 256 KiB.

Message types:

| Type | Purpose |
|------|---------|
| `hello` | Agent startup and identity |
| `schema` | Complete current sensor list with `generation` |
| `sample` | Current sensor values referencing active `generation` |
| `heartbeat` | Agent liveness |
| `goodbye` | Normal agent shutdown |

Sensor value units follow Linux hwmon conventions:

| Class | Unit |
|-------|------|
| temperature | millidegree Celsius |
| fan | RPM |
| voltage | millivolt |
| current | milliampere |
| power | microwatt |

## Dynamic sensors

The agent rescans `/sys/class/hwmon` every `rescan_seconds`. When a sensor is
added, removed, renamed, or changes type, the agent sends a new `schema` with an
incremented generation. The host daemon re-registers the VM hwmon device to match
the new attribute set.

The agent can also poll IOC temperatures from LSI/Broadcom HBA devices via
MPT2/MPT3 ioctl, and disk temperatures through `smartctl`.

## Persistent sensors

A `[[persistent_sensor]]` entry stays in the schema even when the backing source
is absent. The agent reports `default_value` until the source is readable again.
`hostd` preserves persistent sensors across heartbeat timeouts and writes their
defaults to the kernel hwmon device.

## Heartbeat

Any valid message resets the liveness timer. On timeout:

| Policy | Action |
|--------|--------|
| `warn_only` | Write a warning to the journal |
| `remove_only` | Remove the VM hwmon device |
| `warn_then_remove` | Write a warning and remove the VM hwmon device |

Default: `timeout_seconds = 30`, `policy = "warn_then_remove"`.

## Hooks

`hostd` runs commands after device or sensor topology changes complete. Hook
events are debounced; multiple changes within `debounce_seconds` produce one
execution.

Available events: `device_created`, `device_removed`, `sensor_added`,
`sensor_removed`, `sensor_changed`, `vm_offline`.

Hook environment variables: `VSB_EVENTS`, `VSB_VMID`, `VSB_HWMON_NAME`,
`VSB_HWMON_PATH`, `VSB_SENSOR_COUNT`.

## Configuration files

| File | Purpose |
|------|---------|
| `/etc/vfio-sensor-bridge/hostd.toml` | Host daemon configuration |
| `/etc/vfio-sensor-bridge/agent.toml` | Guest agent configuration |
| `/etc/vfio-sensor-bridge/persistent-vms.json` | Persistent sensor cache (written by hostd) |

See [INSTALL.md](INSTALL.md) for build instructions, install procedures, full
configuration reference, and troubleshooting.

