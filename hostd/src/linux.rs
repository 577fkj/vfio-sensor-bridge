#[path = "config.rs"]
mod config;
#[path = "hooks.rs"]
mod hooks;
#[path = "kernel.rs"]
mod kernel;
#[path = "runtime.rs"]
mod runtime;

use self::config::{load_config, HostConfig, DEFAULT_CONFIG};
use self::hooks::HookRunner;
use self::kernel::{
    fixed_hwmon_name, fixed_label, kernel_kind, ChannelCounters, KernelDevice, VsbSchema,
    VsbSensorValue, VsbValues, VSB_MAX_SENSORS,
};
use self::runtime::{topology_events, PersistentRuntimeSensor, VmRuntime};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use vsb_protocol::{
    read_ctl_request, read_frame, write_ctl_response, write_frame, CtlRequest, CtlResponse,
    Message, SensorDescriptor, SensorInfo, VmInfo, MAX_SENSORS,
};

/// Per-VM schema rate-limit window state, kept separately from VmRuntime so
/// it survives VM removal (e.g. after Goodbye) and prevents a malicious guest
/// from bypassing the limit by cycling connect → Schema → Goodbye.
#[derive(Clone, Debug)]
struct SchemaRateWindow {
    count: u32,
    start: Instant,
}

const PERSISTENT_CACHE_FILE: &str = "persistent-vms.json";
/// Maximum number of schema (topology) updates accepted from one VM per minute.
/// Schema is only sent when the sensor list changes; legitimate agents send it
/// at most a handful of times (initial connect + sensor add/remove events).
/// This limit prevents a malicious guest from triggering hwmon re-registration
/// storms and flooding the hook executor, while allowing generous burst on
/// reconnect.  Temperature values flow through Message::Sample and are never
/// rate-limited.
const MAX_SCHEMA_UPDATES_PER_MINUTE: u32 = 10;
const SCHEMA_RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistentCache {
    vms: Vec<PersistentCacheVm>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistentCacheVm {
    vmid: u32,
    generation: u64,
    hwmon_name: String,
    sensors: Vec<PersistentRuntimeSensor>,
}

/// All long-lived shared state passed between the socket-scanning and
/// per-connection handlers.  Every field is cheaply cloneable (Arc or
/// value-level Clone) so threads can take ownership without extra Arc wrapping.
#[derive(Clone)]
struct SharedState {
    persistent_cache_path: PathBuf,
    state: Arc<Mutex<HashMap<u32, VmRuntime>>>,
    hwmon_names: Arc<Mutex<HashMap<u32, String>>>,
    ioctl: Arc<Mutex<KernelDevice>>,
    schema_rate: Arc<Mutex<HashMap<u32, SchemaRateWindow>>>,
    hooks: HookRunner,
}

pub fn run() -> Result<()> {
    let config_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_CONFIG.to_string()),
    );
    let config = load_config(&config_path)?;
    let persistent_cache_path = persistent_cache_path(&config_path);

    fs::create_dir_all(&config.daemon.run_dir)
        .with_context(|| format!("create {}", config.daemon.run_dir.display()))?;

    let active = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));
    let shared = SharedState {
        persistent_cache_path,
        state: Arc::new(Mutex::new(HashMap::<u32, VmRuntime>::new())),
        hwmon_names: Arc::new(Mutex::new(HashMap::<u32, String>::new())),
        ioctl: Arc::new(Mutex::new(KernelDevice::open(&config.daemon.device)?)),
        schema_rate: Arc::new(Mutex::new(HashMap::<u32, SchemaRateWindow>::new())),
        hooks: HookRunner::start(config.hooks.clone()),
    };
    let scan_period = Duration::from_secs(1);

    eprintln!(
        "hostd running: run_dir={} device={} channel={} log_level={}",
        config.daemon.run_dir.display(),
        config.daemon.device.display(),
        config.virtio.channel_name,
        config.daemon.log_level
    );

    restore_persistent_cache(
        &shared.persistent_cache_path,
        &shared.state,
        &shared.hwmon_names,
        &shared.ioctl,
        &shared.hooks,
    )?;

    // Management socket for vsbctl (read-only status queries).
    {
        let ctl_path = config.daemon.run_dir.join("hostd.sock");
        let shared_ctl = shared.clone();
        let timeout_secs = config.heartbeat.timeout_seconds;
        thread::spawn(move || {
            if let Err(err) = run_ctl_listener(&ctl_path, shared_ctl, timeout_secs) {
                eprintln!("management socket error: {err:#}");
            }
        });
    }

    loop {
        scan_sockets(&config, Arc::clone(&active), shared.clone())?;
        check_heartbeats(
            &config,
            &shared.persistent_cache_path,
            &shared.state,
            &shared.hwmon_names,
            &shared.ioctl,
            &shared.hooks,
        );
        thread::sleep(scan_period);
    }
}

