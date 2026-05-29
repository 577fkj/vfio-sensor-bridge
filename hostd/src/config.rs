use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG: &str = "/etc/vfio-sensor-bridge/hostd.toml";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub daemon: DaemonConfig,
    pub virtio: VirtioConfig,
    pub heartbeat: HeartbeatConfig,
    pub hooks: super::hooks::HooksConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub run_dir: PathBuf,
    pub device: PathBuf,
    pub log_level: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            run_dir: PathBuf::from("/run/vfio-sensor-bridge"),
            device: PathBuf::from("/dev/vfio-sensor-bridge"),
            log_level: "info".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct VirtioConfig {
    pub channel_name: String,
    pub socket_template: String,
}

impl Default for VirtioConfig {
    fn default() -> Self {
        Self {
            channel_name: "org.vfio_sensor_bridge.0".to_string(),
            socket_template: "/run/vfio-sensor-bridge/vm-{vmid}.sock".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct HeartbeatConfig {
    pub timeout_seconds: u64,
    pub policy: String,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            policy: "warn_then_remove".to_string(),
        }
    }
}

pub fn load_config(path: &Path) -> Result<HostConfig> {
    match read_config(path) {
        Ok(raw) => toml::from_str(&raw).with_context(|| format!("parse {}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(HostConfig::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn read_config(path: &Path) -> io::Result<String> {
    let mut raw = String::new();
    File::open(path)?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "config exceeds maximum size",
        ));
    }
    Ok(raw)
}
