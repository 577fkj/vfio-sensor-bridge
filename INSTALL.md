# vfio-sensor-bridge — Build and Install

This document covers building, installing, configuring, verifying, upgrading,
and removing `vfio-sensor-bridge` on a Proxmox VE host and a Linux guest VM.

[中文安装文档](INSTALL.zh.md)

## 1. Components

- `vsb_hwmon.ko`: PVE host kernel module. Provides `/dev/vfio-sensor-bridge` and hwmon devices.
- `hostd`: PVE host daemon. Receives VM sensor messages and writes them to the kernel module.
- `agent-linux`: Linux VM agent. Scans `/sys/class/hwmon` and sends data over virtio-serial.
- `vsbctl`: PVE helper CLI. Configures VM channels and runs smoke tests.

Data path:

```text
Linux VM /sys/class/hwmon
  -> agent-linux
  -> /dev/virtio-ports/org.vfio_sensor_bridge.0
  -> PVE /run/vfio-sensor-bridge/vm-<VMID>.sock
  -> hostd
  -> /dev/vfio-sensor-bridge
  -> /sys/class/hwmon/hwmonX
```

## 2. Requirements

### 2.1 PVE host

Install required tools:

```sh
apt update
apt install -y cargo rustc make gcc dkms pve-headers lm-sensors
```

Requirements:

- PVE headers matching the running kernel must be installed.
- `systemd` must be available.
- Root access is required.
- VMs are managed by PVE/QEMU; the `qm` command must be available.

### 2.2 Linux VM

- Root access is required.
- The VM must have a virtio-serial channel attached.
- The agent reads `/sys/class/hwmon` by default.
- `/dev/virtio-ports/org.vfio_sensor_bridge.0` must exist inside the VM.
- Install `smartmontools` if smartctl disk temperature collection is needed.

The install script can compile `agent-linux` directly inside the VM. If the VM
lacks a Rust toolchain, compile on a compatible Linux host and copy the binary.

## 3. Source code

Clone onto the PVE host or a build machine:

```sh
cd /root
git clone <repo-url> vfio-sensor-bridge
cd /root/vfio-sensor-bridge
```

If the source tree already exists:

```sh
cd /root/vfio-sensor-bridge
```

## 4. Build

### 4.1 Rust userspace

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Release build:

```sh
cargo build --release --workspace
```

Produced binaries:

```text
target/release/hostd
target/release/vsbctl
target/release/agent-linux
```

### 4.2 PVE kernel module

Run on the PVE host:

```sh
make -C kernel
```

Output:

```text
kernel/vsb_hwmon.ko
```

## 5. PVE host install

Use the repository install script:

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/install-host.sh
```

The script:

- Installs DKMS source to `/usr/src/vfio-sensor-bridge-0.1.0`.
- Builds and installs `vsb_hwmon.ko` via DKMS.
- Installs `hostd` to `/usr/sbin/hostd`.
- Installs `vsbctl` to `/usr/sbin/vsbctl`.
- Writes `/etc/vfio-sensor-bridge/hostd.toml` on first install.
- Installs the systemd unit.
- Writes `/etc/modules-load.d/vfio-sensor-bridge.conf`.
- Loads `vsb_hwmon`.
- Enables and starts `vfio-sensor-bridge-hostd.service`.

Post-install checks:

```sh
systemctl status vfio-sensor-bridge-hostd.service
ls -l /dev/vfio-sensor-bridge
lsmod | grep vsb_hwmon
dkms status | grep vfio-sensor-bridge
```

## 6. VM channel setup

List VMs:

```sh
qm list
```

Preview what will be written to the PVE config:

```sh
vsbctl attach <VMID> --dry-run
```

Attach the virtio-serial channel:

```sh
vsbctl attach <VMID>
```

Reboot or start the VM:

```sh
qm reboot <VMID>
```

Host socket path:

```text
/run/vfio-sensor-bridge/vm-<VMID>.sock
```

Default virtio channel name:

```text
org.vfio_sensor_bridge.0
```

## 7. Linux VM agent install

### 7.1 Install inside the VM

Copy the source tree into the VM, then run:

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/install-agent.sh
```

The script:

- Builds or reuses `target/release/agent-linux`.
- Installs the binary to `/usr/sbin/agent-linux`.
- Writes `/etc/vfio-sensor-bridge/agent.toml` on first install.
- Installs the systemd unit.
- Enables and starts `vfio-sensor-bridge-agent.service`.

Check agent status:

```sh
systemctl status vfio-sensor-bridge-agent.service
ls -l /dev/virtio-ports/org.vfio_sensor_bridge.0
```

### 7.2 Using a pre-built agent

Build on a compatible Linux host:

```sh
cargo build --release --workspace --bin agent-linux
```

Copy to the VM:

