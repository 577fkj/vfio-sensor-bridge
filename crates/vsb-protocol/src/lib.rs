use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const MAX_FRAME_SIZE: usize = 256 * 1024;
pub const MAX_SENSORS: usize = 128;
pub const MAX_SENSOR_LABEL_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorKind {
    Temperature,
    Fan,
    Voltage,
    Current,
    Power,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SensorDescriptor {
    pub id: String,
    pub kind: SensorKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub persistent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SensorValue {
    pub id: String,
    pub value: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        agent_version: String,
        hostname: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hwmon_name: Option<String>,
    },
    Schema {
        generation: u64,
        sensors: Vec<SensorDescriptor>,
    },
    Sample {
        generation: u64,
        values: Vec<SensorValue>,
    },
    Heartbeat,
    Goodbye,
    /// Sent by the host daemon to the agent immediately after connecting.
    /// The agent must respond by resending `Hello`, `Schema`, and `Sample`
    /// so hostd can register the full sensor set (including non-persistent
    /// sensors that are not stored in the persistent cache).
    RequestResync,
}

// ── Management protocol (vsbctl ↔ hostd) ─────────────────────────────────────

/// Request sent by vsbctl to the hostd management socket.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CtlRequest {
    /// Return status for every known VM.
    ListVms,
    /// Return status for one VM.
    GetVm { vmid: u32 },
}

/// Per-sensor information returned by hostd.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensorInfo {
    /// Agent-assigned sensor identifier string.
    pub id: String,
    pub kind: SensorKind,
    pub label: String,
    /// Whether the sensor is marked persistent (value is kept when VM goes offline).
    pub persistent: bool,
    /// Default value written to the kernel when VM is offline (persistent sensors only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<i64>,
    /// hwmon sysfs attribute name, e.g. `temp1_input`.
    pub hwmon_attr: String,
    /// Current raw value read from sysfs by hostd; `None` if unreadable.
    pub value: Option<i64>,
}

/// Per-VM status returned by hostd.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VmInfo {
    pub vmid: u32,
    pub hwmon_name: String,
    pub hwmon_path: String,
    /// Schema generation counter (increments with each topology change).
    pub generation: u64,
    /// `true` if the last heartbeat arrived within the configured timeout.
    pub online: bool,
    pub offline_reported: bool,
    /// Seconds since the last message received from this VM's agent.
    pub last_seen_secs_ago: f64,
    pub sensors: Vec<SensorInfo>,
}

/// Response sent by hostd to vsbctl.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CtlResponse {
    /// Reply to `ListVms`.
    VmList { vms: Vec<VmInfo> },
    /// Reply to `GetVm` when the VM is known.
    VmInfo(VmInfo),
    /// Reply to `GetVm` when the VMID is not tracked by hostd.
    NotFound { vmid: u32 },
    /// Unexpected error.
    Error { message: String },
}

/// Read one [`CtlRequest`] from `reader` using the standard `u32_le + JSON` framing.
pub fn read_ctl_request<R: Read>(reader: &mut R) -> io::Result<CtlRequest> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ctl request frame exceeds maximum size",
        ));
    }
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write one [`CtlResponse`] to `writer` using the standard `u32_le + JSON` framing.
pub fn write_ctl_response<W: Write>(writer: &mut W, response: &CtlResponse) -> io::Result<()> {
    let payload = serde_json::to_vec(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ctl response frame exceeds maximum size",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Write one [`CtlRequest`] to `writer` using the standard `u32_le + JSON` framing.
pub fn write_ctl_request<W: Write>(writer: &mut W, request: &CtlRequest) -> io::Result<()> {
    let payload = serde_json::to_vec(request)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ctl request frame exceeds maximum size",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Read one [`CtlResponse`] from `reader` using the standard `u32_le + JSON` framing.
pub fn read_ctl_response<R: Read>(reader: &mut R) -> io::Result<CtlResponse> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ctl response frame exceeds maximum size",
        ));
    }
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Message> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds maximum size",
        ));
    }

    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    let message: Message = serde_json::from_slice(&payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    validate_message(&message)?;
    Ok(message)
}

pub fn write_frame<W: Write>(writer: &mut W, message: &Message) -> io::Result<()> {
    validate_message(message)?;

    let payload = serde_json::to_vec(message)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    if payload.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds maximum size",
        ));
    }

    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