fn scan_sockets(
    config: &HostConfig,
    active: Arc<Mutex<HashSet<PathBuf>>>,
    shared: SharedState,
) -> Result<()> {
    // Socket read timeout: allow at least 2x the heartbeat interval so a
    // healthy agent always has time to send the next heartbeat before the
    // connection is dropped.  Floor at 60 s to guard against very short
    // heartbeat configs.
    let socket_read_timeout = Duration::from_secs(
        (config.heartbeat.timeout_seconds * 2).max(60),
    );
    let entries = match fs::read_dir(&config.daemon.run_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("scan {}", config.daemon.run_dir.display()))
        }
    };

    for entry in entries {
        let path = entry?.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("skip {}: {err}", path.display());
                continue;
            }
        };
        if !metadata.file_type().is_socket() {
            continue;
        }

        let Some(vmid) = parse_vmid_from_socket(&path, &config.virtio.socket_template) else {
            continue;
        };

        let mut active_guard = active.lock().expect("active socket lock poisoned");
        if !active_guard.insert(path.clone()) {
            continue;
        }
        drop(active_guard);

        let active = Arc::clone(&active);
        let shared = shared.clone();

        thread::spawn(move || {
            if let Err(err) = handle_socket(vmid, &path, socket_read_timeout, shared) {
                eprintln!("vm {vmid} socket {} ended: {err:#}", path.display());
            }
            active
                .lock()
                .expect("active socket lock poisoned")
                .remove(&path);
        });
    }

    Ok(())
}

fn parse_vmid_from_socket(path: &Path, socket_template: &str) -> Option<u32> {
    if let Some((prefix, suffix)) = socket_template.split_once("{vmid}") {
        let path = path.to_string_lossy();
        let id = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
        return id.parse().ok();
    }

    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix("vm-")?.strip_suffix(".sock")?;
    id.parse().ok()
}

fn handle_socket(
    vmid: u32,
    path: &Path,
    socket_read_timeout: Duration,
    shared: SharedState,
) -> Result<()> {
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connect {}", path.display()))?;

    // Apply a read timeout so a guest that sends a partial frame cannot hold
    // this thread blocked indefinitely, eventually exhausting thread resources.
    stream
        .set_read_timeout(Some(socket_read_timeout))
        .with_context(|| format!("set_read_timeout {}", path.display()))?;

    eprintln!("vm {vmid} connected from {}", path.display());

    // If this VM's runtime was restored from the persistent cache (schema_synced = false)
    // the live agent has not yet sent its full schema to this hostd instance.  Request it
    // to resend so non-persistent sensors are registered in the kernel hwmon device.
    {
        let needs_resync = shared
            .state
            .lock()
            .expect("vm state lock poisoned")
            .get(&vmid)
            .map(|vm| !vm.schema_synced)
            .unwrap_or(true); // vmid not in state → definitely need schema
        if needs_resync {
            eprintln!("vm {vmid} requesting schema resync");
            let _ = write_frame(&mut stream, &Message::RequestResync);
        }
    }
    // Track when we last sent RequestResync so we can retry periodically if the
    // agent's reader thread was dead during the initial send (e.g. host briefly
    // disconnected and the thread exited on EIO, then recovered).
    let mut last_resync_sent = Instant::now();

    loop {
        let message = match read_frame(&mut stream) {
            Ok(message) => message,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::TimedOut
                || err.kind() == io::ErrorKind::WouldBlock =>
            {
                return Err(err).context("socket read timeout: guest may be stalled or malicious");
            }
            Err(err) => return Err(err).context("decode frame"),
        };

        mark_seen(vmid, &shared.state);

        // Re-send RequestResync every 5 s while schema_synced is still false.
        // The initial send on connect may have been missed if the agent's
        // background reader thread had exited due to a previous host disconnect.
        if last_resync_sent.elapsed() >= Duration::from_secs(5) {
            let still_needs = shared
                .state
                .lock()
                .expect("vm state lock poisoned")
                .get(&vmid)
                .map(|vm| !vm.schema_synced)
                .unwrap_or(true); // vmid not yet registered → still need schema
            if still_needs {
                eprintln!("vm {vmid} requesting schema resync (retry)");
                let _ = write_frame(&mut stream, &Message::RequestResync);
            }
            last_resync_sent = Instant::now();
        }

        handle_message(vmid, message, &shared)?;
    }
}

