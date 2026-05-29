# vfio-sensor-bridge 编译与安装文档

[English install guide](INSTALL.md)

本文档说明 `vfio-sensor-bridge` 在 Proxmox VE 主机和 Linux 虚拟机中的编译、安装、配置、验证、升级与卸载流程。

## 1. 组件说明

- `vsb_hwmon.ko`：PVE 主机内核模块，提供 `/dev/vfio-sensor-bridge` 和 hwmon 设备。
- `hostd`：PVE 主机守护进程，接收 VM 传感器消息并写入内核模块。
- `agent-linux`：Linux VM 内部代理，扫描 `/sys/class/hwmon` 并通过 virtio-serial 发送数据。
- `vsbctl`：PVE 主机辅助命令，用于 VM 通道配置和 smoke 测试。

数据路径：

```text
Linux VM /sys/class/hwmon
  -> agent-linux
  -> /dev/virtio-ports/org.vfio_sensor_bridge.0
  -> PVE /run/vfio-sensor-bridge/vm-<VMID>.sock
  -> hostd
  -> /dev/vfio-sensor-bridge
  -> /sys/class/hwmon/hwmonX
```

## 2. 环境要求

### 2.1 PVE 主机

需要以下工具和服务：

```sh
apt update
apt install -y cargo rustc make gcc dkms pve-headers lm-sensors
```

关键要求：

- 当前运行内核的 PVE headers 已安装。
- `systemd` 可用。
- 具备 root 权限。
- VM 由 PVE/QEMU 管理，`qm` 命令可用。

### 2.2 Linux VM

需要以下条件：

- 具备 root 权限。
- VM 已挂载 virtio-serial channel。
- 代理默认读取 `/sys/class/hwmon`。
- VM 内存在 `/dev/virtio-ports/org.vfio_sensor_bridge.0`。
- 使用 smartctl 采集硬盘温度时安装 `smartmontools`。

安装脚本可在 VM 内直接编译 `agent-linux`。VM 缺少 Rust 工具链时，可在兼容 Linux 主机编译后复制二进制。

## 3. 源码准备

在 PVE 主机或构建机上准备源码：

```sh
cd /root
git clone <repo-url> vfio-sensor-bridge
cd /root/vfio-sensor-bridge
```

已有源码时进入仓库目录：

```sh
cd /root/vfio-sensor-bridge
```

## 4. 编译

### 4.1 Rust 用户态组件

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

发布构建：

```sh
cargo build --release --workspace
```

生成的主要二进制：

```text
target/release/hostd
target/release/vsbctl
target/release/agent-linux
```

### 4.2 PVE 内核模块

在 PVE 主机上编译：

```sh
make -C kernel
```

成功后生成：

```text
kernel/vsb_hwmon.ko
```

## 5. PVE 主机安装

推荐使用仓库内安装脚本：

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/install-host.sh
```

脚本执行内容：

- 安装 DKMS 源码到 `/usr/src/vfio-sensor-bridge-0.1.0`。
- 使用 DKMS 构建并安装 `vsb_hwmon.ko`。
- 安装 `hostd` 到 `/usr/sbin/hostd`。
- 安装 `vsbctl` 到 `/usr/sbin/vsbctl`。
- 首次安装时写入 `/etc/vfio-sensor-bridge/hostd.toml`。
- 安装 systemd unit。
- 写入 `/etc/modules-load.d/vfio-sensor-bridge.conf`。
- 加载 `vsb_hwmon`。
- 启用并启动 `vfio-sensor-bridge-hostd.service`。

安装后检查：

```sh
systemctl status vfio-sensor-bridge-hostd.service
ls -l /dev/vfio-sensor-bridge
lsmod | grep vsb_hwmon
dkms status | grep vfio-sensor-bridge
```

## 6. PVE VM 通道配置

查看 VMID：

```sh
qm list
```

先查看将要写入的 PVE 配置：

```sh
vsbctl attach <VMID> --dry-run
```

写入 virtio-serial channel：

```sh
vsbctl attach <VMID>
```

重启或启动 VM：

```sh
qm reboot <VMID>
```

主机侧 socket 路径：

```text
/run/vfio-sensor-bridge/vm-<VMID>.sock
```

默认 virtio channel 名称：

```text
org.vfio_sensor_bridge.0
```

## 7. Linux VM 代理安装

### 7.1 VM 内直接安装

将源码复制到 VM 后执行：

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/install-agent.sh
```

