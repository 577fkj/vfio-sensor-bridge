mod qm;
mod smoke;
mod status;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        print_usage();
        return Ok(());
    };

    if is_help_arg(command) {
        print_usage();
        return Ok(());
    }

    match command.as_str() {
        "help" => print_help_topic(args.get(1).map(String::as_str)),
        "attach" => attach(&args[1..]),
        "detach" => detach(&args[1..]),
        "smoke" => {
            if has_help_arg(&args[1..]) {
                print_smoke_usage();
                Ok(())
            } else {
                smoke::run(args[1..].to_vec())
            }
        }
        "status" => {
            if has_help_arg(&args[1..]) {
                print_status_usage();
                Ok(())
            } else {
                status::run(&args[1..])
            }
        }
        _ => {
            print_usage();
            anyhow::bail!("unknown command: {command}")
        }
    }
}

fn attach(args: &[String]) -> Result<()> {
    if has_help_arg(args) {
        print_attach_usage();
        return Ok(());
    }

    let (vmid, dry_run) = parse_vm_command("attach", args)?;
    qm::attach(&vmid, dry_run)
}

fn detach(args: &[String]) -> Result<()> {
    if has_help_arg(args) {
        print_detach_usage();
        return Ok(());
    }

    let (vmid, dry_run) = parse_vm_command("detach", args)?;
    qm::detach(&vmid, dry_run)
}

fn parse_vm_command(command: &str, args: &[String]) -> Result<(String, bool)> {
    let mut vmid = None;
    let mut dry_run = false;

    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            other if other.starts_with('-') => anyhow::bail!("unknown {command} option: {other}"),
            other => {
                if vmid.is_some() {
                    anyhow::bail!("unexpected {command} argument: {other}");
                }
                vmid = Some(other.to_string());
            }
        }
    }

    let vmid = vmid.with_context(|| format!("missing VMID for {command}"))?;
    Ok((vmid, dry_run))
}

fn print_help_topic(topic: Option<&str>) -> Result<()> {
    match topic {
        None => print_usage(),
        Some("attach") => print_attach_usage(),
        Some("detach") => print_detach_usage(),
        Some("smoke") => print_smoke_usage(),
        Some("status") => print_status_usage(),
        Some("help") => print_usage(),
        Some(other) => {
            print_usage();
            anyhow::bail!("unknown help topic: {other}");
        }
    }
    Ok(())
}

fn is_help_arg(arg: &str) -> bool {
    arg == "-h" || arg == "--help"
}

fn has_help_arg(args: &[String]) -> bool {
    args.iter().any(|arg| is_help_arg(arg))
}

fn print_usage() {
    println!(
        r#"vfio-sensor-bridge PVE helper

Usage:
  vsbctl <command> [options]
  vsbctl help [command]
  vsbctl --help

Commands:
  attach <VMID> [--dry-run]        Add the VM virtio-serial channel.
  detach <VMID> [--dry-run]        Remove the VM virtio-serial channel args.
  smoke [options]                  Exercise the kernel ioctl and hwmon path.
  status [--vmid <VMID>]          Show live sensor data and agent socket state.
  help [command]                   Show full usage or command-specific usage.

Global options:
  -h, --help                       Show this usage.

Examples:
  vsbctl attach 100 --dry-run
  vsbctl attach 100
  vsbctl detach 100
  vsbctl smoke --vmid 9000 --device /dev/vfio-sensor-bridge
  vsbctl status
  vsbctl status --vmid 100

Use:
  vsbctl help attach
  vsbctl help detach
  vsbctl help smoke
  vsbctl help status"#
    );
}