fn mark_seen(vmid: u32, state: &Arc<Mutex<HashMap<u32, VmRuntime>>>) {
    let mut guard = state.lock().expect("vm state lock poisoned");
    if let Some(vm) = guard.get_mut(&vmid) {
        vm.last_seen = Instant::now();
        vm.offline_reported = false;
    }
}

fn handle_message(vmid: u32, message: Message, shared: &SharedState) -> Result<()> {
    match message {
        Message::Hello {
            agent_version,
            hostname,
            hwmon_name,
        } => {
            let mut names = shared.hwmon_names.lock().expect("hwmon name lock poisoned");
            if let Some(hwmon_name) = hwmon_name {
                names.insert(vmid, hwmon_name);
            } else {
                names.remove(&vmid);
            }
            eprintln!("vm {vmid} hello hostname={hostname} agent={agent_version}");
            Ok(())
        }
        Message::Schema {
            generation,
            sensors,
        } => apply_schema(vmid, generation, sensors, shared),
        Message::Sample { generation, values } => {
            let mapping = {
                let guard = shared.state.lock().expect("vm state lock poisoned");
                let Some(vm) = guard.get(&vmid) else {
                    // Schema not yet received; silently drop this sample.
                    // The handle_socket retry loop will re-request the schema.
                    return Ok(());
                };
                if vm.generation != generation {
                    // Generation mismatch; silently drop.  The retry loop will
                    // re-request the schema once schema_synced resets to false
                    // (handled by the reconnect path when the connection closes).
                    return Ok(());
                }
                vm.sensor_ids.clone()
            };

            let mut payload = VsbValues {
                vmid,
                value_count: 0,
                ..VsbValues::default()
            };
            if values.len() > VSB_MAX_SENSORS {
                anyhow::bail!("sample has too many values");
            }

            for value in values {
                if let Some(id) = mapping.get(&value.id) {
                    let idx = payload.value_count as usize;
                    payload.values[idx] = VsbSensorValue {
                        id: *id,
                        reserved: 0,
                        value: value.value,
                    };
                    payload.value_count += 1;
                }
            }

            shared
                .ioctl
                .lock()
                .expect("kernel ioctl lock poisoned")
                .set_values(&payload)
                .context("VSB_IOCTL_SET_VALUES")
        }
        Message::Heartbeat => Ok(()),
        // RequestResync is a host→agent message; ignore if somehow echoed back.
        Message::RequestResync => Ok(()),
        Message::Goodbye => remove_vm(
            vmid,
            &shared.persistent_cache_path,
            &shared.state,
            &shared.hwmon_names,
            &shared.ioctl,
            &shared.hooks,
            "device_removed",
        ),
    }
}