pub fn validate_message(message: &Message) -> io::Result<()> {
    match message {
        Message::Hello {
            agent_version,
            hostname,
            hwmon_name,
        } => {
            validate_text(agent_version, 128, "agent_version")?;
            validate_text(hostname, 255, "hostname")?;
            if let Some(hwmon_name) = hwmon_name {
                validate_hwmon_name(hwmon_name)?;
            }
        }
        Message::Schema { sensors, .. } => {
            if sensors.len() > MAX_SENSORS {
                return invalid("schema has too many sensors");
            }

            let mut ids = std::collections::HashSet::with_capacity(sensors.len());
            for sensor in sensors {
                validate_text(&sensor.id, 512, "sensor id")?;
                validate_text(&sensor.label, MAX_SENSOR_LABEL_BYTES, "sensor label")?;
                if sensor.persistent && sensor.default_value.is_none() {
                    return invalid("persistent sensor is missing default_value");
                }
                if !sensor.persistent && sensor.default_value.is_some() {
                    return invalid("dynamic sensor includes default_value");
                }
                if !ids.insert(&sensor.id) {
                    return invalid("schema has duplicate sensor id");
                }
            }
        }
        Message::Sample { values, .. } => {
            if values.len() > MAX_SENSORS {
                return invalid("sample has too many values");
            }

            let mut ids = std::collections::HashSet::with_capacity(values.len());
            for value in values {
                validate_text(&value.id, 512, "sensor id")?;
                if !ids.insert(&value.id) {
                    return invalid("sample has duplicate sensor id");
                }
            }
        }
        Message::Heartbeat | Message::Goodbye | Message::RequestResync => {}
    }

    Ok(())
}

fn validate_text(value: &str, max_len: usize, name: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > max_len
        || value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return invalid(&format!("invalid {name}"));
    }
    Ok(())
}

fn validate_hwmon_name(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 31
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
    {
        return invalid("invalid hwmon_name");
    }
    Ok(())
}

fn invalid<T>(message: &str) -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_string(),
    ))
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_schema_frame() {
        let message = Message::Schema {
            generation: 7,
            sensors: vec![SensorDescriptor {
                id: "hwmon0:temp1".to_string(),
                kind: SensorKind::Temperature,
                label: "GPU temp".to_string(),
                persistent: false,
                default_value: None,
            }],
        };
        let mut buf = Vec::new();

        write_frame(&mut buf, &message).unwrap();
        let decoded = read_frame(&mut Cursor::new(buf)).unwrap();

        assert_eq!(decoded, message);
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&((MAX_FRAME_SIZE as u32) + 1).to_le_bytes());

        let err = read_frame(&mut Cursor::new(raw)).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_duplicate_schema_sensor_ids() {
        let message = Message::Schema {
            generation: 1,
            sensors: vec![
                SensorDescriptor {
                    id: "dup".to_string(),
                    kind: SensorKind::Temperature,
                    label: "temp1".to_string(),
                    persistent: false,
                    default_value: None,
                },
                SensorDescriptor {
                    id: "dup".to_string(),
                    kind: SensorKind::Fan,
                    label: "fan1".to_string(),
                    persistent: false,
                    default_value: None,
                },
            ],
        };

        assert_eq!(
            validate_message(&message).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn accepts_128_byte_sensor_label() {
        let message = Message::Schema {
            generation: 1,
            sensors: vec![SensorDescriptor {
                id: "long".to_string(),
                kind: SensorKind::Temperature,
                label: "x".repeat(MAX_SENSOR_LABEL_BYTES),
                persistent: false,
                default_value: None,
            }],
        };

        validate_message(&message).unwrap();
    }

    #[test]
    fn rejects_129_byte_sensor_label() {
        let message = Message::Schema {
            generation: 1,
            sensors: vec![SensorDescriptor {
                id: "too-long".to_string(),
                kind: SensorKind::Temperature,
                label: "x".repeat(MAX_SENSOR_LABEL_BYTES + 1),
                persistent: false,
                default_value: None,
            }],
        };

        assert_eq!(
            validate_message(&message).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn requires_default_value_for_persistent_sensor() {
        let message = Message::Schema {
            generation: 1,
            sensors: vec![SensorDescriptor {
                id: "persistent-temp".to_string(),
                kind: SensorKind::Temperature,
                label: "Persistent Temp".to_string(),
                persistent: true,
                default_value: None,
            }],
        };

        assert_eq!(
            validate_message(&message).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn accepts_persistent_sensor_with_default_value() {
        let message = Message::Schema {
            generation: 1,
            sensors: vec![SensorDescriptor {
                id: "persistent-temp".to_string(),
                kind: SensorKind::Temperature,
                label: "Persistent Temp".to_string(),
                persistent: true,
                default_value: Some(65_000),
            }],
        };

        validate_message(&message).unwrap();
    }
}
