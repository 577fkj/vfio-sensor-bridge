# vfio-sensor-bridge

将虚拟机内的硬件传感器数据以标准 Linux hwmon 设备的形式桥接到 Proxmox VE 主机。

[English](README.md) | [Install guide](INSTALL.md) | [安装文档](INSTALL.zh.md)

## 概述

适用于 PCI 直通场景——直通后，温度、风扇、电压、电流或功率传感器仅在 VM 内可见。VM 代理通过专用 virtio-serial 通道将传感器读数上报给 PVE 主机，主机守护进程将数据写入内核 hwmon 模块，使 PVE、`sensors` 及风扇控制软件均可通过 `/sys/class/hwmon` 读取。

## 组件

| 组件 | 职责 |
|------|------|
| `vsb_hwmon.ko` | PVE 主机内核模块。暴露 `/dev/vfio-sensor-bridge`，并在 `/sys/class/hwmon` 下注册 hwmon 设备。 |
| `hostd` | PVE 主机守护进程。通过 QEMU socket 接收 VM 代理的帧 JSON 数据，驱动内核模块的 ioctl 更新，并在拓扑变更时执行 hook。 |
| `agent-linux` | Linux 虚拟机代理。扫描 `/sys/class/hwmon`，可选地采集 LSI/Broadcom HBA 温度和 smartctl 硬盘温度，并通过 virtio-serial 发布读数。 |
| `vsbctl` | PVE 辅助 CLI。管理 virtio-serial 通道的挂载，并执行 smoke 测试。 |

## 架构

```text
Linux VM
  agent-linux
    扫描 /sys/class/hwmon
    写入帧 JSON
      |
      | virtio-serial
      v
PVE 主机
  hostd
    接收 schema、sample、heartbeat
    调用 /dev/vfio-sensor-bridge ioctl
      |
      v
  vsb_hwmon.ko
    注册 /sys/class/hwmon/hwmonX
      |
      v
  sensors / fancontrol / PVE 监控
```

关键路径：

| 路径 | 说明 |
|------|------|
| `/dev/virtio-ports/org.vfio_sensor_bridge.0` | VM 侧 virtio 端口 |
| `/run/vfio-sensor-bridge/vm-{vmid}.sock` | 主机侧每 VM socket |
| `/dev/vfio-sensor-bridge` | 主机内核设备 |
| `kernel/include/uapi/vfio_sensor_bridge.h` | 公开 ioctl ABI 头文件 |

## 协议

帧格式：`u32_le length` + UTF-8 JSON 载荷。最大帧大小：256 KiB。

消息类型：

| 类型 | 用途 |
|------|------|
| `hello` | 代理启动与身份信息 |
| `schema` | 带 `generation` 的完整传感器列表 |
| `sample` | 引用当前 `generation` 的传感器值快照 |
| `heartbeat` | 代理存活信号 |
| `goodbye` | 代理正常退出通知 |

传感器值遵循 Linux hwmon 单位约定：

| 类别 | 单位 |
|------|------|
| 温度 | 毫摄氏度 |
| 风扇 | RPM |
| 电压 | 毫伏 |
| 电流 | 毫安 |
| 功率 | 微瓦 |

## 动态传感器

代理每 `rescan_seconds` 重新扫描一次 `/sys/class/hwmon`。当检测到传感器增加、删除、重命名或类型变更时，代理发送带递增 `generation` 的新 `schema`。主机守护进程重新注册该 VM 的 hwmon 设备，使 sysfs 属性集与 VM 状态保持一致。

代理也可通过 MPT2/MPT3 ioctl 直接从 LSI/Broadcom HBA 设备读取 IOC 温度，以及通过 `smartctl` 采集硬盘温度。

## 持久传感器

`[[persistent_sensor]]` 条目在来源不可用时仍保留在 schema 中。来源恢复前，代理上报 `default_value`。`hostd` 在心跳超时时保留持久传感器，并将其默认值写入内核 hwmon 设备。

## 心跳

任何有效消息都会重置存活计时器。超时策略：

| 策略 | 行为 |
|------|------|
| `warn_only` | 写入警告日志 |
| `remove_only` | 移除 VM hwmon 设备 |
| `warn_then_remove` | 写入警告日志并移除 VM hwmon 设备 |

默认：`timeout_seconds = 30`，`policy = "warn_then_remove"`。

## Hooks

`hostd` 在设备或传感器拓扑变更完成后执行命令。Hook 事件有防抖机制，`debounce_seconds` 内的多次变更只触发一次执行。

可用事件：`device_created`、`device_removed`、`sensor_added`、`sensor_removed`、`sensor_changed`、`vm_offline`。

Hook 环境变量：`VSB_EVENTS`、`VSB_VMID`、`VSB_HWMON_NAME`、`VSB_HWMON_PATH`、`VSB_SENSOR_COUNT`。

## 配置文件路径

| 文件 | 用途 |
|------|------|
| `/etc/vfio-sensor-bridge/hostd.toml` | 主机守护进程配置 |
| `/etc/vfio-sensor-bridge/agent.toml` | 虚拟机代理配置 |
| `/etc/vfio-sensor-bridge/persistent-vms.json` | 持久传感器缓存（由 hostd 写入） |

编译、安装、详细配置说明与常见问题见 [INSTALL.zh.md](INSTALL.zh.md)。