fn apply_schema(
    vmid: u32,
    generation: u64,
    sensors: Vec<SensorDescriptor>,
    shared: &SharedState,
) -> Result<()> {
    let configured_hwmon_name = shared
        .hwmon_names
        .lock()
        .expect("hwmon name lock poisoned")
        .get(&vmid)
        .cloned();
    let hwmon_name = configured_hwmon_name
        .clone()
        .unwrap_or_else(|| format!("vsb_vm_{vmid}"));

    let mut schema = VsbSchema {
        vmid,
        sensor_count: sensors.len() as u32,
        hwmon_name: fixed_hwmon_name(configured_hwmon_name.as_deref()),
        ..VsbSchema::default()
    };
    let mut sensor_ids = HashMap::new();
    let mut signatures = HashMap::new();
    let mut sensor_attrs = HashMap::new();
    let mut persistent_sensors = Vec::new();
    let mut channels = ChannelCounters::default();

    if sensors.len() > MAX_SENSORS {
        anyhow::bail!("schema has too many sensors");
    }

    for (idx, sensor) in sensors.iter().enumerate() {
        let numeric_id = (idx + 1) as u32;
        let channel = channels.next(sensor.kind);
        schema.sensors[idx] = kernel::VsbSensorDesc {
            id: numeric_id,
            kind: kernel_kind(sensor.kind),
            channel,
            reserved: 0,
            label: fixed_label(&sensor.label),
        };
        let attr = format!("{}{}_input", kind_prefix_str(sensor.kind), channel);
        sensor_ids.insert(sensor.id.clone(), numeric_id);
        signatures.insert(sensor.id.clone(), (sensor.kind, sensor.label.clone()));
        sensor_attrs.insert(sensor.id.clone(), attr);
        if sensor.persistent {
            let default_value = sensor
                .default_value
                .context("persistent sensor missing default_value")?;
            persistent_sensors.push(PersistentRuntimeSensor {
                descriptor: sensor.clone(),
                default_value,
            });
        }
    }

    let (events, changed) = {
        let guard = shared.state.lock().expect("vm state lock poisoned");

        // --- Rate-limit schema topology updates from this VM guest ---
        // Schema messages trigger kernel hwmon re-registration and hook
        // execution.  Temperature values use Message::Sample and are never
        // limited here.  Allow at most MAX_SCHEMA_UPDATES_PER_MINUTE updates
        // in any 60-second sliding window; excess are silently dropped.
        //
        // The window is tracked in a separate schema_rate map rather than
        // inside VmRuntime so the limit persists across VM removals.  A
        // malicious guest cannot bypass it by cycling Goodbye → reconnect.
        let now = Instant::now();
        {
            let mut rate = shared.schema_rate.lock().expect("schema rate lock poisoned");
            let window = rate.entry(vmid).or_insert(SchemaRateWindow {
                count: 0,
                start: now,
            });
            if now.duration_since(window.start) >= SCHEMA_RATE_WINDOW {
                window.start = now;
                window.count = 0;
            }
            window.count += 1;
            if window.count > MAX_SCHEMA_UPDATES_PER_MINUTE {
                eprintln!(
                    "vm {vmid} schema rate limit exceeded ({} schema/min); dropping",
                    window.count
                );
                return Ok(());
            }
        }

        let events = topology_events(guard.get(&vmid), &signatures);
        let changed = guard
            .get(&vmid)
            .map(|prior| {
                prior.signatures != signatures
                    || prior.sensor_ids != sensor_ids
                    || prior.hwmon_name != hwmon_name
                    || prior.persistent_sensors != persistent_sensors
            })
            .unwrap_or(true);
        (events, changed)
    };

    if !changed {
        {
            let mut guard = shared.state.lock().expect("vm state lock poisoned");
            if let Some(prior) = guard.get_mut(&vmid) {
                prior.generation = generation;
                prior.sensor_ids = sensor_ids;
                prior.signatures = signatures;
                prior.sensor_attrs = sensor_attrs;
                prior.persistent_sensors = persistent_sensors;
                prior.last_seen = Instant::now();
                prior.offline_reported = false;
                prior.schema_synced = true;
                prior.sensor_count = schema.sensor_count as usize;
            }
        } // release state lock before calling save_persistent_cache
        save_persistent_cache(&shared.persistent_cache_path, &shared.state)?;
        return Ok(());
    }

    shared
        .ioctl
        .lock()
        .expect("kernel ioctl lock poisoned")
        .set_schema(&schema)
        .context("VSB_IOCTL_SET_SCHEMA")?;

    let hwmon_path = find_hwmon_path(&hwmon_name).unwrap_or_default();
    {
        let mut guard = shared.state.lock().expect("vm state lock poisoned");
        guard.insert(
            vmid,
            VmRuntime {
                generation,
                sensor_ids,
                signatures,
                sensor_attrs,
                persistent_sensors,
                last_seen: Instant::now(),
                offline_reported: false,
                schema_synced: true,
                hwmon_name: hwmon_name.clone(),
                hwmon_path,
                sensor_count: schema.sensor_count as usize,
            },
        );
    }
    save_persistent_cache(&shared.persistent_cache_path, &shared.state)?;

    notify_schema_events(
        &shared.hooks,
        vmid,
        hwmon_name,
        schema.sensor_count as usize,
        events,
    )
}

