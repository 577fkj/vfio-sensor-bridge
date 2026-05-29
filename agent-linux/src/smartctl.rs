use super::config::SmartctlSection;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use vsb_protocol::{SensorDescriptor, SensorKind, MAX_SENSOR_LABEL_BYTES};

#[derive(Clone, Debug)]
pub struct SmartctlSensor {
    pub descriptor: SensorDescriptor,
    pub source: SmartctlSource,
}

#[derive(Clone, Debug)]
pub struct SmartctlSource {
    command: PathBuf,
    device: PathBuf,
    timeout: Duration,
    poll_interval: Duration,
    cache: Arc<Mutex<Option<CachedTemperature>>>,
}

#[derive(Clone, Copy, Debug)]
struct CachedTemperature {
    value: i64,
    read_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct DiscoveryTemperature {
    value: Option<i64>,
    read_at: Instant,
}

#[derive(Clone, Debug, Default)]
struct SmartctlIdentity {
    model_family: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    wwn: Option<String>,
    firmware: Option<String>,
    capacity: Option<String>,
    sector_sizes: Option<String>,
    rotation_rate: Option<String>,
    form_factor: Option<String>,
    ata_version: Option<String>,
    sata_version: Option<String>,
    vendor: Option<String>,
    product: Option<String>,
    revision: Option<String>,
    database: Option<String>,
    smart_available: Option<String>,
    smart_enabled: Option<String>,
}

#[derive(Clone, Debug)]
struct DiscoveryIdentity {
    identity: Option<SmartctlIdentity>,
    read_at: Instant,
}

thread_local! {
    static DISCOVERY_CACHE: RefCell<BTreeMap<String, DiscoveryTemperature>> =
        const { RefCell::new(BTreeMap::new()) };
    static DISCOVERY_IDENTITY_CACHE: RefCell<BTreeMap<String, DiscoveryIdentity>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub fn scan_smartctl_sensors(config: &SmartctlSection) -> Vec<SmartctlSensor> {
    if !config.enabled {
        return Vec::new();
    }

    let mut sensors = Vec::new();
    for device in expand_device_patterns(&config.device_globs) {
        let source = SmartctlSource::new(
            config.command.clone(),
            device.clone(),
            Duration::from_secs(config.timeout_seconds.max(1)),
            Duration::from_secs(config.poll_seconds.max(1)),
            None,
        );
        let Ok(value) = read_smartctl_temperature_millicelsius_for_discovery(&source) else {
            continue;
        };
        let identity = read_smartctl_identity_for_discovery(&source);

        sensors.push(SmartctlSensor {
            descriptor: SensorDescriptor {
                id: format!("smartctl:{}", device.display()),
                kind: SensorKind::Temperature,
                label: render_smartctl_label(&config.label_template, &device, &identity),
                persistent: false,
                default_value: None,
            },
            source: source.with_cached_value(value),
        });
    }

    sensors.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    sensors
}

pub fn read_smartctl_temperature_millicelsius(source: &SmartctlSource) -> Result<i64> {
    let now = Instant::now();
    {
        let cache = source.cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = *cache {
            if now.duration_since(cached.read_at) < source.poll_interval {
                return Ok(cached.value);
            }
        }
    }

    let value = read_smartctl_temperature_millicelsius_uncached(source)?;
    let mut cache = source.cache.lock().unwrap_or_else(|err| err.into_inner());
    *cache = Some(CachedTemperature {
        value,
        read_at: now,
    });
    Ok(value)
}

pub fn probe_smartctl_temperatures(config: &SmartctlSection) -> Result<()> {
    let mut found = 0_u32;
    let mut probe_config = config.clone();
    probe_config.enabled = true;

    for sensor in scan_smartctl_sensors(&probe_config) {
        let value = read_smartctl_temperature_millicelsius(&sensor.source)?;
        found += 1;
        println!(
            "{} {}: {}",
            sensor.source.device.display(),
            sensor.descriptor.label,
            value
        );
    }

    if found == 0 {
        anyhow::bail!("no smartctl temperature found")
    }

    Ok(())
}

impl SmartctlSource {
    pub fn new(
        command: PathBuf,
        device: PathBuf,
        timeout: Duration,
        poll_interval: Duration,
        cached_value: Option<i64>,
    ) -> Self {
        Self {
            command,
            device,
            timeout,
            poll_interval,
            cache: Arc::new(Mutex::new(cached_value.map(|value| CachedTemperature {
                value,
                read_at: Instant::now(),
            }))),
        }
    }

    fn with_cached_value(&self, value: i64) -> Self {
        Self::new(
            self.command.clone(),
            self.device.clone(),
            self.timeout,
            self.poll_interval,
            Some(value),
        )
    }

    pub fn device(&self) -> &Path {
        &self.device
    }
}

pub fn source_for_device(config: &SmartctlSection, device: PathBuf) -> SmartctlSource {
    SmartctlSource::new(
        config.command.clone(),
        device,
        Duration::from_secs(config.timeout_seconds.max(1)),
        Duration::from_secs(config.poll_seconds.max(1)),
        None,
    )
}

fn read_smartctl_temperature_millicelsius_uncached(source: &SmartctlSource) -> Result<i64> {
    let output = run_smartctl(source, "-A")?;
    parse_smartctl_temperature_millicelsius(&output).with_context(|| {
        format!(
            "parse smartctl temperature from {}",
            source.device.display()
        )
    })
}

fn read_smartctl_temperature_millicelsius_for_discovery(source: &SmartctlSource) -> Result<i64> {
    let now = Instant::now();
    let key = smartctl_cache_key(source);

    let cached = DISCOVERY_CACHE.with(|cache| cache.borrow().get(&key).copied());
    if let Some(cached) = cached {
        if now.duration_since(cached.read_at) < source.poll_interval {
            return cached
                .value
                .with_context(|| format!("cached smartctl miss for {}", source.device.display()));
        }
    }

    let value = read_smartctl_temperature_millicelsius_uncached(source);
    DISCOVERY_CACHE.with(|cache| {
        cache.borrow_mut().insert(
            key,
            DiscoveryTemperature {
                value: value.as_ref().ok().copied(),
                read_at: now,
            },
        );
    });
    value
}

fn smartctl_cache_key(source: &SmartctlSource) -> String {
    format!("{}\0{}", source.command.display(), source.device.display())
}

fn read_smartctl_identity_for_discovery(source: &SmartctlSource) -> SmartctlIdentity {
    let now = Instant::now();
    let key = smartctl_cache_key(source);

    let cached = DISCOVERY_IDENTITY_CACHE.with(|cache| cache.borrow().get(&key).cloned());
    if let Some(cached) = cached {
        if now.duration_since(cached.read_at) < source.poll_interval {
            return cached.identity.unwrap_or_default();
        }
    }

    let identity = read_smartctl_identity_uncached(source).ok();
    DISCOVERY_IDENTITY_CACHE.with(|cache| {
        cache.borrow_mut().insert(
            key,
            DiscoveryIdentity {
                identity: identity.clone(),
                read_at: now,
            },
        );
    });
    identity.unwrap_or_default()
}

fn read_smartctl_identity_uncached(source: &SmartctlSource) -> Result<SmartctlIdentity> {
    let output = run_smartctl(source, "-i")?;
    Ok(parse_smartctl_identity(&output))
}

fn run_smartctl(source: &SmartctlSource, mode: &str) -> Result<String> {
    let mut child = Command::new(&source.command)
        .arg(mode)
        .arg(&source.device)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", source.command.display()))?;
    let start = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            return output_to_string(output.stdout, output.stderr);
        }

        if start.elapsed() >= source.timeout {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            let text = output_to_string(output.stdout, output.stderr)?;
            anyhow::bail!("smartctl timed out after {:?}: {}", source.timeout, text);
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn output_to_string(stdout: Vec<u8>, stderr: Vec<u8>) -> Result<String> {
    let mut raw = stdout;
    if !stderr.is_empty() {
        raw.push(b'\n');
        raw.extend_from_slice(&stderr);
    }
    String::from_utf8(raw).context("smartctl output is not UTF-8")
}

fn parse_smartctl_identity(output: &str) -> SmartctlIdentity {
    let mut identity = SmartctlIdentity::default();

    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let Some(value) = normalize_identity_value(value) else {
            continue;
        };

        match key {
            "Model Family" => identity.model_family = Some(value),
            "Device Model" | "Model Number" => identity.model = Some(value),
            "Serial Number" => identity.serial = Some(value),
            "LU WWN Device Id" | "Logical Unit id" => identity.wwn = Some(value),
            "Firmware Version" => identity.firmware = Some(value),
            "User Capacity" | "Total NVM Capacity" | "Namespace 1 Size/Capacity" => {
                identity.capacity = Some(value);
            }
            "Sector Size" | "Sector Sizes" => identity.sector_sizes = Some(value),
            "Rotation Rate" => identity.rotation_rate = Some(value),
            "Form Factor" => identity.form_factor = Some(value),
            "ATA Version is" | "ATA Version" => identity.ata_version = Some(value),
            "SATA Version is" | "SATA Version" => identity.sata_version = Some(value),
            "Vendor" => identity.vendor = Some(value),
            "Product" => identity.product = Some(value),
            "Revision" => identity.revision = Some(value),
            "Device is" => identity.database = Some(value),
            "SMART support is" => {
                if value.starts_with("Available") {
                    identity.smart_available = Some(value);
                } else if value.starts_with("Enabled") || value.starts_with("Disabled") {
                    identity.smart_enabled = Some(value);
                }
            }
            _ => {}
        }
    }

    if identity.model.is_none() {
        identity.model = match (&identity.vendor, &identity.product) {
            (Some(vendor), Some(product)) => Some(format!("{vendor} {product}")),
            (Some(vendor), None) => Some(vendor.clone()),
            (None, Some(product)) => Some(product.clone()),
            (None, None) => None,
        };
    }

    if identity.firmware.is_none() {
        identity.firmware = identity.revision.clone();
    }

    identity
}

fn normalize_identity_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_smartctl_temperature_millicelsius(output: &str) -> Option<i64> {
    let mut best_attr: Option<(u8, i64)> = None;

    for line in output.lines() {
        if let Some(value) = parse_temperature_line(line) {
            return Some(value * 1000);
        }

        let Some((priority, value)) = parse_ata_temperature_attribute(line) else {
            continue;
        };
        match best_attr {
            Some((best_priority, _)) if best_priority <= priority => {}
            _ => best_attr = Some((priority, value)),
        }
    }

    best_attr.map(|(_, value)| value * 1000)
}

fn parse_temperature_line(line: &str) -> Option<i64> {
    let lower = line.to_ascii_lowercase();
    let lower = lower.trim_start();
    if !(lower.starts_with("temperature:")
        || lower.starts_with("current drive temperature:")
        || lower.starts_with("temperature sensor "))
    {
        return None;
    }
    if !(lower.contains("celsius") || lower.ends_with(" c") || lower.contains(" c ")) {
        return None;
    }

    let (_, value_part) = line.split_once(':')?;
    let value = first_i64(value_part)?;
    plausible_celsius(value)
}

fn parse_ata_temperature_attribute(line: &str) -> Option<(u8, i64)> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 10 {
        return None;
    }

    let id = cols[0].parse::<u16>().ok()?;
    let name = cols[1].to_ascii_lowercase();
    let is_temperature =
        matches!(id, 190 | 194 | 231) || name.contains("temperature") || name.contains("airflow");
    if !is_temperature {
        return None;
    }

    let value = cols[9..].iter().find_map(|part| first_i64(part))?;
    let value = plausible_celsius(value)?;
    let priority = match id {
        194 => 0,
        190 => 1,
        231 => 3,
        _ => 2,
    };
    Some((priority, value))
}

fn first_i64(input: &str) -> Option<i64> {
    for token in input.split(|ch: char| !(ch == '-' || ch.is_ascii_digit())) {
        if token.is_empty() || token == "-" {
            continue;
        }
        if let Ok(value) = token.parse::<i64>() {
            return Some(value);
        }
    }
    None
}

fn plausible_celsius(value: i64) -> Option<i64> {
    (-60..=200).contains(&value).then_some(value)
}

fn expand_device_patterns(patterns: &[String]) -> Vec<PathBuf> {
    let mut devices = BTreeSet::new();

    for pattern in patterns {
        if !has_wildcard(pattern) {
            let path = PathBuf::from(pattern);
            if is_candidate_device(&path) {
                devices.insert(path);
            }
            continue;
        }

        let path = Path::new(pattern);
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let Some(name_pattern) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            continue;
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if wildcard_match(name_pattern, file_name) && is_candidate_device(&entry.path()) {
                devices.insert(entry.path());
            }
        }
    }

