#[path = "cli.rs"]
mod cli;
#[path = "config.rs"]
mod config;
#[path = "config_edit.rs"]
mod config_edit;
#[path = "hba.rs"]
mod hba;
#[path = "sensors.rs"]
mod sensors;
#[path = "signals.rs"]
mod signals;
#[path = "smartctl.rs"]
mod smartctl;

use self::config::{load_config, DEFAULT_CONFIG};
use self::hba::probe_hba_temperatures;
use self::sensors::{descriptors, read_values, scan_all_sensors};
use self::signals::{install_signal_handlers, running};
use self::smartctl::probe_smartctl_temperatures;
use anyhow::Result;
use std::fs::{File, OpenOptions};
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use vsb_protocol::{read_frame, write_frame, Message};

pub fn run() -> Result<()> {
    install_signal_handlers();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first_arg) = args.first().map(String::as_str) else {
        return run_agent(std::path::Path::new(DEFAULT_CONFIG));
    };

    if cli::is_help_arg(first_arg) {
        cli::print_usage();
        return Ok(());
    }

    match first_arg {
        "run" => {
            let config_path = cli::parse_run_config(&args[1..])?;
            run_agent(&config_path)
        }
        "config" => cli::run_config(&args[1..]),
        "--probe-hba" => {
            let config_path = args.get(1).map(String::as_str).unwrap_or(DEFAULT_CONFIG);
            let config = load_config(std::path::Path::new(config_path))?;
            probe_hba_temperatures(&config.lsi_hba)
        }
        "--probe-smartctl" => {
            let config_path = args.get(1).map(String::as_str).unwrap_or(DEFAULT_CONFIG);
            let config = load_config(std::path::Path::new(config_path))?;
            probe_smartctl_temperatures(&config.smartctl)
        }
        config_path => run_agent(std::path::Path::new(config_path)),
    }
}

fn run_agent(config_path: &std::path::Path) -> Result<()> {
    let config = load_config(config_path)?;
    let mut port = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&config.agent.virtio_port)
        .map_err(anyhow::Error::from)?;

    let hostname = sensors::hostname();
    let hwmon_name = render_hwmon_name(
        config.agent.hwmon_name_template.as_deref(),
        &hostname,
        env!("CARGO_PKG_VERSION"),
    );

    write_frame(
        &mut port,
        &Message::Hello {
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: hostname.clone(),
            hwmon_name: hwmon_name.clone(),
        },
    )?;

    let mut generation = 1_u64;
    let mut sensors = scan_all_sensors(&config)?;
    send_schema_and_sample(&mut port, generation, &sensors)?;

    // Spawn a background thread to read messages from the host daemon (e.g.
    // RequestResync sent when hostd restarts with only the persistent cache).
    let (resync_tx, resync_rx) = mpsc::channel::<()>();
    {
        let mut port_reader = port.try_clone()?;
        let tx = resync_tx;
        thread::spawn(move || {
            loop {
                match read_frame(&mut port_reader) {
                    Ok(Message::RequestResync) => {
                        if tx.send(()).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {} // ignore unexpected host→agent messages
                    // InvalidData means a malformed frame; stop reading.
                    Err(ref e) if e.kind() == io::ErrorKind::InvalidData => break,
                    // Any other error (EIO/EOF when host disconnects) is transient.
                    // Sleep briefly and retry so we are ready when host reconnects.
                    Err(_) => thread::sleep(Duration::from_millis(500)),
                }
            }
        });
    };

    let mut last_sample = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut last_rescan = Instant::now();

    while running() {
        let now = Instant::now();

        // Handle resync request from hostd (e.g. after hostd restarts).
        // Drain all queued requests and respond with a single resend.
        if resync_rx.try_recv().is_ok() {
            while resync_rx.try_recv().is_ok() {}
            write_frame(
                &mut port,
                &Message::Hello {
                    agent_version: env!("CARGO_PKG_VERSION").to_string(),
                    hostname: hostname.clone(),
                    hwmon_name: hwmon_name.clone(),
                },
            )?;
            send_schema_and_sample(&mut port, generation, &sensors)?;
        }

        if now.duration_since(last_sample)
            >= Duration::from_secs(config.agent.sample_seconds.max(1))
        {
            write_frame(
                &mut port,
                &Message::Sample {
                    generation,
                    values: read_values(&sensors),
                },
            )?;
            last_sample = now;
        }

        if now.duration_since(last_heartbeat)
            >= Duration::from_secs(config.agent.heartbeat_seconds.max(1))
        {
            write_frame(&mut port, &Message::Heartbeat)?;
            last_heartbeat = now;
        }

        if now.duration_since(last_rescan)
            >= Duration::from_secs(config.agent.rescan_seconds.max(1))
        {
            let next = scan_all_sensors(&config)?;
            let next_descriptors = descriptors(&next);
            if next_descriptors != descriptors(&sensors) {
                generation += 1;
                sensors = next;
                send_schema_and_sample(&mut port, generation, &sensors)?;
            } else {
                sensors = next;
            }
            last_rescan = now;
        }

        thread::sleep(Duration::from_millis(200));
    }

    write_frame(&mut port, &Message::Goodbye)?;
    Ok(())
}

fn render_hwmon_name(
    template: Option<&str>,
    hostname: &str,
    agent_version: &str,
) -> Option<String> {
    let template = template?;
    let rendered = template
        .replace("{hostname}", hostname)
        .replace("{agent_version}", agent_version);
    let mut sanitized = String::with_capacity(rendered.len().min(31));

    for ch in rendered.chars() {
        if sanitized.len() >= 31 {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }

    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

fn send_schema_and_sample(
    port: &mut File,
    generation: u64,
    sensors: &[sensors::TrackedSensor],
) -> Result<()> {
    write_frame(
        port,
        &Message::Schema {
            generation,
            sensors: descriptors(sensors),
        },
    )?;
    write_frame(
        port,
        &Message::Sample {
            generation,
            values: read_values(sensors),
        },
    )?;
    Ok(())
}
