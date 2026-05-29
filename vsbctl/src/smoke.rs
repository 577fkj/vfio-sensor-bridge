#[cfg(target_family = "unix")]
use anyhow::Context;
use anyhow::Result;

#[cfg(target_family = "unix")]
pub fn run(args: Vec<String>) -> Result<()> {
    use std::fs::{self, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    let mut vmid = 9000_u32;
    let mut device = PathBuf::from("/dev/vfio-sensor-bridge");
    let mut keep = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--vmid" => {
                vmid = iter
                    .next()
                    .context("missing --vmid value")?
                    .parse()
                    .context("invalid --vmid value")?
            }
            "--device" => device = PathBuf::from(iter.next().context("missing --device value")?),
            "--keep" => keep = true,
            other => anyhow::bail!("unknown smoke argument: {other}"),
        }
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&device)
        .with_context(|| format!("open {}", device.display()))?;

    let mut schema = Schema {
        vmid,
        sensor_count: 5,
        ..Schema::default()
    };
    schema.sensors[0] = desc(1, VSB_SENSOR_TEMP, 1, "smoke_temp");
    schema.sensors[1] = desc(2, VSB_SENSOR_FAN, 1, "smoke_fan");
    schema.sensors[2] = desc(3, VSB_SENSOR_IN, 0, "smoke_voltage");
    schema.sensors[3] = desc(4, VSB_SENSOR_CURR, 1, "smoke_current");
    schema.sensors[4] = desc(5, VSB_SENSOR_POWER, 1, "smoke_power");

    ioctl_write(file.as_raw_fd(), request_none(0x01), &schema).context("VSB_IOCTL_SET_SCHEMA")?;

    let mut values = Values {
        vmid,
        value_count: 5,
        ..Values::default()
    };
    values.values[0] = value(1, 42000);
    values.values[1] = value(2, 1500);
    values.values[2] = value(3, 12000);
    values.values[3] = value(4, 2500);
    values.values[4] = value(5, 75000000);

    ioctl_write(
        file.as_raw_fd(),
        request_write(0x02, std::mem::size_of::<Values>()),
        &values,
    )
    .context("VSB_IOCTL_SET_VALUES")?;

    thread::sleep(Duration::from_millis(100));

    let hwmon_name = format!("vsb_vm_{vmid}");
    let path = find_hwmon_path(&hwmon_name).context("created hwmon device not found")?;
    println!("created {}", path.display());

    for file_name in [
        "temp1_input",
        "fan1_input",
        "in0_input",
        "curr1_input",
        "power1_input",
    ] {
        let file_path = path.join(file_name);
        let value = fs::read_to_string(&file_path)
            .with_context(|| format!("read {}", file_path.display()))?;
        println!("{file_name}={}", value.trim());
    }

    if !keep {
        let vm_ref = VmRef { vmid };
        ioctl_write(
            file.as_raw_fd(),
            request_write(0x03, std::mem::size_of::<VmRef>()),
            &vm_ref,
        )
        .context("VSB_IOCTL_REMOVE_VM")?;
        println!("removed {hwmon_name}");
    }

    Ok(())
}

#[cfg(not(target_family = "unix"))]
pub fn run(_args: Vec<String>) -> Result<()> {
    anyhow::bail!("vsbctl smoke targets Linux hosts");
}

#[cfg(target_family = "unix")]
const VSB_MAX_SENSORS: usize = 128;
#[cfg(target_family = "unix")]
const VSB_LABEL_MAX: usize = 128;
#[cfg(target_family = "unix")]
const VSB_LABEL_LEN: usize = VSB_LABEL_MAX + 1;
#[cfg(target_family = "unix")]
const VSB_HWMON_NAME_LEN: usize = 32;
#[cfg(target_family = "unix")]
const VSB_SENSOR_TEMP: u32 = 1;
#[cfg(target_family = "unix")]
const VSB_SENSOR_FAN: u32 = 2;
#[cfg(target_family = "unix")]
const VSB_SENSOR_IN: u32 = 3;
#[cfg(target_family = "unix")]
const VSB_SENSOR_CURR: u32 = 4;
#[cfg(target_family = "unix")]
const VSB_SENSOR_POWER: u32 = 5;