脚本执行内容：

- 构建或复用 `target/release/agent-linux`。
- 安装二进制到 `/usr/sbin/agent-linux`。
- 首次安装时写入 `/etc/vfio-sensor-bridge/agent.toml`。
- 安装 systemd unit。
- 启用并启动 `vfio-sensor-bridge-agent.service`。

检查代理状态：

```sh
systemctl status vfio-sensor-bridge-agent.service
ls -l /dev/virtio-ports/org.vfio_sensor_bridge.0
```

### 7.2 使用预编译 agent

在兼容 Linux 主机编译：

```sh
cargo build --release --workspace --bin agent-linux
```

复制到 VM：

```sh
scp target/release/agent-linux root@<vm-ip>:/usr/sbin/agent-linux
```

在 VM 内安装配置和服务文件：

```sh
chmod 0755 /usr/sbin/agent-linux
mkdir -p /etc/vfio-sensor-bridge
cp packaging/agent.toml /etc/vfio-sensor-bridge/agent.toml
cp packaging/systemd/vfio-sensor-bridge-agent.service /etc/systemd/system/vfio-sensor-bridge-agent.service
systemctl daemon-reload
systemctl enable --now vfio-sensor-bridge-agent.service
```

## 8. 配置文件

### 8.1 PVE 主机配置

路径：

```text
/etc/vfio-sensor-bridge/hostd.toml
```

默认配置：

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

`hostd` 会把 PVE 侧持久传感器缓存保存在 hostd 配置文件同目录。默认路径：

```text
/etc/vfio-sensor-bridge/persistent-vms.json
```

PVE 重启后，`hostd` 启动时会读取该缓存，并用默认值重建持久 hwmon 传感器。

修改后重启主机守护进程：

```sh
systemctl restart vfio-sensor-bridge-hostd.service
```

hook 环境变量：

```text
VSB_EVENTS
VSB_VMID
VSB_HWMON_NAME
VSB_HWMON_PATH
VSB_SENSOR_COUNT
```

### 8.2 Linux VM 代理配置

路径：

```text
/etc/vfio-sensor-bridge/agent.toml
```

默认配置：

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

也可以用 agent CLI 修改配置：

```sh
agent-linux config show
agent-linux config validate
agent-linux config set --sample-seconds 1 --heartbeat-seconds 5 --rescan-seconds 10
agent-linux config persistent discover
agent-linux config persistent add --from 1 --default-value 65000
systemctl restart vfio-sensor-bridge-agent.service
```

持久传感器示例：

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

自定义 PVE 主机上显示的 hwmon 名称：

```toml
[agent]
hwmon_name_template = "gpu_{hostname}"
```

支持占位符：

- `{hostname}`：VM 主机名。
- `{agent_version}`：代理版本。

名称规则：

- 最大 31 字节。
- 允许 ASCII 字母、数字和 `_`。
- 代理会把其他字符替换为 `_`。
- 空值使用默认名称 `vsb_vm_<VMID>`。

修改后重启代理：

```sh
systemctl restart vfio-sensor-bridge-agent.service
```

启用 smartctl 硬盘温度采集示例：

```toml
[smartctl]
enabled = true
command = "/usr/sbin/smartctl"
device_globs = ["/dev/sata*", "/dev/sd*"]
poll_seconds = 30
label_template = "{device} {model} {serial} temperature"
```

VM 内探测 smartctl 温度源：

```sh
agent-linux --probe-smartctl /etc/vfio-sensor-bridge/agent.toml
```

`label_template` 可使用 `smartctl -i` 信息：`{model}`、`{serial}`、`{wwn}`、`{firmware}`、`{capacity}`、`{sector_sizes}`、`{rotation_rate}`、`{form_factor}`、`{ata_version}`、`{sata_version}`、`{model_family}`、`{vendor}`、`{product}`、`{revision}`、`{database}`、`{smart_available}`、`{smart_enabled}`。传感器 label 最长 128 字节。

