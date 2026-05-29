use super::config::{validate_config, AgentConfig, PersistentSensorConfig, PersistentSensorSource};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

pub struct AgentSet {
    pub scan_root: Option<PathBuf>,
    pub sample_seconds: Option<u64>,
    pub heartbeat_seconds: Option<u64>,
    pub rescan_seconds: Option<u64>,
}

pub struct HbaSet {
    pub enabled: Option<bool>,
    pub devices: Option<Vec<PathBuf>>,
    pub max_ioc: Option<u32>,
}

pub struct SmartctlSet {
    pub enabled: Option<bool>,
    pub device_globs: Option<Vec<String>>,
    pub timeout_seconds: Option<u64>,
    pub poll_seconds: Option<u64>,
}

pub fn default_config_text() -> &'static str {
    r#"[agent]
virtio_port = "/dev/virtio-ports/org.vfio_sensor_bridge.0"
scan_root = "/sys/class/hwmon"
# Optional hwmon name sent to the host. Supported placeholders:
# {hostname}, {agent_version}
# hwmon_name_template = "vsb_{hostname}"
rescan_seconds = 10
sample_seconds = 1
heartbeat_seconds = 5

[lsi_hba]
# LSI HBA IOC temperature polling is optional and disabled by default.
enabled = false
devices = ["/dev/mpt2ctl", "/dev/mpt3ctl"]
max_ioc = 16
# HBA temperature sensor label. Supported placeholders:
# {portname}, {chip}, {version}
label_template = "{chip}"

[smartctl]
# Disk temperature polling through smartctl is optional and disabled by default.
enabled = false
command = "/usr/sbin/smartctl"
# Device globs are expanded by the agent. Partitions are skipped when sysfs
# exposes them as partitions, so /dev/sata* can match Synology-style devices.
device_globs = ["/dev/sd*", "/dev/sata*"]
timeout_seconds = 10
poll_seconds = 30
# Disk temperature label. Supported placeholders:
# {device}, {path}, {model_family}, {model}, {serial}, {wwn}, {firmware},
# {capacity}, {sector_sizes}, {rotation_rate}, {form_factor}, {ata_version},
# {sata_version}, {vendor}, {product}, {revision}, {database},
# {smart_available}, {smart_enabled}
label_template = "{device} temperature"

# Persistent sensors can be added with:
# agent-linux config persistent discover
# agent-linux config persistent add --from 1 --default-value 65000
"#
}

pub fn init_config(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!("config already exists: {}", path.display());
    }
    write_text(path, default_config_text())
}

pub fn show_config(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(default_config_text().to_string())
        }
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

pub fn set_agent(path: &Path, set: AgentSet) -> Result<()> {
    let mut doc = read_doc(path)?;
    let agent = ensure_table(&mut doc, "agent")?;

    if let Some(scan_root) = set.scan_root {
        agent["scan_root"] = value(scan_root.to_string_lossy().to_string());
    }
    if let Some(seconds) = set.sample_seconds {
        agent["sample_seconds"] = value(seconds as i64);
    }
    if let Some(seconds) = set.heartbeat_seconds {
        agent["heartbeat_seconds"] = value(seconds as i64);
    }
    if let Some(seconds) = set.rescan_seconds {
        agent["rescan_seconds"] = value(seconds as i64);
    }

    write_doc(path, doc)
}

pub fn set_hba(path: &Path, set: HbaSet) -> Result<()> {
    let mut doc = read_doc(path)?;
    let hba = ensure_table(&mut doc, "lsi_hba")?;

    if let Some(enabled) = set.enabled {
        hba["enabled"] = value(enabled);
    }
    if let Some(devices) = set.devices {
        hba["devices"] = value(path_array(devices));
    }
    if let Some(max_ioc) = set.max_ioc {
        hba["max_ioc"] = value(max_ioc as i64);
    }

    write_doc(path, doc)
}

pub fn set_smartctl(path: &Path, set: SmartctlSet) -> Result<()> {
    let mut doc = read_doc(path)?;
    let smartctl = ensure_table(&mut doc, "smartctl")?;

    if let Some(enabled) = set.enabled {
        smartctl["enabled"] = value(enabled);
    }
    if let Some(globs) = set.device_globs {
        smartctl["device_globs"] = value(string_array(globs));
    }
    if let Some(seconds) = set.timeout_seconds {
        smartctl["timeout_seconds"] = value(seconds as i64);
    }
    if let Some(seconds) = set.poll_seconds {
        smartctl["poll_seconds"] = value(seconds as i64);
    }

    write_doc(path, doc)
}

