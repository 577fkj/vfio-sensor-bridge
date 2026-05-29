use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use vsb_protocol::{SensorDescriptor, SensorKind};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistentRuntimeSensor {
    pub descriptor: SensorDescriptor,
    pub default_value: i64,
}

#[derive(Clone, Debug)]
pub struct VmRuntime {
    pub generation: u64,
    pub sensor_ids: HashMap<String, u32>,
    pub signatures: HashMap<String, (SensorKind, String)>,
    /// Maps agent sensor id → hwmon sysfs attribute name (e.g. `"temp1_input"`).
    pub sensor_attrs: HashMap<String, String>,
    pub persistent_sensors: Vec<PersistentRuntimeSensor>,
    pub last_seen: std::time::Instant,
    pub offline_reported: bool,
    /// `true` once a live agent has sent `Schema` over the socket.
    /// `false` when the runtime was restored from the persistent cache.
    /// Used by hostd to decide whether to send `RequestResync` on connect.
    pub schema_synced: bool,
    pub hwmon_name: String,
    pub hwmon_path: String,
    pub sensor_count: usize,
}

pub fn topology_events(
    prior: Option<&VmRuntime>,
    current: &HashMap<String, (SensorKind, String)>,
) -> Vec<String> {
    let Some(prior) = prior else {
        return vec!["device_created".to_string()];
    };

    let mut events = BTreeSet::new();

    for (id, signature) in current {
        match prior.signatures.get(id) {
            Some(old) if old == signature => {}
            Some(_) => {
                events.insert("sensor_changed".to_string());
            }
            None => {
                events.insert("sensor_added".to_string());
            }
        }
    }

    for id in prior.signatures.keys() {
        if !current.contains_key(id) {
            events.insert("sensor_removed".to_string());
        }
    }

    events.into_iter().collect()
}
