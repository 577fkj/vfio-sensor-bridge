use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use vsb_protocol::{SensorKind, MAX_SENSOR_LABEL_BYTES};

pub const DEFAULT_CONFIG: &str = "/etc/vfio-sensor-bridge/agent.toml";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub agent: AgentSection,
    #[serde(alias = "hba")]
    pub lsi_hba: LsiHbaSection,
    pub smartctl: SmartctlSection,
    pub persistent_sensor: Vec<PersistentSensorConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentSection {
    pub virtio_port: PathBuf,
    pub scan_root: PathBuf,
    pub hwmon_name_template: Option<String>,
    pub rescan_seconds: u64,
    pub sample_seconds: u64,
    pub heartbeat_seconds: u64,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            virtio_port: PathBuf::from("/dev/virtio-ports/org.vfio_sensor_bridge.0"),
            scan_root: PathBuf::from("/sys/class/hwmon"),
            hwmon_name_template: None,
            rescan_seconds: 10,
            sample_seconds: 1,
            heartbeat_seconds: 5,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LsiHbaSection {
    pub enabled: bool,
    pub devices: Vec<PathBuf>,
    pub max_ioc: u32,
    pub label_template: String,
}

impl Default for LsiHbaSection {
    fn default() -> Self {
        Self {
            enabled: false,
            devices: vec![PathBuf::from("/dev/mpt2ctl"), PathBuf::from("/dev/mpt3ctl")],
            max_ioc: 16,
            label_template: "{chip}".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SmartctlSection {
    pub enabled: bool,
    pub command: PathBuf,
    pub device_globs: Vec<String>,
    pub timeout_seconds: u64,
    pub poll_seconds: u64,
    pub label_template: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistentSensorConfig {
    pub id: String,
    pub kind: SensorKind,
    pub label: String,
    pub default_value: i64,
    pub source: PersistentSensorSource,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PersistentSensorSource {
    Hwmon {
        chip_name: Option<String>,
        input: String,
        source_label: Option<String>,
        device_path_contains: Option<String>,
    },
    Smartctl {
        device: PathBuf,
    },
    LsiHba {
        device: PathBuf,
        ioc: u32,
    },
}

impl Default for SmartctlSection {
    fn default() -> Self {
        Self {
            enabled: false,
            command: PathBuf::from("/usr/sbin/smartctl"),
            device_globs: vec!["/dev/sd*".to_string(), "/dev/sata*".to_string()],
            timeout_seconds: 10,
            poll_seconds: 30,
            label_template: "{device} temperature".to_string(),
        }
    }
}

pub fn load_config(path: &Path) -> Result<AgentConfig> {
    let config = match read_config(path) {
        Ok(raw) => toml::from_str(&raw).with_context(|| format!("parse {}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AgentConfig::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }?;
    validate_config(&config)?;
    Ok(config)
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

pub fn validate_config(config: &AgentConfig) -> Result<()> {
    if config.agent.rescan_seconds == 0 {
        anyhow::bail!("agent.rescan_seconds must be greater than zero");
    }
    if config.agent.sample_seconds == 0 {
        anyhow::bail!("agent.sample_seconds must be greater than zero");
    }
    if config.agent.heartbeat_seconds == 0 {
        anyhow::bail!("agent.heartbeat_seconds must be greater than zero");
    }
    if config.smartctl.timeout_seconds == 0 {
        anyhow::bail!("smartctl.timeout_seconds must be greater than zero");
    }
    if config.smartctl.poll_seconds == 0 {
        anyhow::bail!("smartctl.poll_seconds must be greater than zero");
    }

    let mut ids = std::collections::HashSet::new();
    for sensor in &config.persistent_sensor {
        validate_text(&sensor.id, 512, "persistent_sensor.id")?;
        validate_text(
            &sensor.label,
            MAX_SENSOR_LABEL_BYTES,
            "persistent_sensor.label",
        )?;
        if !ids.insert(&sensor.id) {
            anyhow::bail!("duplicate persistent_sensor.id {}", sensor.id);
        }
        match &sensor.source {
            PersistentSensorSource::Hwmon { input, .. } => {
                validate_text(input, 64, "persistent_sensor.source.input")?;
                if !input.ends_with("_input") {
                    anyhow::bail!("persistent hwmon source input must end with _input");
                }
            }
            PersistentSensorSource::Smartctl { device } => {
                validate_path(device, "persistent_sensor.source.device")?;
            }
            PersistentSensorSource::LsiHba { device, .. } => {
                validate_path(device, "persistent_sensor.source.device")?;
            }
        }
    }

    Ok(())
}

fn validate_text(value: &str, max_len: usize, name: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > max_len
        || value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        anyhow::bail!("invalid {name}");
    }
    Ok(())
}

fn validate_path(path: &Path, name: &str) -> Result<()> {
    let text = path.to_string_lossy();
    validate_text(&text, 512, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_packaging_config() {
        let config: AgentConfig =
            toml::from_str(include_str!("../../packaging/agent.toml")).unwrap();

        validate_config(&config).unwrap();
    }

    #[test]
    fn parses_persistent_hwmon_sensor() {
        let config: AgentConfig = toml::from_str(
            r#"
[agent]

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
"#,
        )
        .unwrap();

        validate_config(&config).unwrap();
        assert_eq!(config.persistent_sensor.len(), 1);
    }
}