fn notify_schema_events(
    hooks: &HookRunner,
    vmid: u32,
    hwmon_name: String,
    sensor_count: usize,
    events: Vec<String>,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }

    let hwmon_path = find_hwmon_path(&hwmon_name).unwrap_or_default();
    hooks.notify(vmid, hwmon_name, hwmon_path, sensor_count, events);
    Ok(())
}

fn check_heartbeats(
    config: &HostConfig,
    persistent_cache_path: &Path,
    state: &Arc<Mutex<HashMap<u32, VmRuntime>>>,
    hwmon_names: &Arc<Mutex<HashMap<u32, String>>>,
    ioctl: &Arc<Mutex<KernelDevice>>,
    hooks: &HookRunner,
) {
    let timeout = Duration::from_secs(config.heartbeat.timeout_seconds);
    let mut removals = Vec::new();
    let mut preserves = Vec::new();

    {
        let mut guard = state.lock().expect("vm state lock poisoned");
        let now = Instant::now();
        let vmids: Vec<u32> = guard.keys().copied().collect();
        let mut remove_vmids = Vec::new();

        for vmid in vmids {
            let Some(vm) = guard.get_mut(&vmid) else {
                continue;
            };
            if vm.offline_reported || now.duration_since(vm.last_seen) < timeout {
                continue;
            }

            match config.heartbeat.policy.as_str() {
                "warn_only" => {
                    eprintln!("vm {vmid} heartbeat timeout");
                    vm.offline_reported = true;
                }
                "remove_only" | "warn_then_remove" => {
                    if config.heartbeat.policy == "warn_then_remove" {
                        eprintln!("vm {vmid} heartbeat timeout");
                    }
                    if vm.persistent_sensors.is_empty() {
                        remove_vmids.push(vmid);
                    } else {
                        vm.offline_reported = true;
                        preserves.push((
                            vmid,
                            vm.generation,
                            vm.hwmon_name.clone(),
                            vm.persistent_sensors.clone(),
                            "vm_offline".to_string(),
                        ));
                    }
                }
                other => {
                    eprintln!("unknown heartbeat policy {other}; using warn_then_remove");
                    if vm.persistent_sensors.is_empty() {
                        remove_vmids.push(vmid);
                    } else {
                        vm.offline_reported = true;
                        preserves.push((
                            vmid,
                            vm.generation,
                            vm.hwmon_name.clone(),
                            vm.persistent_sensors.clone(),
                            "vm_offline".to_string(),
                        ));
                    }
                }
            }
        }

        for vmid in remove_vmids {
            let removed = guard.remove(&vmid).expect("vm exists");
            removals.push((
                vmid,
                removed.hwmon_name,
                removed.hwmon_path,
                removed.sensor_count,
            ));
        }
    }

    for (vmid, generation, hwmon_name, persistent_sensors, event) in preserves {
        if let Err(err) = preserve_persistent_vm(
            vmid,
            generation,
            hwmon_name,
            persistent_sensors,
            state,
            ioctl,
            hooks,
            persistent_cache_path,
            &event,
        ) {
            eprintln!("vm {vmid} persistent offline update failed: {err:#}");
        }
    }

    let removed_any = !removals.is_empty();
    for (vmid, hwmon_name, hwmon_path, sensor_count) in removals {
        hwmon_names
            .lock()
            .expect("hwmon name lock poisoned")
            .remove(&vmid);
        if let Err(err) = ioctl
            .lock()
            .expect("kernel ioctl lock poisoned")
            .remove_vm(vmid)
        {
            eprintln!("vm {vmid} remove after heartbeat timeout failed: {err:#}");
        }
        hooks.notify(
            vmid,
            hwmon_name,
            hwmon_path,
            sensor_count,
            ["vm_offline".to_string()],
        );
    }

    if removed_any {
        if let Err(err) = save_persistent_cache(persistent_cache_path, state) {
            eprintln!("persistent cache save failed: {err:#}");
        }
    }
}

