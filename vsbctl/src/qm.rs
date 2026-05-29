use anyhow::{Context, Result};
use std::fs;
use std::process::Command;

const RUN_DIR: &str = "/run/vfio-sensor-bridge";
const CHANNEL_NAME: &str = "org.vfio_sensor_bridge.0";

pub fn attach(vmid: &str, dry_run: bool) -> Result<()> {
    vmid.parse::<u32>()
        .context("VMID must be an unsigned integer")?;
    fs::create_dir_all(RUN_DIR).with_context(|| format!("create {RUN_DIR}"))?;

    let existing_args = read_qm_args(vmid)?;
    if has_bridge_channel(&existing_args) {
        eprintln!("VM {vmid} already has vfio-sensor-bridge channel");
        return Ok(());
    }

    let next_args = if existing_args.trim().is_empty() {
        channel_args(vmid)
    } else {
        format!("{} {}", existing_args.trim(), channel_args(vmid))
    };

    qm_set_args(vmid, &next_args, dry_run)
}

pub fn detach(vmid: &str, dry_run: bool) -> Result<()> {
    vmid.parse::<u32>()
        .context("VMID must be an unsigned integer")?;

    let existing_args = read_qm_args(vmid)?;
    let next_args = remove_bridge_channel(&existing_args, vmid);

    if collapse_spaces(&existing_args) == next_args {
        eprintln!("VM {vmid} has no vfio-sensor-bridge channel args to remove");
        return Ok(());
    }

    qm_set_args(vmid, &next_args, dry_run)
}

fn qm_set_args(vmid: &str, args: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("qm set {vmid} --args {args}");
        return Ok(());
    }

    let status = Command::new("qm")
        .args(["set", vmid, "--args", args])
        .status()
        .context("run qm set")?;
    if !status.success() {
        anyhow::bail!("qm set exited with {status}");
    }

    Ok(())
}

fn channel_args(vmid: &str) -> String {
    format!(
        "-chardev socket,id=vsb0,path={RUN_DIR}/vm-{vmid}.sock,server=on,wait=off \
         -device virtio-serial-pci,id=vsbserial0 \
         -device virtserialport,chardev=vsb0,name={CHANNEL_NAME}"
    )
}

fn collapse_spaces(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_bridge_channel(args: &str) -> bool {
    args.split_whitespace().any(is_bridge_device_arg)
}

fn remove_bridge_channel(args: &str, vmid: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut kept = Vec::with_capacity(tokens.len());
    let mut idx = 0;

    while idx < tokens.len() {
        if idx + 1 < tokens.len()
            && (tokens[idx] == "-chardev" || tokens[idx] == "-device")
            && is_bridge_pair(tokens[idx], tokens[idx + 1], vmid)
        {
            idx += 2;
            continue;
        }

        kept.push(tokens[idx]);
        idx += 1;
    }

    kept.join(" ")
}

fn is_bridge_pair(kind: &str, value: &str, vmid: &str) -> bool {
    match kind {
        "-chardev" => {
            value.starts_with("socket,")
                && value.contains("id=vsb0")
                && value.contains(&format!("path={RUN_DIR}/vm-{vmid}.sock"))
        }
        "-device" => is_bridge_device_arg(value),
        _ => false,
    }
}

fn is_bridge_device_arg(value: &str) -> bool {
    (value.starts_with("virtserialport,") && value.contains(&format!("name={CHANNEL_NAME}")))
        || (value.starts_with("virtio-serial-pci,") && value.contains("id=vsbserial0"))
}

fn read_qm_args(vmid: &str) -> Result<String> {
    let output = Command::new("qm")
        .args(["config", vmid])
        .output()
        .context("run qm config")?;
    if !output.status.success() {
        anyhow::bail!("qm config exited with {}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(args) = line.strip_prefix("args: ") {
            return Ok(args.to_string());
        }
    }

    Ok(String::new())
}