#[cfg(target_family = "unix")]
#[repr(C)]
#[derive(Clone, Copy)]
struct SensorDesc {
    id: u32,
    kind: u32,
    channel: u32,
    reserved: u32,
    label: [u8; VSB_LABEL_LEN],
}

#[cfg(target_family = "unix")]
impl Default for SensorDesc {
    fn default() -> Self {
        Self {
            id: 0,
            kind: 0,
            channel: 0,
            reserved: 0,
            label: [0; VSB_LABEL_LEN],
        }
    }
}

#[cfg(target_family = "unix")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Schema {
    vmid: u32,
    sensor_count: u32,
    hwmon_name: [u8; VSB_HWMON_NAME_LEN],
    reserved: u32,
    sensors: [SensorDesc; VSB_MAX_SENSORS],
}

#[cfg(target_family = "unix")]
impl Default for Schema {
    fn default() -> Self {
        Self {
            vmid: 0,
            sensor_count: 0,
            hwmon_name: [0; VSB_HWMON_NAME_LEN],
            reserved: 0,
            sensors: [SensorDesc::default(); VSB_MAX_SENSORS],
        }
    }
}

#[cfg(target_family = "unix")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SensorValue {
    id: u32,
    reserved: u32,
    value: i64,
}

#[cfg(target_family = "unix")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Values {
    vmid: u32,
    value_count: u32,
    values: [SensorValue; VSB_MAX_SENSORS],
}

#[cfg(target_family = "unix")]
impl Default for Values {
    fn default() -> Self {
        Self {
            vmid: 0,
            value_count: 0,
            values: [SensorValue::default(); VSB_MAX_SENSORS],
        }
    }
}

#[cfg(target_family = "unix")]
#[repr(C)]
#[derive(Clone, Copy)]
struct VmRef {
    vmid: u32,
}

#[cfg(target_family = "unix")]
fn desc(id: u32, kind: u32, channel: u32, label: &str) -> SensorDesc {
    let mut out = SensorDesc {
        id,
        kind,
        channel,
        reserved: 0,
        label: [0; VSB_LABEL_LEN],
    };
    let bytes = label.as_bytes();
    let len = bytes.len().min(VSB_LABEL_LEN - 1);
    out.label[..len].copy_from_slice(&bytes[..len]);
    out
}

#[cfg(target_family = "unix")]
fn value(id: u32, value: i64) -> SensorValue {
    SensorValue {
        id,
        reserved: 0,
        value,
    }
}

#[cfg(target_family = "unix")]
fn find_hwmon_path(hwmon_name: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(name) = std::fs::read_to_string(path.join("name")) else {
            continue;
        };
        if name.trim() == hwmon_name {
            return Some(path);
        }
    }
    None
}

#[cfg(target_family = "unix")]
fn ioctl_write<T>(fd: i32, request: libc::c_ulong, payload: &T) -> Result<()> {
    let ret = unsafe { libc::ioctl(fd, request, payload as *const T) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("ioctl");
    }
    Ok(())
}

#[cfg(target_family = "unix")]
fn request_none(nr: u64) -> libc::c_ulong {
    const IOC_NRBITS: u64 = 8;
    const IOC_NRSHIFT: u64 = 0;
    const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
    const VSB_IOCTL_MAGIC: u64 = b'V' as u64;

    ((VSB_IOCTL_MAGIC << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT)) as libc::c_ulong
}

#[cfg(target_family = "unix")]
fn request_write(nr: u64, size: usize) -> libc::c_ulong {
    const IOC_NRBITS: u64 = 8;
    const IOC_TYPEBITS: u64 = 8;
    const IOC_SIZEBITS: u64 = 14;
    const IOC_NRSHIFT: u64 = 0;
    const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
    const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
    const IOC_WRITE: u64 = 1;
    const VSB_IOCTL_MAGIC: u64 = b'V' as u64;

    ((IOC_WRITE << IOC_DIRSHIFT)
        | (VSB_IOCTL_MAGIC << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((size as u64) << IOC_SIZESHIFT)) as libc::c_ulong
}