pub fn add_persistent_sensor(path: &Path, sensor: PersistentSensorConfig) -> Result<()> {
    let mut doc = read_doc(path)?;
    {
        let sensors = ensure_persistent_array(&mut doc)?;
        if let Some(index) = persistent_index(sensors, &sensor.id) {
            sensors.remove(index);
        }
        sensors.push(persistent_table(sensor));
    }
    write_doc(path, doc)
}

pub fn remove_persistent_sensor(path: &Path, id: &str) -> Result<bool> {
    let mut doc = read_doc(path)?;
    let removed = {
        let sensors = ensure_persistent_array(&mut doc)?;
        if let Some(index) = persistent_index(sensors, id) {
            sensors.remove(index);
            true
        } else {
            false
        }
    };
    write_doc(path, doc)?;
    Ok(removed)
}

fn read_doc(path: &Path) -> Result<DocumentMut> {
    let raw = show_config(path)?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))
}

fn write_doc(path: &Path, doc: DocumentMut) -> Result<()> {
    let text = doc.to_string();
    let config: AgentConfig =
        toml::from_str(&text).with_context(|| format!("validate {}", path.display()))?;
    validate_config(&config)?;
    write_text(path, &text)
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent.toml");
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

fn ensure_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table> {
    if !doc.as_table().contains_key(name) {
        doc[name] = Item::Table(Table::new());
    }
    doc[name]
        .as_table_mut()
        .with_context(|| format!("{name} must be a TOML table"))
}

fn ensure_persistent_array(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
    if !doc.as_table().contains_key("persistent_sensor") {
        doc["persistent_sensor"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    doc["persistent_sensor"]
        .as_array_of_tables_mut()
        .context("persistent_sensor must be an array of tables")
}

fn persistent_index(sensors: &ArrayOfTables, id: &str) -> Option<usize> {
    sensors
        .iter()
        .position(|table| table.get("id").and_then(Item::as_str) == Some(id))
}

fn persistent_table(sensor: PersistentSensorConfig) -> Table {
    let mut table = Table::new();
    table["id"] = value(sensor.id);
    table["kind"] = value(sensor_kind_name(sensor.kind));
    table["label"] = value(sensor.label);
    table["default_value"] = value(sensor.default_value);
    table["source"] = Item::Table(source_table(sensor.source));
    table
}

fn source_table(source: PersistentSensorSource) -> Table {
    let mut table = Table::new();
    match source {
        PersistentSensorSource::Hwmon {
            chip_name,
            input,
            source_label,
            device_path_contains,
        } => {
            table["type"] = value("hwmon");
            set_optional_string(&mut table, "chip_name", chip_name);
            table["input"] = value(input);
            set_optional_string(&mut table, "source_label", source_label);
            set_optional_string(&mut table, "device_path_contains", device_path_contains);
        }
        PersistentSensorSource::Smartctl { device } => {
            table["type"] = value("smartctl");
            table["device"] = value(device.to_string_lossy().to_string());
        }
        PersistentSensorSource::LsiHba { device, ioc } => {
            table["type"] = value("lsi_hba");
            table["device"] = value(device.to_string_lossy().to_string());
            table["ioc"] = value(ioc as i64);
        }
    }
    table
}

fn set_optional_string(table: &mut Table, key: &str, value_opt: Option<String>) {
    if let Some(value_opt) = value_opt {
        table[key] = value(value_opt);
    }
}

fn path_array(paths: Vec<PathBuf>) -> Array {
    string_array(
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
    )
}

fn string_array(values: Vec<String>) -> Array {
    let mut array = Array::new();
    for item in values {
        array.push(item);
    }
    array
}

fn sensor_kind_name(kind: vsb_protocol::SensorKind) -> &'static str {
    match kind {
        vsb_protocol::SensorKind::Temperature => "temperature",
        vsb_protocol::SensorKind::Fan => "fan",
        vsb_protocol::SensorKind::Voltage => "voltage",
        vsb_protocol::SensorKind::Current => "current",
        vsb_protocol::SensorKind::Power => "power",
    }
}