```sh
scp target/release/agent-linux root@<vm-ip>:/usr/sbin/agent-linux
```

Install config and service files inside the VM:

```sh
chmod 0755 /usr/sbin/agent-linux
mkdir -p /etc/vfio-sensor-bridge
cp packaging/agent.toml /etc/vfio-sensor-bridge/agent.toml
cp packaging/systemd/vfio-sensor-bridge-agent.service /etc/systemd/system/vfio-sensor-bridge-agent.service
systemctl daemon-reload
systemctl enable --now vfio-sensor-bridge-agent.service
```

## 8. Configuration

### 8.1 PVE host config

Path:

```text
/etc/vfio-sensor-bridge/hostd.toml
```

Default:

```toml
[daemon]
run_dir = "/run/vfio-sensor-bridge"
device = "/dev/vfio-sensor-bridge"
log_level = "info"

[virtio]
channel_name = "org.vfio_sensor_bridge.0"
socket_template = "/run/vfio-sensor-bridge/vm-{vmid}.sock"

[heartbeat]
timeout_seconds = 30
policy = "warn_then_remove"

[hooks]
enabled = true
debounce_seconds = 5
timeout_seconds = 30

[[hooks.rule]]
events = ["device_created", "device_removed", "sensor_added", "sensor_removed", "sensor_changed", "vm_offline"]
command = ["/usr/bin/systemctl", "restart", "fancontrol.service"]
```

`hostd` stores the persistent sensor cache alongside the host config. Default
path:

```text
/etc/vfio-sensor-bridge/persistent-vms.json
```

On startup after a PVE reboot, `hostd` reads this cache and recreates persistent
hwmon sensors with their default values before the VM agent reconnects.

Restart after changes:

```sh
systemctl restart vfio-sensor-bridge-hostd.service
```

Hook environment variables:

```text
VSB_EVENTS
VSB_VMID
VSB_HWMON_NAME
VSB_HWMON_PATH
VSB_SENSOR_COUNT
```

### 8.2 VM agent config

Path:

```text
/etc/vfio-sensor-bridge/agent.toml
```

Default:

```toml
[agent]
virtio_port = "/dev/virtio-ports/org.vfio_sensor_bridge.0"
scan_root = "/sys/class/hwmon"
# Optional hwmon name sent to the host. Supported placeholders:
# {hostname}, {agent_version}
# hwmon_name_template = "vsb_{hostname}"
rescan_seconds = 10
sample_seconds = 1
heartbeat_seconds = 5

[lsi_hba]
enabled = false
devices = ["/dev/mpt2ctl", "/dev/mpt3ctl"]
max_ioc = 16
# HBA temperature sensor label. Supported placeholders:
# {portname}, {chip}, {version}
label_template = "{chip}"

[smartctl]
enabled = false
command = "/usr/sbin/smartctl"
device_globs = ["/dev/sd*", "/dev/sata*"]
timeout_seconds = 10
poll_seconds = 30
# Disk temperature label. Supported placeholders:
# {device}, {path}, {model_family}, {model}, {serial}, {wwn}, {firmware},
# {capacity}, {sector_sizes}, {rotation_rate}, {form_factor}, {ata_version},
# {sata_version}, {vendor}, {product}, {revision}, {database},
# {smart_available}, {smart_enabled}
label_template = "{device} temperature"
```

Edit config using the agent CLI:

```sh
agent-linux config show
agent-linux config validate
agent-linux config set --sample-seconds 1 --heartbeat-seconds 5 --rescan-seconds 10
agent-linux config persistent discover
agent-linux config persistent add --from 1 --default-value 65000
systemctl restart vfio-sensor-bridge-agent.service
```

Persistent sensor example:

```toml
[[persistent_sensor]]
id = "gpu_edge_temp"
kind = "temperature"
label = "GPU Edge"
default_value = 65000

[persistent_sensor.source]
type = "hwmon"
chip_name = "amdgpu"
input = "temp1_input"
source_label = "edge"
device_path_contains = "0000:03:00.0"
```

Customize the hwmon name shown on the PVE host:

```toml
[agent]
hwmon_name_template = "gpu_{hostname}"
```

Supported placeholders: `{hostname}`, `{agent_version}`.

Name rules:

- Maximum 31 bytes.
- ASCII letters, digits, and `_` are allowed.
- Other characters are replaced with `_`.
- Empty value falls back to `vsb_vm_<VMID>`.

Restart after changes:

```sh
systemctl restart vfio-sensor-bridge-agent.service
```

Enable smartctl disk temperature collection:

```toml
[smartctl]
enabled = true
command = "/usr/sbin/smartctl"
device_globs = ["/dev/sata*", "/dev/sd*"]
poll_seconds = 30
label_template = "{device} {model} {serial} temperature"
```

Probe smartctl sources inside a VM:

