use super::config::{load_config, DEFAULT_CONFIG};
use super::config_edit::{
    add_persistent_sensor, init_config, remove_persistent_sensor, set_agent, set_hba, set_smartctl,
    show_config, AgentSet, HbaSet, SmartctlSet,
};
use super::sensors::{discover_persistent_candidates, DiscoveredSensor};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use vsb_protocol::SensorKind;

const DISCOVER_CACHE: &str = "/run/vfio-sensor-bridge/agent-discover.json";

pub fn run_config(args: &[String]) -> Result<()> {
    let (config_path, args) = split_config_path(args)?;
    let Some(command) = args.first().map(String::as_str) else {
        print_config_usage();
        return Ok(());
    };

    if is_help_arg(command) {
        print_config_usage();
        return Ok(());
    }

    match command {
        "show" => {
            println!("{}", show_config(&config_path)?);
            Ok(())
        }
        "validate" => {
            load_config(&config_path)?;
            println!("valid {}", config_path.display());
            Ok(())
        }
        "init" => config_init(&config_path, &args[1..]),
        "set" => config_set(&config_path, &args[1..]),
        "hba" => config_hba(&config_path, &args[1..]),
        "smartctl" => config_smartctl(&config_path, &args[1..]),
        "persistent" => config_persistent(&config_path, &args[1..]),
        _ => {
            print_config_usage();
            anyhow::bail!("unknown config command: {command}")
        }
    }
}

pub fn parse_run_config(args: &[String]) -> Result<PathBuf> {
    let (config_path, args) = split_config_path(args)?;
    if args.iter().any(|arg| is_help_arg(arg)) {
        print_run_usage();
        std::process::exit(0);
    }
    if let Some(extra) = args.first() {
        anyhow::bail!("unexpected run argument: {extra}");
    }
    Ok(config_path)
}

pub fn is_help_arg(arg: &str) -> bool {
    arg == "-h" || arg == "--help"
}

pub fn print_usage() {
    println!(
        r#"vfio-sensor-bridge Linux guest agent

Usage:
  agent-linux [CONFIG]
  agent-linux run [--config PATH]
  agent-linux config <command> [--config PATH]
  agent-linux --probe-hba [CONFIG]
  agent-linux --probe-smartctl [CONFIG]

Commands:
  run                         Start the guest agent.
  config                      Show, validate, and edit agent config.

Compatibility:
  agent-linux /etc/vfio-sensor-bridge/agent.toml still starts the agent."#
    );
}

fn print_run_usage() {
    println!(
        r#"Usage:
  agent-linux run [--config PATH]

Default config:
  /etc/vfio-sensor-bridge/agent.toml"#
    );
}

fn print_config_usage() {
    println!(
        r#"Usage:
  agent-linux config show [--config PATH]
  agent-linux config validate [--config PATH]
  agent-linux config init [--config PATH] [--force]
  agent-linux config set [options] [--config PATH]
  agent-linux config hba [options] [--config PATH]
  agent-linux config smartctl [options] [--config PATH]
  agent-linux config persistent <command> [--config PATH]

Config set options:
  --scan-root PATH
  --sample-seconds N
  --heartbeat-seconds N
  --rescan-seconds N

Persistent commands:
  persistent list
  persistent discover
  persistent add --from N --default-value VALUE
  persistent remove --id ID"#
    );
}

fn split_config_path(args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let mut config_path = PathBuf::from(DEFAULT_CONFIG);
    let mut rest = Vec::new();
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    anyhow::bail!("--config requires PATH");
                };
                config_path = PathBuf::from(path);
            }
            other => rest.push(other.to_string()),
        }
        i += 1;
    }

    Ok((config_path, rest))
}

fn config_init(config_path: &Path, args: &[String]) -> Result<()> {
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown init option: {other}"),
        }
    }
    init_config(config_path, force)?;
    println!("wrote {}", config_path.display());
    Ok(())
}

fn config_set(config_path: &Path, args: &[String]) -> Result<()> {
    let mut set = AgentSet {
        scan_root: None,
        sample_seconds: None,
        heartbeat_seconds: None,
        rescan_seconds: None,
    };
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--scan-root" => {
                set.scan_root = Some(PathBuf::from(next_arg(args, &mut i, "--scan-root")?))
            }
            "--sample-seconds" => {
                set.sample_seconds = Some(parse_u64(next_arg(args, &mut i, "--sample-seconds")?)?)
            }
            "--heartbeat-seconds" => {
                set.heartbeat_seconds =
                    Some(parse_u64(next_arg(args, &mut i, "--heartbeat-seconds")?)?)
            }
            "--rescan-seconds" => {
                set.rescan_seconds = Some(parse_u64(next_arg(args, &mut i, "--rescan-seconds")?)?)
            }
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown set option: {other}"),
        }
        i += 1;
    }

    set_agent(config_path, set)?;
    println!("updated {}", config_path.display());
    Ok(())
}

fn config_hba(config_path: &Path, args: &[String]) -> Result<()> {
    let mut set = HbaSet {
        enabled: None,
        devices: None,
        max_ioc: None,
    };
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--enabled" => set.enabled = Some(parse_bool(next_arg(args, &mut i, "--enabled")?)?),
            "--devices" => {
                set.devices = Some(
                    split_csv(next_arg(args, &mut i, "--devices")?)
                        .into_iter()
                        .map(PathBuf::from)
                        .collect(),
                )
            }
            "--max-ioc" => set.max_ioc = Some(parse_u32(next_arg(args, &mut i, "--max-ioc")?)?),
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown hba option: {other}"),
        }
        i += 1;
    }
    set_hba(config_path, set)?;
    println!("updated {}", config_path.display());
    Ok(())
}