    devices.into_iter().collect()
}

fn has_wildcard(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0_usize, 0_usize);
    let mut star = None;
    let mut retry_text = 0_usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            retry_text = t;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            retry_text += 1;
            t = retry_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

fn is_candidate_device(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    let sys_block = Path::new("/sys/class/block").join(name);
    if sys_block.join("partition").exists() {
        return false;
    }
    if sys_block.exists() {
        return true;
    }

    if looks_like_partition_name(name) {
        return false;
    }

    match fs::metadata(path) {
        Ok(metadata) => is_device_file(&metadata),
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

#[cfg(target_family = "unix")]
use std::os::unix::fs::FileTypeExt;

#[cfg(target_family = "unix")]
fn is_device_file(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_block_device()
}

#[cfg(not(target_family = "unix"))]
fn is_device_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
}

fn looks_like_partition_name(name: &str) -> bool {
    if let Some((base, suffix)) = name.rsplit_once('p') {
        if base.chars().any(|ch| ch.is_ascii_digit())
            && !suffix.is_empty()
            && suffix.chars().all(|ch| ch.is_ascii_digit())
        {
            return true;
        }
    }

    let split_at = name
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8());
    let Some(split_at) = split_at else {
        return false;
    };
    if split_at == name.len() {
        return false;
    }
    let base = &name[..split_at];
    is_lettered_disk_base(base, "sd")
        || is_lettered_disk_base(base, "vd")
        || is_lettered_disk_base(base, "xvd")
}