fn print_attach_usage() {
    println!(
        r#"Usage:
  vsbctl attach <VMID> [--dry-run]

Purpose:
  Add the vfio-sensor-bridge virtio-serial channel to a Proxmox VM config.

Effect:
  Creates /run/vfio-sensor-bridge when needed.
  Reads current VM args with: qm config <VMID>
  Appends these QEMU args through: qm set <VMID> --args ...
    -chardev socket,id=vsb0,path=/run/vfio-sensor-bridge/vm-<VMID>.sock,server=on,wait=off
    -device virtio-serial-pci,id=vsbserial0
    -device virtserialport,chardev=vsb0,name=org.vfio_sensor_bridge.0

Arguments:
  <VMID>                           Proxmox VMID, unsigned integer.

Options:
  --dry-run                        Print the qm set command.
  -h, --help                       Show this usage.

After attach:
  qm reboot <VMID>
  Inside the VM, verify:
    ls -l /dev/virtio-ports/org.vfio_sensor_bridge.0"#
    );
}

fn print_detach_usage() {
    println!(
        r#"Usage:
  vsbctl detach <VMID> [--dry-run]

Purpose:
  Remove vfio-sensor-bridge QEMU args from a Proxmox VM config.

Effect:
  Reads current VM args with: qm config <VMID>
  Removes only the channel args managed by vsbctl:
    -chardev socket,id=vsb0,path=/run/vfio-sensor-bridge/vm-<VMID>.sock,server=on,wait=off
    -device virtio-serial-pci,id=vsbserial0
    -device virtserialport,chardev=vsb0,name=org.vfio_sensor_bridge.0

Arguments:
  <VMID>                           Proxmox VMID, unsigned integer.

Options:
  --dry-run                        Print the qm set command.
  -h, --help                       Show this usage.

After detach:
  qm reboot <VMID>"#
    );
}

fn print_smoke_usage() {
    println!(
        r#"Usage:
  vsbctl smoke [--vmid <VMID>] [--device <PATH>] [--keep]

Purpose:
  Validate the host kernel module path without a VM agent.

Effect:
  Opens /dev/vfio-sensor-bridge by default.
  Sends VSB_IOCTL_SET_SCHEMA for one VM hwmon device.
  Sends VSB_IOCTL_SET_VALUES for temp, fan, voltage, current, and power values.
  Looks for /sys/class/hwmon/hwmonX/name = vsb_vm_<VMID>.
  Prints the created hwmon input files.
  Removes the smoke VM device unless --keep is set.

Options:
  --vmid <VMID>                    Smoke-test VMID. Default: 9000.
  --device <PATH>                  Kernel misc device. Default: /dev/vfio-sensor-bridge.
  --keep                           Leave the created hwmon device in sysfs.
  -h, --help                       Show this usage.

Examples:
  vsbctl smoke
  vsbctl smoke --vmid 9000 --keep
  vsbctl smoke --device /dev/vfio-sensor-bridge"#
    );
}

fn print_status_usage() {
    println!(
        r#"Usage:
  vsbctl status [--vmid <VMID>]

Purpose:
  Show live sensor data and agent socket state for all vfio-sensor-bridge
  managed hwmon devices on this PVE host.  Useful for diagnosing sensor
  delivery issues without running `sensors` or parsing sysfs manually.

What is shown:
  - Each vsb_vm_<VMID> hwmon device found under /sys/class/hwmon
  - The sysfs path of the hwmon device
  - Whether the VM's QEMU socket file exists in /run/vfio-sensor-bridge
    (socket present = VM is running with the channel configured)
  - All sensor attributes (*_input) with their label and formatted value
  - VMIDs that have a socket file but no hwmon entry yet (schema pending)

Sensor units (as stored in sysfs / kernel module):
  temp   millidegree Celsius  → displayed as °C
  fan    RPM                  → displayed as RPM
  in     millivolt            → displayed as V
  curr   milliampere          → displayed as A
  power  microwatt            → displayed as W

Options:
  --vmid <VMID>                    Limit output to a single VM.
  -h, --help                       Show this usage.

Examples:
  vsbctl status
  vsbctl status --vmid 100"#
    );
}
