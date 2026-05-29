use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::process::{Child, Command};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const MAX_HOOK_DELAY_SECONDS: u64 = 300;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub enabled: bool,
    pub debounce_seconds: u64,
    pub timeout_seconds: u64,
    pub rule: Vec<HookRule>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_seconds: 5,
            timeout_seconds: 30,
            rule: vec![HookRule {
                events: vec![
                    "device_created".to_string(),
                    "device_removed".to_string(),
                    "sensor_added".to_string(),
                    "sensor_removed".to_string(),
                    "sensor_changed".to_string(),
                    "vm_offline".to_string(),
                ],
                command: vec![
                    "/usr/bin/systemctl".to_string(),
                    "restart".to_string(),
                    "fancontrol.service".to_string(),
                ],
            }],
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct HookRule {
    pub events: Vec<String>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug)]
struct TopologyEvent {
    events: BTreeSet<String>,
    vmid: u32,
    hwmon_name: String,
    hwmon_path: String,
    sensor_count: usize,
}

#[derive(Clone)]
pub struct HookRunner {
    enabled: bool,
    tx: Sender<TopologyEvent>,
}

impl HookRunner {
    pub fn start(config: HooksConfig) -> Self {
        let enabled = config.enabled;
        let (tx, rx) = mpsc::channel::<TopologyEvent>();

        thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut pending = HashMap::new();
                pending.insert(first.vmid, first);
                let debounce = bounded_duration(config.debounce_seconds);
                let deadline = Instant::now() + debounce;

                loop {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }

                    match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                        Ok(next) => merge_event(&mut pending, next),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }

                for event in pending.values() {
                    run_matching_hooks(&config, event);
                }
            }
        });

        Self { enabled, tx }
    }

    pub fn notify<I>(
        &self,
        vmid: u32,
        hwmon_name: String,
        hwmon_path: String,
        sensor_count: usize,
        events: I,
    ) where
        I: IntoIterator<Item = String>,
    {
        if !self.enabled {
            return;
        }

        let event = TopologyEvent {
            events: events.into_iter().collect(),
            vmid,
            hwmon_name,
            hwmon_path,
            sensor_count,
        };
        let _ = self.tx.send(event);
    }
}

fn merge_event(pending: &mut HashMap<u32, TopologyEvent>, next: TopologyEvent) {
    pending
        .entry(next.vmid)
        .and_modify(|event| {
            event.events.extend(next.events.clone());
            event.hwmon_name = next.hwmon_name.clone();
            event.hwmon_path = next.hwmon_path.clone();
            event.sensor_count = next.sensor_count;
        })
        .or_insert(next);
}

fn run_matching_hooks(config: &HooksConfig, event: &TopologyEvent) {
    for rule in &config.rule {
        if rule.command.is_empty() || !rule.events.iter().any(|item| event.events.contains(item)) {
            continue;
        }

        // Require absolute path for the hook executable.  A relative path
        // would resolve against the daemon's working directory (or $PATH),
        // which an attacker who can write to the config file could exploit to
        // run an unintended binary.
        let exe = &rule.command[0];
        if !exe.starts_with('/') {
            eprintln!(
                "hook {:?} rejected: executable path must be absolute",
                rule.command
            );
            continue;
        }

        if let Err(err) = run_hook(rule, config.timeout_seconds, event) {
            eprintln!("hook {:?} failed: {err:#}", rule.command);
        }
    }
}

fn run_hook(rule: &HookRule, timeout_seconds: u64, event: &TopologyEvent) -> Result<()> {
    let mut child = Command::new(&rule.command[0])
        .args(&rule.command[1..])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env(
            "VSB_EVENTS",
            event.events.iter().cloned().collect::<Vec<_>>().join(","),
        )
        .env("VSB_VMID", event.vmid.to_string())
        .env("VSB_HWMON_NAME", &event.hwmon_name)
        .env("VSB_HWMON_PATH", &event.hwmon_path)
        .env("VSB_SENSOR_COUNT", event.sensor_count.to_string())
        .spawn()
        .with_context(|| format!("spawn {:?}", rule.command))?;

    wait_with_timeout(&mut child, bounded_duration(timeout_seconds))
}

fn bounded_duration(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, MAX_HOOK_DELAY_SECONDS))
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("process exited with {status}");
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("process timed out");
        }

        thread::sleep(Duration::from_millis(100));
    }
}