fn is_lettered_disk_base(value: &str, prefix: &str) -> bool {
    let Some(suffix) = value.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == 1 && suffix.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn render_smartctl_label(template: &str, device: &Path, identity: &SmartctlIdentity) -> String {
    let device_name = device
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("disk");
    let rendered = template
        .replace("{device}", device_name)
        .replace("{path}", &device.display().to_string())
        .replace("{model_family}", identity_value(&identity.model_family))
        .replace("{model}", identity_value(&identity.model))
        .replace("{serial}", identity_value(&identity.serial))
        .replace("{wwn}", identity_value(&identity.wwn))
        .replace("{firmware}", identity_value(&identity.firmware))
        .replace("{capacity}", identity_value(&identity.capacity))
        .replace("{sector_sizes}", identity_value(&identity.sector_sizes))
        .replace("{rotation_rate}", identity_value(&identity.rotation_rate))
        .replace("{form_factor}", identity_value(&identity.form_factor))
        .replace("{ata_version}", identity_value(&identity.ata_version))
        .replace("{sata_version}", identity_value(&identity.sata_version))
        .replace("{vendor}", identity_value(&identity.vendor))
        .replace("{product}", identity_value(&identity.product))
        .replace("{revision}", identity_value(&identity.revision))
        .replace("{database}", identity_value(&identity.database))
        .replace(
            "{smart_available}",
            identity_value(&identity.smart_available),
        )
        .replace("{smart_enabled}", identity_value(&identity.smart_enabled));
    normalize_label(&rendered)
}

fn identity_value(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

fn normalize_label(label: &str) -> String {
    let mut out = String::new();
    for ch in label.trim().chars() {
        if ch.is_control() {
            continue;
        }

        if out.len() + ch.len_utf8() > MAX_SENSOR_LABEL_BYTES {
            break;
        }

        out.push(ch);
    }

    if out.is_empty() {
        "disk temperature".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ata_temperature_attribute_raw_value() {
        let output = r#"
194 Temperature_Celsius                                              0x0002   107   107   000    Old_age   Always       -       56 (Min/Max 15/69)
231 Temperature_Celsius                                              0x0032   100   100   000    Old_age   Always       -       0
"#;

        assert_eq!(
            parse_smartctl_temperature_millicelsius(output),
            Some(56_000)
        );
    }

    #[test]
    fn parses_smartctl_identity() {
        let output = r#"
Device Model:     HUS726060ALE611
Serial Number:    NCG8ARHS
LU WWN Device Id: 5 000cca 24dc3cb3b
Firmware Version: APGL0001
User Capacity:    6,001,175,126,016 bytes [6.00 TB]
Sector Sizes:     512 bytes logical, 4096 bytes physical
Rotation Rate:    7200 rpm
Form Factor:      3.5 inches
Device is:        Not in smartctl database [for details use: -P showall]
ATA Version is:   ACS-2, ATA8-ACS T13/1699-D revision 4
SATA Version is:  SATA 3.1, 6.0 Gb/s (current: 6.0 Gb/s)
SMART support is: Available - device has SMART capability.
SMART support is: Enabled
"#;
        let identity = parse_smartctl_identity(output);

        assert_eq!(identity.model.as_deref(), Some("HUS726060ALE611"));
        assert_eq!(identity.serial.as_deref(), Some("NCG8ARHS"));
        assert_eq!(identity.wwn.as_deref(), Some("5 000cca 24dc3cb3b"));
        assert_eq!(identity.firmware.as_deref(), Some("APGL0001"));
        assert_eq!(identity.rotation_rate.as_deref(), Some("7200 rpm"));
    }

    #[test]
    fn renders_identity_placeholders() {
        let identity = SmartctlIdentity {
            model: Some("HUS726060ALE611".to_string()),
            serial: Some("NCG8ARHS".to_string()),
            firmware: Some("APGL0001".to_string()),
            ..SmartctlIdentity::default()
        };

        assert_eq!(
            render_smartctl_label(
                "{device} {model} {serial} fw {firmware}",
                Path::new("/dev/sata4"),
                &identity
            ),
            "sata4 HUS726060ALE611 NCG8ARHS fw APGL0001"
        );
    }

    #[test]
    fn parses_scsi_current_drive_temperature() {
        assert_eq!(
            parse_smartctl_temperature_millicelsius("Current Drive Temperature:     36 C"),
            Some(36_000)
        );
    }

    #[test]
    fn parses_nvme_temperature() {
        assert_eq!(
            parse_smartctl_temperature_millicelsius(
                "Temperature:                        34 Celsius"
            ),
            Some(34_000)
        );
    }

    #[test]
    fn matches_device_globs() {
        assert!(wildcard_match("sata*", "sata4"));
        assert!(wildcard_match("sd?", "sda"));
        assert!(!wildcard_match("sd?", "sdaa"));
    }

    #[test]
    fn detects_common_partition_names() {
        assert!(looks_like_partition_name("sda1"));
        assert!(looks_like_partition_name("sata4p1"));
        assert!(looks_like_partition_name("nvme0n1p1"));
        assert!(!looks_like_partition_name("sata4"));
    }
}