fn config_smartctl(config_path: &Path, args: &[String]) -> Result<()> {
    let mut set = SmartctlSet {
        enabled: None,
        device_globs: None,
        timeout_seconds: None,
        poll_seconds: None,
    };
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--enabled" => set.enabled = Some(parse_bool(next_arg(args, &mut i, "--enabled")?)?),
            "--device-globs" => {
                set.device_globs = Some(split_csv(next_arg(args, &mut i, "--device-globs")?))
            }
            "--timeout-seconds" => {
                set.timeout_seconds = Some(parse_u64(next_arg(args, &mut i, "--timeout-seconds")?)?)
            }
            "--poll-seconds" => {
                set.poll_seconds = Some(parse_u64(next_arg(args, &mut i, "--poll-seconds")?)?)
            }
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown smartctl option: {other}"),
        }
        i += 1;
    }
    set_smartctl(config_path, set)?;
    println!("updated {}", config_path.display());
    Ok(())
}

fn config_persistent(config_path: &Path, args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        print_config_usage();
        return Ok(());
    };

    match command {
        "list" => persistent_list(config_path),
        "discover" => persistent_discover(config_path),
        "add" => persistent_add(config_path, &args[1..]),
        "remove" => persistent_remove(config_path, &args[1..]),
        other if is_help_arg(other) => {
            print_config_usage();
            Ok(())
        }
        other => anyhow::bail!("unknown persistent command: {other}"),
    }
}

fn persistent_list(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    for sensor in &config.persistent_sensor {
        println!(
            "{}  {}  {}  default={}",
            sensor.id,
            kind_name(sensor.kind),
            sensor.label,
            sensor.default_value
        );
    }
    Ok(())
}

fn persistent_discover(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    let discovered = discover_persistent_candidates(&config)?;
    write_discover_cache(&discovered)?;
    for sensor in &discovered {
        println!(
            "{:<3} {:<11} {:<8} {}",
            sensor.index,
            kind_name(sensor.kind),
            sensor.value,
            sensor.summary
        );
    }
    println!("cached {}", DISCOVER_CACHE);
    Ok(())
}

fn persistent_add(config_path: &Path, args: &[String]) -> Result<()> {
    let mut from = None;
    let mut default_value = None;
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => from = Some(parse_usize(next_arg(args, &mut i, "--from")?)?),
            "--default-value" => {
                default_value = Some(parse_i64(next_arg(args, &mut i, "--default-value")?)?)
            }
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown persistent add option: {other}"),
        }
        i += 1;
    }

    let from = from.context("persistent add requires --from N")?;
    let default_value = default_value.context("persistent add requires --default-value VALUE")?;
    let discovered = read_discover_cache()?;
    let sensor = discovered
        .into_iter()
        .find(|sensor| sensor.index == from)
        .with_context(|| format!("discover cache has no sensor index {from}"))?;
    let id = sensor.id.clone();
    add_persistent_sensor(config_path, sensor.into_persistent_config(default_value))?;
    println!("added persistent sensor {id}");
    Ok(())
}

fn persistent_remove(config_path: &Path, args: &[String]) -> Result<()> {
    let mut id = None;
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => id = Some(next_arg(args, &mut i, "--id")?.to_string()),
            other if is_help_arg(other) => {
                print_config_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown persistent remove option: {other}"),
        }
        i += 1;
    }
    let id = id.context("persistent remove requires --id ID")?;
    let removed = remove_persistent_sensor(config_path, &id)?;
    if removed {
        println!("removed persistent sensor {id}");
    } else {
        println!("persistent sensor {id} was absent");
    }
    Ok(())
}

fn write_discover_cache(discovered: &[DiscoveredSensor]) -> Result<()> {
    let path = Path::new(DISCOVER_CACHE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let data = serde_json::to_vec_pretty(discovered)?;
    fs::write(path, data).with_context(|| format!("write {}", path.display()))
}

fn read_discover_cache() -> Result<Vec<DiscoveredSensor>> {
    let path = Path::new(DISCOVER_CACHE);
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("parse {}", path.display()))
}

fn next_arg<'a>(args: &'a [String], index: &mut usize, name: &str) -> Result<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .with_context(|| format!("{name} requires a value"))
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("invalid bool: {value}"),
    }
}

fn parse_u64(value: &str) -> Result<u64> {
    let value = value.parse::<u64>()?;
    if value == 0 {
        anyhow::bail!("value must be greater than zero");
    }
    Ok(value)
}

fn parse_u32(value: &str) -> Result<u32> {
    let value = value.parse::<u32>()?;
    if value == 0 {
        anyhow::bail!("value must be greater than zero");
    }
    Ok(value)
}

fn parse_usize(value: &str) -> Result<usize> {
    let value = value.parse::<usize>()?;
    if value == 0 {
        anyhow::bail!("value must be greater than zero");
    }
    Ok(value)
}

fn parse_i64(value: &str) -> Result<i64> {
    Ok(value.parse::<i64>()?)
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn kind_name(kind: SensorKind) -> &'static str {
    match kind {
        SensorKind::Temperature => "temperature",
        SensorKind::Fan => "fan",
        SensorKind::Voltage => "voltage",
        SensorKind::Current => "current",
        SensorKind::Power => "power",
    }
}
