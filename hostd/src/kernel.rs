use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use vsb_protocol::SensorKind;

pub const VSB_MAX_SENSORS: usize = 128;
const VSB_LABEL_MAX: usize = 128;
const VSB_LABEL_LEN: usize = VSB_LABEL_MAX + 1;
const VSB_HWMON_NAME_LEN: usize = 32;
const VSB_SENSOR_TEMP: u32 = 1;
const VSB_SENSOR_FAN: u32 = 2;
const VSB_SENSOR_IN: u32 = 3;
const VSB_SENSOR_CURR: u32 = 4;
const VSB_SENSOR_POWER: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VsbSensorDesc {
    pub id: u32,
    pub kind: u32,
    pub channel: u32,
    pub reserved: u32,
    pub label: [u8; VSB_LABEL_LEN],
}

impl Default for VsbSensorDesc {
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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VsbSchema {
    pub vmid: u32,
    pub sensor_count: u32,
    pub hwmon_name: [u8; VSB_HWMON_NAME_LEN],
    pub reserved: u32,
    pub sensors: [VsbSensorDesc; VSB_MAX_SENSORS],
}

impl Default for VsbSchema {
    fn default() -> Self {
        Self {
            vmid: 0,
            sensor_count: 0,
            hwmon_name: [0; VSB_HWMON_NAME_LEN],
            reserved: 0,
            sensors: [VsbSensorDesc::default(); VSB_MAX_SENSORS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VsbSensorValue {
    pub id: u32,
    pub reserved: u32,
    pub value: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VsbValues {
    pub vmid: u32,
    pub value_count: u32,
    pub values: [VsbSensorValue; VSB_MAX_SENSORS],
}

impl Default for VsbValues {
    fn default() -> Self {
        Self {
            vmid: 0,
            value_count: 0,
            values: [VsbSensorValue::default(); VSB_MAX_SENSORS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VsbVmRef {
    vmid: u32,
}

pub struct KernelDevice {
    file: File,
}

impl KernelDevice {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        Ok(Self { file })
    }

    pub fn set_schema(&self, schema: &VsbSchema) -> Result<()> {
        ioctl_write(self.file.as_raw_fd(), request_none(0x01), schema)
    }

    pub fn set_values(&self, values: &VsbValues) -> Result<()> {
        ioctl_write(
            self.file.as_raw_fd(),
            request_write(0x02, std::mem::size_of::<VsbValues>()),
            values,
        )
    }

    pub fn remove_vm(&self, vmid: u32) -> Result<()> {
        let ref_payload = VsbVmRef { vmid };
        ioctl_write(
            self.file.as_raw_fd(),
            request_write(0x03, std::mem::size_of::<VsbVmRef>()),
            &ref_payload,
        )
    }
}

#[derive(Default)]
pub struct ChannelCounters {
    temp: u32,
    fan: u32,
    voltage: u32,
    current: u32,
    power: u32,
}

impl ChannelCounters {
    pub fn next(&mut self, kind: SensorKind) -> u32 {
        match kind {
            SensorKind::Temperature => {
                self.temp += 1;
                self.temp
            }
            SensorKind::Fan => {
                self.fan += 1;
                self.fan
            }
            SensorKind::Voltage => {
                let channel = self.voltage;
                self.voltage += 1;
                channel
            }
            SensorKind::Current => {
                self.current += 1;
                self.current
            }
            SensorKind::Power => {
                self.power += 1;
                self.power
            }
        }
    }
}

pub fn kernel_kind(kind: SensorKind) -> u32 {
    match kind {
        SensorKind::Temperature => VSB_SENSOR_TEMP,
        SensorKind::Fan => VSB_SENSOR_FAN,
        SensorKind::Voltage => VSB_SENSOR_IN,
        SensorKind::Current => VSB_SENSOR_CURR,
        SensorKind::Power => VSB_SENSOR_POWER,
    }
}

pub fn fixed_label(label: &str) -> [u8; VSB_LABEL_LEN] {
    let mut out = [0_u8; VSB_LABEL_LEN];
    let bytes = label.as_bytes();
    let len = bytes.len().min(VSB_LABEL_LEN - 1);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

pub fn fixed_hwmon_name(name: Option<&str>) -> [u8; VSB_HWMON_NAME_LEN] {
    let mut out = [0_u8; VSB_HWMON_NAME_LEN];
    let Some(name) = name else {
        return out;
    };
    let bytes = name.as_bytes();
    let len = bytes.len().min(VSB_HWMON_NAME_LEN - 1);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;
const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const VSB_IOCTL_MAGIC: u64 = b'V' as u64;

const fn request_none(nr: u64) -> libc::c_ulong {
    ((IOC_NONE << IOC_DIRSHIFT) | (VSB_IOCTL_MAGIC << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT))
        as libc::c_ulong
}

const fn request_write(nr: u64, size: usize) -> libc::c_ulong {
    ((IOC_WRITE << IOC_DIRSHIFT)
        | (VSB_IOCTL_MAGIC << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((size as u64) << IOC_SIZESHIFT)) as libc::c_ulong
}

fn ioctl_write<T>(fd: i32, request: libc::c_ulong, payload: &T) -> Result<()> {
    let ret = unsafe { libc::ioctl(fd, request, payload as *const T) };
    if ret < 0 {
        return Err(io::Error::last_os_error()).context("ioctl");
    }
    Ok(())
}