fn remove_vm(
    vmid: u32,
    persistent_cache_path: &Path,
    state: &Arc<Mutex<HashMap<u32, VmRuntime>>>,
    hwmon_names: &Arc<Mutex<HashMap<u32, String>>>,
    ioctl: &Arc<Mutex<KernelDevice>>,
    hooks: &HookRunner,
    event: &str,
) -> Result<()> {
    let preserve = {
        let mut guard = state.lock().expect("vm state lock poisoned");
        match guard.get_mut(&vmid) {
            Some(vm) if !vm.persistent_sensors.is_empty() => {
                vm.offline_reported = true;
                Some((
                    vm.generation,
                    vm.hwmon_name.clone(),
                    vm.persistent_sensors.clone(),
                ))
            }
            _ => None,
        }
    };

    if let Some((generation, hwmon_name, persistent_sensors)) = preserve {
        return preserve_persistent_vm(
            vmid,
            generation,
            hwmon_name,
            persistent_sensors,
            state,
            ioctl,
            hooks,
            persistent_cache_path,
            event,
        );
    }

    let removed = state.lock().expect("vm state lock poisoned").remove(&vmid);
    hwmon_names
        .lock()
        .expect("hwmon name lock poisoned")
        .remove(&vmid);
    ioctl
        .lock()
        .expect("kernel ioctl lock poisoned")
        .remove_vm(vmid)
        .context("VSB_IOCTL_REMOVE_VM")?;
    let hwmon_name = removed
        .as_ref()
        .map(|vm| vm.hwmon_name.clone())
        .unwrap_or_else(|| format!("vsb_vm_{vmid}"));
    let hwmon_path = removed
        .as_ref()
        .map(|vm| vm.hwmon_path.clone())
        .unwrap_or_default();
    hooks.notify(vmid, hwmon_name, hwmon_path, 0, [event.to_string()]);
    save_persistent_cache(persistent_cache_path, state)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn preserve_persistent_vm(
    vmid: u32,
    generation: u64,
    hwmon_name: String,
    persistent_sensors: Vec<PersistentRuntimeSensor>,
    state: &Arc<Mutex<HashMap<u32, VmRuntime>>>,
    ioctl: &Arc<Mutex<KernelDevice>>,
    hooks: &HookRunner,
    persistent_cache_path: &Path,
    event: &str,
) -> Result<()> {
    install_persistent_vm_defaults(
        vmid,
        generation,
        hwmon_name,
        persistent_sensors,
        state,
        ioctl,
        Some((hooks, event)),
    )?;
    save_persistent_cache(persistent_cache_path, state)
}

fn install_persistent_vm_defaults(
    vmid: u32,
    generation: u64,
    hwmon_name: String,
    persistent_sensors: Vec<PersistentRuntimeSensor>,
    state: &Arc<Mutex<HashMap<u32, VmRuntime>>>,
    ioctl: &Arc<Mutex<KernelDevice>>,
    hook_event: Option<(&HookRunner, &str)>,
) -> Result<()> {
    let sensor_count = persistent_sensors.len();
    let mut schema = VsbSchema {
        vmid,
        sensor_count: sensor_count as u32,
        hwmon_name: fixed_hwmon_name(Some(&hwmon_name)),
        ..VsbSchema::default()
    };
    let mut values = VsbValues {
        vmid,
        value_count: sensor_count as u32,
        ..VsbValues::default()
    };
    let mut sensor_ids = HashMap::new();
    let mut signatures = HashMap::new();
    let mut sensor_attrs = HashMap::new();
    let mut channels = ChannelCounters::default();

    for (idx, persistent) in persistent_sensors.iter().enumerate() {
        let numeric_id = (idx + 1) as u32;
        let channel = channels.next(persistent.descriptor.kind);
        schema.sensors[idx] = kernel::VsbSensorDesc {
            id: numeric_id,
            kind: kernel_kind(persistent.descriptor.kind),
            channel,
            reserved: 0,
            label: fixed_label(&persistent.descriptor.label),
        };
        let attr = format!(
            "{}{}_input",
            kind_prefix_str(persistent.descriptor.kind),
            channel
        );
        values.values[idx] = VsbSensorValue {
            id: numeric_id,
            reserved: 0,
            value: persistent.default_value,
        };
        sensor_ids.insert(persistent.descriptor.id.clone(), numeric_id);
        signatures.insert(
            persistent.descriptor.id.clone(),
            (
                persistent.descriptor.kind,
                persistent.descriptor.label.clone(),
            ),
        );
        sensor_attrs.insert(persistent.descriptor.id.clone(), attr);
    }

    {
        let device = ioctl.lock().expect("kernel ioctl lock poisoned");
        device.set_schema(&schema).context("VSB_IOCTL_SET_SCHEMA")?;
        device.set_values(&values).context("VSB_IOCTL_SET_VALUES")?;
    }

    let hwmon_path = find_hwmon_path(&hwmon_name).unwrap_or_default();
    {
        let mut guard = state.lock().expect("vm state lock poisoned");
        guard.insert(
            vmid,
            VmRuntime {
                generation,
                sensor_ids,
                signatures,
                sensor_attrs,
                persistent_sensors,
                last_seen: Instant::now(),
                offline_reported: true,
                schema_synced: false,
                hwmon_name: hwmon_name.clone(),
                hwmon_path: hwmon_path.clone(),
                sensor_count,
            },
        );
    }

    if let Some((hooks, event)) = hook_event {
        hooks.notify(
            vmid,
            hwmon_name,
            hwmon_path,
            sensor_count,
            [event.to_string()],
        );
    }
    Ok(())
}

fn restore_persistent_cache(
    path: &Path,
    state: &Arc<Mutex<HashMap<u32, VmRuntime>>>,
    hwmon_names: &Arc<Mutex<HashMap<u32, String>>>,
    ioctl: &Arc<Mutex<KernelDevice>>,
    hooks: &HookRunner,
) -> Result<()> {
    let cache = read_persistent_cache(path)?;
    for vm in cache.vms {
        if vm.vmid == 0 || vm.sensors.is_empty() || vm.sensors.len() > MAX_SENSORS {
            eprintln!("skip invalid persistent cache entry for vm {}", vm.vmid);
            continue;
        }

        hwmon_names
            .lock()
            .expect("hwmon name lock poisoned")
            .insert(vm.vmid, vm.hwmon_name.clone());

        if let Err(err) = install_persistent_vm_defaults(
            vm.vmid,
            vm.generation,
            vm.hwmon_name,
            vm.sensors,
            state,
            ioctl,
            Some((hooks, "device_created")),
        ) {
            eprintln!("restore persistent vm {} failed: {err:#}", vm.vmid);
        }
    }
    Ok(())
}

fn read_persistent_cache(path: &Path) -> Result<PersistentCache> {
    match fs::read(path) {
        Ok(data) => {
            serde_json::from_slice(&data).with_context(|| format!("parse {}", path.display()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(PersistentCache::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn save_persistent_cache(path: &Path, state: &Arc<Mutex<HashMap<u32, VmRuntime>>>) -> Result<()> {
    let mut vms = {
        let guard = state.lock().expect("vm state lock poisoned");
        guard
            .iter()
            .filter_map(|(vmid, vm)| {
                if vm.persistent_sensors.is_empty() {
                    return None;
                }
                Some(PersistentCacheVm {
                    vmid: *vmid,
                    generation: vm.generation,
                    hwmon_name: vm.hwmon_name.clone(),
                    sensors: vm.persistent_sensors.clone(),
                })
            })
            .collect::<Vec<_>>()
    };
    vms.sort_by_key(|vm| vm.vmid);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let data = serde_json::to_vec_pretty(&PersistentCache { vms })?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

fn persistent_cache_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(PERSISTENT_CACHE_FILE)
}

fn find_hwmon_path(hwmon_name: &str) -> Option<String> {
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        if name.trim() == hwmon_name {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

fn kind_prefix_str(kind: vsb_protocol::SensorKind) -> &'static str {
    match kind {
        vsb_protocol::SensorKind::Temperature => "temp",
        vsb_protocol::SensorKind::Fan => "fan",
        vsb_protocol::SensorKind::Voltage => "in",
        vsb_protocol::SensorKind::Current => "curr",
        vsb_protocol::SensorKind::Power => "power",
    }
}

// ── Management socket (vsbctl ↔ hostd) ───────────────────────────────────────

fn run_ctl_listener(
    socket_path: &Path,
    shared: SharedState,
    timeout_secs: u64,
) -> Result<()> {
    // Remove any stale socket from a previous run.
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind management socket {}", socket_path.display()))?;
    eprintln!("management socket listening on {}", socket_path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = shared.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_ctl_connection(stream, &shared, timeout_secs) {
                        eprintln!("ctl connection error: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("ctl accept error: {err}"),
        }
    }
    Ok(())
}

fn handle_ctl_connection(
    mut stream: UnixStream,
    shared: &SharedState,
    timeout_secs: u64,
) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set ctl read timeout")?;

    let request = read_ctl_request(&mut stream).context("read ctl request")?;

    let response = match request {
        CtlRequest::ListVms => {
            let vms = collect_vm_infos(shared, None, timeout_secs);
            CtlResponse::VmList { vms }
        }
        CtlRequest::GetVm { vmid } => {
            let mut vms = collect_vm_infos(shared, Some(vmid), timeout_secs);
            match vms.pop() {
                Some(info) => CtlResponse::VmInfo(info),
                None => CtlResponse::NotFound { vmid },
            }
        }
    };

    write_ctl_response(&mut stream, &response).context("write ctl response")
}

fn collect_vm_infos(
    shared: &SharedState,
    filter_vmid: Option<u32>,
    timeout_secs: u64,
) -> Vec<VmInfo> {
    let guard = shared.state.lock().expect("vm state lock poisoned");
    let now = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let mut infos = Vec::new();

    for (&vmid, vm) in &*guard {
        if filter_vmid.is_some_and(|fv| fv != vmid) {
            continue;
        }
        let elapsed = now.duration_since(vm.last_seen);
        let sensors = build_sensor_infos(vm);
        infos.push(VmInfo {
            vmid,
            hwmon_name: vm.hwmon_name.clone(),
            hwmon_path: vm.hwmon_path.clone(),
            generation: vm.generation,
            online: elapsed < timeout,
            offline_reported: vm.offline_reported,
            last_seen_secs_ago: elapsed.as_secs_f64(),
            sensors,
        });
    }

    infos.sort_by_key(|i| i.vmid);
    infos
}

fn build_sensor_infos(vm: &VmRuntime) -> Vec<SensorInfo> {
    let mut sensors: Vec<SensorInfo> = vm
        .signatures
        .iter()
        .map(|(id, (kind, label))| {
            let hwmon_attr = vm
                .sensor_attrs
                .get(id)
                .cloned()
                .unwrap_or_default();

            let value = if !hwmon_attr.is_empty() && !vm.hwmon_path.is_empty() {
                let path = Path::new(&vm.hwmon_path).join(&hwmon_attr);
                fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
            } else {
                None
            };

            let persistent_entry = vm
                .persistent_sensors
                .iter()
                .find(|p| p.descriptor.id == *id);

            SensorInfo {
                id: id.clone(),
                kind: *kind,
                label: label.clone(),
                persistent: persistent_entry.is_some(),
                default_value: persistent_entry.map(|p| p.default_value),
                hwmon_attr,
                value,
            }
        })
        .collect();

    sensors.sort_by(|a, b| a.hwmon_attr.cmp(&b.hwmon_attr));
    sensors
}