代理按 `rescan_seconds` 重新展开 `device_globs`。硬盘增加、删除、路径变化或 label 渲染结果变化会生成新的 schema 并发送到 PVE 主机。

## 9. 验证

### 9.1 主机 smoke 测试

安装后执行：

```sh
vsbctl smoke --device /dev/vfio-sensor-bridge
```

源码树内完整 smoke 测试：

```sh
sh packaging/scripts/pve-smoke.sh
```

### 9.2 检查 hwmon 设备

查看所有 hwmon 名称：

```sh
for f in /sys/class/hwmon/hwmon*/name; do echo "$f: $(cat "$f")"; done
```

默认名称示例：

```text
vsb_vm_<VMID>
```

自定义名称示例：

```text
gpu_debian
```

读取传感器属性：

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

使用 `sensors` 查看：

```sh
sensors
```

### 9.3 VM 代理日志

VM 内查看：

```sh
journalctl -u vfio-sensor-bridge-agent.service -n 100 --no-pager
```

PVE 主机查看：

```sh
journalctl -u vfio-sensor-bridge-hostd.service -n 100 --no-pager
```

## 10. 升级

### 10.1 PVE 主机升级

```sh
cd /root/vfio-sensor-bridge
systemctl stop vfio-sensor-bridge-hostd.service
sh packaging/scripts/install-host.sh
systemctl status vfio-sensor-bridge-hostd.service
```

DKMS 会重新构建并安装当前版本模块。

### 10.2 VM 代理升级

```sh
cd /root/vfio-sensor-bridge
systemctl stop vfio-sensor-bridge-agent.service
sh packaging/scripts/install-agent.sh
systemctl status vfio-sensor-bridge-agent.service
```

## 11. 卸载

### 11.1 从 VM 移除 virtio 通道

PVE 主机执行：

```sh
vsbctl detach <VMID> --dry-run
vsbctl detach <VMID>
qm reboot <VMID>
```

### 11.2 卸载 VM 代理

VM 内执行：

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/uninstall-agent.sh
```

### 11.3 卸载 PVE 主机组件

PVE 主机执行：

```sh
cd /root/vfio-sensor-bridge
sh packaging/scripts/uninstall-host.sh
```

保留的配置文件：

```text
/etc/vfio-sensor-bridge/hostd.toml
/etc/vfio-sensor-bridge/agent.toml
```

## 12. 常见问题处理

### 12.1 `/dev/vfio-sensor-bridge` 缺失

```sh
modprobe vsb_hwmon
lsmod | grep vsb_hwmon
journalctl -k -n 100 --no-pager
```

### 12.2 DKMS 构建失败

确认 PVE headers 与运行内核匹配：

```sh
uname -r
dpkg -l | grep pve-headers
dkms status
```

重新安装 headers 后再次执行：

```sh
sh packaging/scripts/install-host.sh
```

### 12.3 VM 内 virtio port 缺失

PVE 主机检查 VM 配置：

```sh
qm config <VMID>
vsbctl attach <VMID> --dry-run
```

重新写入通道并重启 VM：

```sh
vsbctl attach <VMID>
qm reboot <VMID>
```

### 12.4 VM 缺少 `cargo`

采用预编译 agent 流程：在兼容 Linux 主机执行 `cargo build --release --workspace --bin agent-linux`，再复制 `target/release/agent-linux` 到 VM 的 `/usr/sbin/agent-linux`。

### 12.5 hook 执行失败

查看主机日志：

```sh
journalctl -u vfio-sensor-bridge-hostd.service -n 100 --no-pager
```

默认 hook 会执行：

```text
/usr/bin/systemctl restart fancontrol.service
```

调整 `/etc/vfio-sensor-bridge/hostd.toml` 中的 `[hooks]` 配置后重启 `hostd`。

### 12.6 脚本出现 CRLF 解释器错误

转换脚本为 LF：

```sh
sed -i 's/\r$//' packaging/scripts/*.sh
```

### 12.7 模块正在使用导致卸载失败

先停止主机服务，再卸载：

```sh
systemctl stop vfio-sensor-bridge-hostd.service
rmmod vsb_hwmon
```