```sh
agent-linux --probe-smartctl /etc/vfio-sensor-bridge/agent.toml
```

`label_template` supports all fields from `smartctl -i`: `{model}`, `{serial}`,
`{wwn}`, `{firmware}`, `{capacity}`, `{sector_sizes}`, `{rotation_rate}`,
`{form_factor}`, `{ata_version}`, `{sata_version}`, `{model_family}`,
`{vendor}`, `{product}`, `{revision}`, `{database}`, `{smart_available}`,
`{smart_enabled}`. Sensor labels are limited to 128 bytes.

The agent re-expands `device_globs` every `rescan_seconds`. Disk additions,
removals, path changes, or label changes trigger a new schema.

## 9. Verification

### 9.1 Host smoke test

After install:

```sh
vsbctl smoke --device /dev/vfio-sensor-bridge
```

### 9.2 Check hwmon devices

List all hwmon names:

```sh
for f in /sys/class/hwmon/hwmon*/name; do echo "$f: $(cat "$f")"; done
```

Default name example: `vsb_vm_<VMID>`  
Custom name example: `gpu_debian`

Read sensor attributes:

```sh
for d in /sys/class/hwmon/hwmon*; do
  [ -f "$d/name" ] || continue
  name=$(cat "$d/name")
  case "$name" in
    vsb_vm_*|gpu_*)
      echo "== $d $name =="
      find "$d" -maxdepth 1 -type f \
        \( -name 'temp*_input' -o -name 'fan*_input' -o -name 'in*_input' -o -name 'curr*_input' -o -name 'power*_input' -o -name '*_label' \) \
        -print -exec cat {} \;
      ;;
  esac
done
```

Use `sensors`:

```sh
sensors
```

### 9.3 Service logs

Inside the VM:

```sh
journalctl -u vfio-sensor-bridge-agent.service -n 100 --no-pager
```

On the PVE host:

```sh
journalctl -u vfio-sensor-bridge-hostd.service -n 100 --no-pager
```

## 10. Upgrade

### 10.1 PVE host upgrade

```sh
cd /root/vfio-sensor-bridge
systemctl stop vfio-sensor-bridge-hostd.service
sh packaging/scripts/install-host.sh
systemctl status vfio-sensor-bridge-hostd.service
```

DKMS rebuilds and installs the current module version.

### 10.2 VM agent upgrade

```sh
cd /root/vfio-sensor-bridge
systemctl stop vfio-sensor-bridge-agent.service
sh packaging/scripts/install-agent.sh
systemctl status vfio-sensor-bridge-agent.service
```

## 11. Uninstall

### 11.1 Remove the VM virtio channel

On the PVE host:

```sh
vsbctl detach <VMID> --dry-run
vsbctl detach <VMID>
qm reboot <VMID>
```

### 11.2 Uninstall the VM agent

Inside the VM:

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/uninstall-agent.sh
```

### 11.3 Uninstall PVE host components

On the PVE host:

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/uninstall-host.sh
```

Config files that are retained:

```text
/etc/vfio-sensor-bridge/hostd.toml
/etc/vfio-sensor-bridge/agent.toml
```

## 12. Troubleshooting

### 12.1 `/dev/vfio-sensor-bridge` missing

```sh
modprobe vsb_hwmon
lsmod | grep vsb_hwmon
journalctl -k -n 100 --no-pager
```

### 12.2 DKMS build failure

Verify PVE headers match the running kernel:

```sh
uname -r
dpkg -l | grep pve-headers
dkms status
```

Reinstall headers, then retry:

```sh
sh packaging/scripts/install-host.sh
```

### 12.3 Virtio port missing inside VM

Check the VM config on the PVE host:

```sh
qm config <VMID>
vsbctl attach <VMID> --dry-run
```

Re-attach the channel and reboot the VM:

```sh
vsbctl attach <VMID>
qm reboot <VMID>
```

### 12.4 `cargo` not available inside VM

Use the pre-built agent workflow: build with
`cargo build --release --workspace --bin agent-linux` on a compatible Linux
host, then copy `target/release/agent-linux` to `/usr/sbin/agent-linux` inside
the VM.

### 12.5 Hook execution failure

Check host logs:

```sh
journalctl -u vfio-sensor-bridge-hostd.service -n 100 --no-pager
```

The default hook runs:

```text
/usr/bin/systemctl restart fancontrol.service
```

Adjust `[hooks]` in `/etc/vfio-sensor-bridge/hostd.toml`, then restart `hostd`.

### 12.6 CRLF interpreter error in scripts

Convert scripts to LF line endings:

```sh
sed -i 's/\r$//' packaging/scripts/*.sh
```

### 12.7 Module in use, unload fails

Stop the host service first, then unload:

```sh
systemctl stop vfio-sensor-bridge-hostd.service
rmmod vsb_hwmon
```

