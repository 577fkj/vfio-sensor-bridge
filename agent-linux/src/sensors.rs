use super::config::{AgentConfig, PersistentSensorConfig, PersistentSensorSource};
use super::hba::{hba_ioc_label, read_mpt2_ioc_temperature_millicelsius};
use super::smartctl::{
    read_smartctl_temperature_millicelsius, scan_smartctl_sensors, source_for_device,
    SmartctlSource,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use vsb_protocol::{SensorDescriptor, SensorKind, SensorValue};

const MAX_SENSOR_FILE_BYTES: u64 = 4096;

#[derive(Clone, Debug)]
pub struct TrackedSensor {
    pub descriptor: SensorDescriptor,
    source: SensorSource,
}

#[derive(Clone, Debug)]
enum SensorSource {
    HwmonInput(PathBuf),
    Mpt2IocTemperature {
        device: PathBuf,
        ioc: u32,
    },
    SmartctlTemperature(SmartctlSource),
    Persistent {
        source: Option<Box<SensorSource>>,
        default_value: i64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscoveredSensor {
    pub index: usize,
    pub id: String,
    pub kind: SensorKind,
    pub label: String,
    pub value: i64,
    pub source: PersistentSensorSource,
    pub summary: String,
}

impl DiscoveredSensor {
    pub fn into_persistent_config(self, default_value: i64) -> PersistentSensorConfig {
        PersistentSensorConfig {
            id: self.id,
            kind: self.kind,
            label: self.label,
            default_value,
            source: self.source,
        }
    }
}

#[derive(Clone, Debug)]
struct HwmonCandidate {
    descriptor: SensorDescriptor,
    source: SensorSource,
    chip_name: String,
    input_name: String,
    source_label: Option<String>,
    device_path: String,
}

pub fn scan_all_sensors(config: &AgentConfig) -> Result<Vec<TrackedSensor>> {
    let (persistent, hidden_ids) = scan_persistent_sensors(config)?;
    let mut sensors = persistent;

    sensors.extend(
        scan_hwmon_sensors(&config.agent.scan_root)?
            .into_iter()
            .filter(|sensor| !hidden_ids.contains(&sensor.descriptor.id)),
    );
    sensors.extend(
        scan_hba_sensors(config)
            .into_iter()
            .filter(|sensor| !hidden_ids.contains(&sensor.descriptor.id)),
    );
    sensors.extend(
        scan_smartctl_sensors(&config.smartctl)
            .into_iter()
            .map(|sensor| TrackedSensor {
                descriptor: sensor.descriptor,
                source: SensorSource::SmartctlTemperature(sensor.source),
            })
            .filter(|sensor| !hidden_ids.contains(&sensor.descriptor.id)),
    );
    sensors.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    Ok(sensors)
}

fn scan_persistent_sensors(config: &AgentConfig) -> Result<(Vec<TrackedSensor>, HashSet<String>)> {
    let mut sensors = Vec::new();
    let mut hidden_ids = HashSet::new();

    for persistent in &config.persistent_sensor {
        let resolved = resolve_persistent_source(config, persistent)?;
        if let Some((_, shadow_id)) = &resolved {
            hidden_ids.insert(shadow_id.clone());
        }

        sensors.push(TrackedSensor {
            descriptor: SensorDescriptor {
                id: persistent.id.clone(),
                kind: persistent.kind,
                label: persistent.label.clone(),
                persistent: true,
                default_value: Some(persistent.default_value),
            },
            source: SensorSource::Persistent {
                source: resolved.map(|(source, _)| Box::new(source)),
                default_value: persistent.default_value,
            },
        });
    }

    Ok((sensors, hidden_ids))
}

fn resolve_persistent_source(
    config: &AgentConfig,
    persistent: &PersistentSensorConfig,
) -> Result<Option<(SensorSource, String)>> {
    match &persistent.source {
        PersistentSensorSource::Hwmon {
            chip_name,
            input,
            source_label,
            device_path_contains,
        } => {
            let matches: Vec<_> = scan_hwmon_candidates(&config.agent.scan_root)?
                .into_iter()
                .filter(|candidate| candidate.descriptor.kind == persistent.kind)
                .filter(|candidate| candidate.input_name == *input)
                .filter(|candidate| {
                    chip_name
                        .as_ref()
                        .map(|expected| candidate.chip_name == *expected)
                        .unwrap_or(true)
                })
                .filter(|candidate| {
                    source_label
                        .as_ref()
                        .map(|expected| {
                            candidate
                                .source_label
                                .as_deref()
                                .unwrap_or(&candidate.descriptor.label)
                                == expected
                        })
                        .unwrap_or(true)
                })
                .filter(|candidate| {
                    device_path_contains
                        .as_ref()
                        .map(|needle| candidate.device_path.contains(needle))
                        .unwrap_or(true)
                })
                .collect();

            if matches.len() == 1 {
                let candidate = matches.into_iter().next().expect("one candidate");
                Ok(Some((candidate.source, candidate.descriptor.id)))
            } else {
                Ok(None)
            }
        }
        PersistentSensorSource::Smartctl { device } => Ok(Some((
            SensorSource::SmartctlTemperature(source_for_device(&config.smartctl, device.clone())),
            smartctl_dynamic_id(device),
        ))),
        PersistentSensorSource::LsiHba { device, ioc } => Ok(Some((
            SensorSource::Mpt2IocTemperature {
                device: device.clone(),
                ioc: *ioc,
            },
            hba_dynamic_id(device, *ioc),
        ))),
    }
}

pub fn discover_persistent_candidates(config: &AgentConfig) -> Result<Vec<DiscoveredSensor>> {
    let mut discovered = Vec::new();

    for candidate in scan_hwmon_candidates(&config.agent.scan_root)? {
        let Ok(value) = read_source_value(&candidate.source) else {
            continue;
        };
        let source = PersistentSensorSource::Hwmon {
            chip_name: Some(candidate.chip_name.clone()),
            input: candidate.input_name.clone(),
            source_label: candidate.source_label.clone(),
            device_path_contains: Some(candidate.device_path.clone()),
        };
        discovered.push(DiscoveredSensor {
            index: discovered.len() + 1,
            id: stable_sensor_id(&[
                "hwmon",
                &candidate.chip_name,
                &candidate.input_name,
                candidate
                    .source_label
                    .as_deref()
                    .unwrap_or(&candidate.descriptor.label),
                &candidate.device_path,
            ]),
            kind: candidate.descriptor.kind,
            label: candidate.descriptor.label.clone(),
            value,
            source,
            summary: format!(
                "{} {} {}",
                candidate.chip_name,
                candidate.input_name,
                candidate.source_label.unwrap_or(candidate.descriptor.label)
            ),
        });
    }

    for sensor in scan_smartctl_sensors(&config.smartctl) {
        let Ok(value) = read_smartctl_temperature_millicelsius(&sensor.source) else {
            continue;
        };
        let device = sensor.source.device().to_path_buf();
        discovered.push(DiscoveredSensor {
            index: discovered.len() + 1,
            id: stable_sensor_id(&["smartctl", &device.display().to_string()]),
            kind: sensor.descriptor.kind,
            label: sensor.descriptor.label.clone(),
            value,
            source: PersistentSensorSource::Smartctl {
                device: device.clone(),
            },
            summary: format!("smartctl {} {}", device.display(), sensor.descriptor.label),
        });
    }

    if config.lsi_hba.enabled {
        for device in &config.lsi_hba.devices {
            for ioc in 0..config.lsi_hba.max_ioc {
                let Ok(value) = read_mpt2_ioc_temperature_millicelsius(device, ioc) else {
                    continue;
                };
                let label = hba_ioc_label(&config.lsi_hba, device, ioc);
                discovered.push(DiscoveredSensor {
                    index: discovered.len() + 1,
                    id: stable_sensor_id(&[
                        "lsi_hba",
                        &device.display().to_string(),
                        &ioc.to_string(),
                    ]),
                    kind: SensorKind::Temperature,
                    label: label.clone(),
                    value,
                    source: PersistentSensorSource::LsiHba {
                        device: device.clone(),
                        ioc,
                    },
                    summary: format!("lsi_hba {} ioc{} {}", device.display(), ioc, label),
                });
            }
        }
    }

    Ok(discovered)
}

fn scan_hwmon_sensors(root: &Path) -> Result<Vec<TrackedSensor>> {
    Ok(scan_hwmon_candidates(root)?
        .into_iter()
        .map(|candidate| TrackedSensor {
            descriptor: candidate.descriptor,
            source: candidate.source,
        })
        .collect())
}

fn scan_hwmon_candidates(root: &Path) -> Result<Vec<HwmonCandidate>> {
    let mut sensors = Vec::new();

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(sensors),
        Err(err) => return Err(err).with_context(|| format!("scan {}", root.display())),
    };

    for entry in entries {
        let hwmon_path = entry?.path();
        if !hwmon_path.is_dir() {
            continue;
        }

        let chip_name =
            read_trimmed(&hwmon_path.join("name")).unwrap_or_else(|_| "hwmon".to_string());
        let device_path = hwmon_device_path(&hwmon_path);
        let files = match fs::read_dir(&hwmon_path) {
            Ok(files) => files,
            Err(_) => continue,
        };

        for file in files {
            let input_path = file?.path();
            let Some(file_name) = input_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let Some((kind, prefix, index)) = parse_input_name(&file_name) else {
                continue;
            };

            let label_path = hwmon_path.join(format!("{prefix}{index}_label"));
            let source_label = read_trimmed(&label_path)
                .ok()
                .filter(|label| !label.is_empty());
            let label = source_label
                .clone()
                .unwrap_or_else(|| format!("{chip_name} {prefix}{index}"));
            let id = format!("{}:{prefix}{index}", hwmon_path.display());

            sensors.push(HwmonCandidate {
                descriptor: SensorDescriptor {
                    id,
                    kind,
                    label,
                    persistent: false,
                    default_value: None,
                },
                source: SensorSource::HwmonInput(input_path),
                chip_name: chip_name.clone(),
                input_name: file_name,
                source_label,
                device_path: device_path.clone(),
            });
        }
    }

    sensors.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    Ok(sensors)
}

fn scan_hba_sensors(config: &AgentConfig) -> Vec<TrackedSensor> {
    if !config.lsi_hba.enabled {
        return Vec::new();
    }

    let mut sensors = Vec::new();
    for device in &config.lsi_hba.devices {
        for ioc in 0..config.lsi_hba.max_ioc {
            if read_mpt2_ioc_temperature_millicelsius(device, ioc).is_err() {
                continue;
            }

            sensors.push(TrackedSensor {
                descriptor: SensorDescriptor {
                    id: hba_dynamic_id(device, ioc),
                    kind: SensorKind::Temperature,
                    label: hba_ioc_label(&config.lsi_hba, device, ioc),
                    persistent: false,
                    default_value: None,
                },
                source: SensorSource::Mpt2IocTemperature {
                    device: device.clone(),
                    ioc,
                },
            });
        }
    }

    sensors
}

fn parse_input_name(name: &str) -> Option<(SensorKind, &'static str, u32)> {
    let stem = name.strip_suffix("_input")?;

    for (prefix, kind) in [
        ("temp", SensorKind::Temperature),
        ("fan", SensorKind::Fan),
        ("in", SensorKind::Voltage),
        ("curr", SensorKind::Current),
        ("power", SensorKind::Power),
    ] {
        let Some(index) = stem.strip_prefix(prefix) else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        return Some((kind, prefix, index));
    }

    None
}

pub fn descriptors(sensors: &[TrackedSensor]) -> Vec<SensorDescriptor> {
    sensors
        .iter()
        .map(|sensor| sensor.descriptor.clone())
        .collect()
}

pub fn read_values(sensors: &[TrackedSensor]) -> Vec<SensorValue> {
    sensors
        .iter()
        .filter_map(|sensor| {
            let value = read_source_value(&sensor.source).ok()?;
            Some(SensorValue {
                id: sensor.descriptor.id.clone(),
                value,
            })
        })
        .collect()
}

fn read_source_value(source: &SensorSource) -> Result<i64> {
    match source {
        SensorSource::HwmonInput(path) => Ok(read_trimmed(path)?.parse::<i64>()?),
        SensorSource::Mpt2IocTemperature { device, ioc } => {
            read_mpt2_ioc_temperature_millicelsius(device, *ioc)
        }
        SensorSource::SmartctlTemperature(source) => read_smartctl_temperature_millicelsius(source),
        SensorSource::Persistent {
            source,
            default_value,
        } => match source {
            Some(source) => Ok(read_source_value(source).unwrap_or(*default_value)),
            None => Ok(*default_value),
        },
    }
}

pub fn hostname() -> String {
    read_trimmed(Path::new("/etc/hostname")).unwrap_or_else(|_| "unknown".to_string())
}

fn read_trimmed(path: &Path) -> io::Result<String> {
    let mut raw = String::new();
    File::open(path)?
        .take(MAX_SENSOR_FILE_BYTES)
        .read_to_string(&mut raw)?;
    Ok(raw.trim().to_string())
}

fn smartctl_dynamic_id(device: &Path) -> String {
    format!("smartctl:{}", device.display())
}

fn hba_dynamic_id(device: &Path, ioc: u32) -> String {
    let dev_name = device
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mptctl");
    format!("hba:{dev_name}:ioc{ioc}")
}

fn hwmon_device_path(hwmon_path: &Path) -> String {
    fs::canonicalize(hwmon_path.join("device"))
        .or_else(|_| fs::canonicalize(hwmon_path))
        .unwrap_or_else(|_| hwmon_path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn stable_sensor_id(parts: &[&str]) -> String {
    let mut out = String::with_capacity(64);
    for part in parts {
        for byte in part.bytes() {
            let ch = byte.to_ascii_lowercase();
            if ch.is_ascii_alphanumeric() {
                out.push(ch as char);
            } else if !out.ends_with('_') {
                out.push('_');
            }
            if out.len() >= 120 {
                break;
            }
        }
        if !out.ends_with('_') {
            out.push('_');
        }
    }

    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "persistent_sensor".to_string()
    } else {
        out
    }
}
