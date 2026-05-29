use anyhow::Result;

#[cfg(target_family = "unix")]
use anyhow::Context;

#[cfg(target_family = "unix")]
const DEFAULT_CTL_SOCKET: &str = "/run/vfio-sensor-bridge/hostd.sock";

// ── display ───────────────────────────────────────────────────────────────────

#[cfg(target_family = "unix")]
fn format_value(kind: vsb_protocol::SensorKind, raw: i64) -> String {
    use vsb_protocol::SensorKind;
    match kind {
        SensorKind::Temperature => {
            format!("{:.1} °C  (raw {} m°C)", raw as f64 / 1_000.0, raw)
        }
        SensorKind::Fan => format!("{} RPM", raw),
        SensorKind::Voltage => format!("{:.3} V   (raw {} mV)", raw as f64 / 1_000.0, raw),
        SensorKind::Current => format!("{:.3} A   (raw {} mA)", raw as f64 / 1_000.0, raw),
        SensorKind::Power => {
            format!("{:.3} W   (raw {} µW)", raw as f64 / 1_000_000.0, raw)
        }
    }
}

#[cfg(target_family = "unix")]
fn print_vm(info: &vsb_protocol::VmInfo, heartbeat_timeout: Option<u64>) {
    let agent_status = if info.online {
        let secs = info.last_seen_secs_ago;
        format!("online  (last seen {secs:.1}s ago)")
    } else if info.offline_reported {
        let secs = info.last_seen_secs_ago;
        format!(
            "offline (last seen {secs:.0}s ago{})",
            heartbeat_timeout
                .map(|t| format!(", timeout {t}s"))
                .unwrap_or_default()
        )
    } else {
        "unknown".to_string()
    };

    println!(
        "VMID {}  hwmon: {}  gen: {}",
        info.vmid, info.hwmon_name, info.generation
    );
    println!("  path:   {}", info.hwmon_path);
    println!("  agent:  {}", agent_status);

    if info.sensors.is_empty() {
        println!("  sensors: (none)");
    } else {
        const ATTR_W: usize = 18;
        const VALUE_W: usize = 28;

        let labels: Vec<String> = info
            .sensors
            .iter()
            .map(|s| {
                if s.label.is_empty() {
                    String::new()
                } else {
                    format!("[{}]", s.label)
                }
            })
            .collect();
        let label_w = labels
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max("LABEL".len());

        println!("  sensors ({}):", info.sensors.len());
        println!(
            "    {:<ATTR_W$}  {:<label_w$}  {:<VALUE_W$}  FLAGS",
            "ATTRIBUTE", "LABEL", "VALUE"
        );
        println!(
            "    {}  {}  {}  {}",
            "─".repeat(ATTR_W),
            "─".repeat(label_w),
            "─".repeat(VALUE_W),
            "─".repeat(12)
        );
        for (s, label) in info.sensors.iter().zip(labels.iter()) {
            let value_col = match s.value {
                Some(v) => format_value(s.kind, v),
                None => "(unreadable)".to_string(),
            };
            let flags = if s.persistent { "[persistent]" } else { "" };
            println!(
                "    {:<ATTR_W$}  {:<label_w$}  {:<VALUE_W$}  {}",
                s.hwmon_attr, label, value_col, flags
            );
        }
    }

    println!();
}

// ── public entry point ────────────────────────────────────────────────────────

#[cfg(target_family = "unix")]
pub fn run(args: &[String]) -> Result<()> {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use vsb_protocol::{read_ctl_response, write_ctl_request, CtlRequest, CtlResponse};

    let mut filter_vmid: Option<u32> = None;
    let mut socket_path = DEFAULT_CTL_SOCKET.to_string();
    let mut iter = args.iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--vmid" => {
                filter_vmid = Some(
                    iter.next()
                        .context("--vmid requires a value")?
                        .parse::<u32>()
                        .context("--vmid value must be an unsigned integer")?,
                );
            }
            "--socket" => {
                socket_path = iter
                    .next()
                    .context("--socket requires a value")?
                    .clone();
            }
            other => anyhow::bail!("unknown status argument: {other}"),
        }
    }

    let mut stream = UnixStream::connect(&socket_path).with_context(|| {
        format!(
            "connect to hostd management socket {socket_path}\n\
             (is hostd running? check: systemctl status vfio-sensor-bridge-hostd)"
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .context("set read timeout")?;

    let request = match filter_vmid {
        Some(vmid) => CtlRequest::GetVm { vmid },
        None => CtlRequest::ListVms,
    };
    write_ctl_request(&mut stream, &request).context("send request to hostd")?;

    let response = read_ctl_response(&mut stream).context("read response from hostd")?;

    match response {
        CtlResponse::VmList { vms } => {
            if vms.is_empty() {
                println!("hostd is running but no VMs are currently tracked.");
            } else {
                for vm in &vms {
                    print_vm(vm, None);
                }
            }
        }
        CtlResponse::VmInfo(info) => {
            print_vm(&info, None);
        }
        CtlResponse::NotFound { vmid } => {
            println!("VMID {vmid}: not tracked by hostd (schema never received or VM removed).");
        }
        CtlResponse::Error { message } => {
            anyhow::bail!("hostd returned error: {message}");
        }
    }

    Ok(())
}

#[cfg(not(target_family = "unix"))]
pub fn run(_args: &[String]) -> Result<()> {
    anyhow::bail!("vsbctl status is only supported on Linux hosts");
}



