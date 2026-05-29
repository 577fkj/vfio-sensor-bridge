use super::config::LsiHbaSection;
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem;
use std::os::fd::AsRawFd;
use std::path::Path;
use vsb_protocol::MAX_SENSOR_LABEL_BYTES;

pub fn probe_hba_temperatures(config: &LsiHbaSection) -> Result<()> {
    let mut found = 0_u32;
    for device in &config.devices {
        for ioc in 0..config.max_ioc {
            let value = match read_mpt2_ioc_temperature_millicelsius(device, ioc) {
                Ok(value) => value,
                Err(_) => continue,
            };
            found += 1;
            println!(
                "{} ioc{} {}: {}",
                device.display(),
                ioc,
                hba_ioc_label(config, device, ioc),
                value
            );
        }
    }

    if found == 0 {
        anyhow::bail!("no HBA IOC temperature found")
    }

    Ok(())
}

pub fn hba_ioc_label(config: &LsiHbaSection, device: &Path, ioc: u32) -> String {
    let identity = read_mpt2_ioc_identity(device, ioc).unwrap_or_else(|| HbaIocIdentity {
        portname: hba_portname(ioc),
        chip: format!("HBA IOC {ioc} Controller"),
        version: "unknown".to_string(),
    });
    render_hba_label(&config.label_template, &identity)
}

pub fn read_mpt2_ioc_temperature_millicelsius(device: &Path, ioc: u32) -> Result<i64> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .with_context(|| format!("open {}", device.display()))?;

    let mut page = Mpi2IoUnitPage7::default();
    get_mpt2_config_page(&file, ioc, 0x00, 7, 0, &mut page)
        .with_context(|| format!("read IOUnitPage7 from {} ioc {}", device.display(), ioc))?;

    hba_temperature_to_millicelsius(page.ioc_temperature, page.ioc_temperature_units)
        .context("IOUnitPage7 has no supported IOC temperature")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HbaIocIdentity {
    portname: String,
    chip: String,
    version: String,
}

fn read_mpt2_ioc_identity(device: &Path, ioc: u32) -> Option<HbaIocIdentity> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .ok()?;

    let mut page = Mpi2IocPage0::default();
    get_mpt2_config_page(&file, ioc, MPI2_CONFIG_PAGETYPE_IOC, 0, 0, &mut page).ok()?;

    if u16::from_le(page.vendor_id) != MPI2_MFGPAGE_VENDORID_LSI {
        return None;
    }

    let device_id = normalize_lsi_device_id(u16::from_le(page.device_id));
    let chip_name = get_chip_name_rev(device_id, page.revision_id);
    Some(HbaIocIdentity {
        portname: hba_portname(ioc),
        chip: format!("LSI Logic {chip_name}"),
        version: read_mpt2_ioc_firmware_version(&file, ioc)
            .unwrap_or_else(|| "unknown".to_string()),
    })
}

fn read_mpt2_ioc_firmware_version(file: &File, ioc: u32) -> Option<String> {
    let mut request = Mpi2IocFactsRequest {
        function: MPI2_FUNCTION_IOC_FACTS,
        ..Mpi2IocFactsRequest::default()
    };
    let mut reply = Mpi2IocFactsReply::default();

    mpt2_command(file, ioc, &mut request, &mut reply, None::<&mut ()>).ok()?;
    let status = u16::from_le(reply.ioc_status) & MPI2_IOCSTATUS_MASK;
    if status != MPI2_IOCSTATUS_SUCCESS {
        return None;
    }

    Some(format!("{:08x}", u32::from_le(reply.fw_version)))
}

fn hba_portname(ioc: u32) -> String {
    format!("ioc{ioc}")
}

fn render_hba_label(template: &str, identity: &HbaIocIdentity) -> String {
    let rendered = template
        .replace("{portname}", &identity.portname)
        .replace("{chip}", &identity.chip)
        .replace("{version}", &identity.version);
    normalize_hba_label(&rendered)
}

fn normalize_hba_label(label: &str) -> String {
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
        "HBA Controller".to_string()
    } else {
        out
    }
}

fn normalize_lsi_device_id(device_id_raw: u16) -> u16 {
    match device_id_raw {
        MPI_MANUFACTPAGE_DEVID_53C1030ZC
        | MPI_MANUFACTPAGE_DEVID_1030ZC_53C1035
        | MPI_MANUFACTPAGE_DEVID_53C1035ZC => device_id_raw & !1,
        _ => device_id_raw,
    }
}

fn get_chip_name_rev(device_id: u16, revision: u8) -> String {
    let template = match device_id {
        MPI2_MFGPAGE_DEVID_SAS2004 => match revision {
            0x00 => "SAS2004 A0",
            0x01 => "SAS2004 B0",
            0x02 => "SAS2004 B1",
            0x03 => "SAS2004 B2",
            _ => "SAS2004 xx",
        },
        MPI2_MFGPAGE_DEVID_SAS2008 => match revision {
            0x00 => "SAS2008 A0",
            0x01 => "SAS2008 B0",
            0x02 => "SAS2008 B1",
            0x03 => "SAS2008 B2",
            _ => "SAS2008 xx",
        },
        MPI2_MFGPAGE_DEVID_SAS2108_1
        | MPI2_MFGPAGE_DEVID_SAS2108_2
        | MPI2_MFGPAGE_DEVID_SAS2108_3 => match revision {
            0x00 => "SAS2108 A0",
            0xff => "SAS2 FPGA A0",
            0x01 | 0x02 => "SAS2108 B1",
            0x03 => "SAS2108 B2",
            0x04 => "SAS2108 B3",
            0x05 => "SAS2108 B4",
            _ => "SAS2108 xx",
        },
        MPI2_MFGPAGE_DEVID_SAS2116_1 | MPI2_MFGPAGE_DEVID_SAS2116_2 => match revision {
            0x00 => "SAS2116 A0",
            0x01 => "SAS2116 B0",
            0x02 => "SAS2116 B1",
            _ => "SAS2116 xx",
        },
        MPI2_MFGPAGE_DEVID_SAS2208_1
        | MPI2_MFGPAGE_DEVID_SAS2208_2
        | MPI2_MFGPAGE_DEVID_SAS2208_3
        | MPI2_MFGPAGE_DEVID_SAS2208_4
        | MPI2_MFGPAGE_DEVID_SAS2208_5
        | MPI2_MFGPAGE_DEVID_SAS2208_6 => match revision {
            0x00 => "SAS2208 A0",
            0x01 => "SAS2208 B0",
            0x02 => "SAS2208 C0",
            0x03 => "SAS2208 C1",
            0x04 => "SAS2208 D0",
            0x05 => "SAS2208 D1",
            _ => "SAS2208 xx",
        },
        MPI2_MFGPAGE_DEVID_SAS2308_1
        | MPI2_MFGPAGE_DEVID_SAS2308_2
        | MPI2_MFGPAGE_DEVID_SAS2308_3 => match revision {
            0x00 => "SAS2308 A0",
            0x01 => "SAS2308 B0",
            0x02 => "SAS2308 C0",
            0x03 => "SAS2308 C1",
            0x04 => "SAS2308 D0",
            0x05 => "SAS2308 D1",
            _ => "SAS2308 xx",
        },
        MPI25_MFGPAGE_DEVID_SAS3004 => match revision {
            0x00 => "SA3004 A0",
            0x01 => "SAS3004 B0",
            0x02 => "SAS3004 C0",
            _ => "SAS3004 xx",
        },
        MPI25_MFGPAGE_DEVID_SAS3008 => match revision {
            0x00 => "SA3008 A0",
            0x01 => "SAS3008 B0",
            0x02 => "SAS3008 C0",
            _ => "SAS3008 xx",
        },
        MPI25_MFGPAGE_DEVID_SAS3108_1
        | MPI25_MFGPAGE_DEVID_SAS3108_2
        | MPI25_MFGPAGE_DEVID_SAS3108_5
        | MPI25_MFGPAGE_DEVID_SAS3108_6 => match revision {
            0x00 => "SAS3108 A0",
            0x01 => "SAS3108 B0",
            0x02 => "SAS3108 C0",
            _ => "SAS3108 xx",
        },
        MPI2_MFGPAGE_DEVID_SSS6200 => match revision {
            0x00 => "SSS6200 A0",
            0x01 => "SSS6200 B0",
            0x02 => "SSS6200 C0",
            _ => "SSS6200 xx",
        },
        _ => "xxxx xx",
    };

    fill_unknown_revision(template, device_id, revision)
}

fn fill_unknown_revision(template: &str, device_id: u16, revision: u8) -> String {
    if template == "xxxx xx" {
        return format!("{device_id:04x} {revision:02x}");
    }

    if let Some(prefix) = template.strip_suffix("xx") {
        return format!("{prefix}{revision:02x}");
    }

    template.to_string()
}

fn hba_temperature_to_millicelsius(value: u16, units: u8) -> Option<i64> {
    match units {
        0x01 => Some(((i64::from(value) - 32) * 5 / 9) * 1000),
        0x02 => Some(i64::from(value) * 1000),
        _ => None,
    }
}

fn get_mpt2_config_page<T: Default>(
    file: &File,
    ioc: u32,
    page_type: u8,
    page_number: u8,
    page_address: u32,
    page: &mut T,
) -> Result<()> {
    let mut header_reply = Mpi2ConfigReply::default();
    let mut header_request = Mpi2ConfigRequestNoSge {
        action: MPI2_CONFIG_ACTION_PAGE_HEADER,
        function: MPI2_FUNCTION_CONFIG,
        header: Mpi2ConfigPageHeader {
            page_number,
            page_type,
            ..Mpi2ConfigPageHeader::default()
        },
        page_address,
        ..Mpi2ConfigRequestNoSge::default()
    };

    mpt2_command(
        file,
        ioc,
        &mut header_request,
        &mut header_reply,
        None::<&mut ()>,
    )?;
    ensure_mpt2_config_success(&header_reply)?;

    let mut read_reply = Mpi2ConfigReply::default();
    let mut read_request = Mpi2ConfigRequestNoSge {
        action: MPI2_CONFIG_ACTION_PAGE_READ_CURRENT,
        function: MPI2_FUNCTION_CONFIG,
        header: header_reply.header,
        ext_page_length: header_reply.ext_page_length,
        ext_page_type: header_reply.ext_page_type,
        page_address,
        ..Mpi2ConfigRequestNoSge::default()
    };

    mpt2_command(file, ioc, &mut read_request, &mut read_reply, Some(page))?;
    ensure_mpt2_config_success(&read_reply)
}

fn ensure_mpt2_config_success(reply: &Mpi2ConfigReply) -> Result<()> {
    let status = u16::from_le(reply.ioc_status) & MPI2_IOCSTATUS_MASK;
    if status == MPI2_IOCSTATUS_SUCCESS {
        return Ok(());
    }
    anyhow::bail!("MPT config IOCStatus 0x{status:04x}")
}

fn mpt2_command<Req, Rep, PayIn>(
    file: &File,
    ioc: u32,
    request: &mut Req,
    reply: &mut Rep,
    payload_in: Option<&mut PayIn>,
) -> Result<()> {
    let payload_ptr = payload_in
        .as_ref()
        .map(|payload| *payload as *const PayIn as *mut libc::c_void)
        .unwrap_or(std::ptr::null_mut());
    let payload_size = payload_in
        .as_ref()
        .map(|_| mem::size_of::<PayIn>() as u32)
        .unwrap_or(0);
    let request_bytes = unsafe {
        std::slice::from_raw_parts(request as *mut Req as *const u8, mem::size_of::<Req>())
    };
    if request_bytes.len() > MPT2_MAX_REQUEST_BYTES {
        anyhow::bail!("MPT request exceeds ioctl buffer");
    }

    let mut command = Mpt2IoctlCommand {
        hdr: Mpt2IoctlHeader {
            ioc_number: ioc,
            port_number: 0,
            max_data_size: payload_size,
        },
        timeout: 10,
        reply_frame_buf_ptr: reply as *mut Rep as *mut libc::c_void,
        data_in_buf_ptr: payload_ptr,
        data_out_buf_ptr: std::ptr::null_mut(),
        sense_data_ptr: std::ptr::null_mut(),
        max_reply_bytes: mem::size_of::<Rep>() as u32,
        data_in_size: payload_size,
        data_out_size: 0,
        max_sense_bytes: 0,
        data_sge_offset: (mem::size_of::<Req>() / 4) as u32,
        mf: [0; MPT2_MAX_REQUEST_BYTES],
    };
    command.mf[..request_bytes.len()].copy_from_slice(request_bytes);

    let status = unsafe { libc::ioctl(file.as_raw_fd(), mpt2command_ioctl(), &mut command) };
    if status != 0 {
        return Err(io::Error::last_os_error()).context("MPT2COMMAND ioctl");
    }
    Ok(())
}

const MPT2_MAX_REQUEST_BYTES: usize = mem::size_of::<Mpi2ConfigRequestNoSge>();
const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;
const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const MPT2_MAGIC_NUMBER: u64 = b'L' as u64;
const MPT2COMMAND_NR: u64 = 20;
const MPI2_FUNCTION_IOC_FACTS: u8 = 0x03;
const MPI2_FUNCTION_CONFIG: u8 = 0x04;
const MPI2_CONFIG_ACTION_PAGE_HEADER: u8 = 0x00;
const MPI2_CONFIG_ACTION_PAGE_READ_CURRENT: u8 = 0x01;
const MPI2_CONFIG_PAGETYPE_IOC: u8 = 0x01;
const MPI2_MFGPAGE_VENDORID_LSI: u16 = 0x1000;
const MPI_MANUFACTPAGE_DEVID_53C1030ZC: u16 = 0x0031;
const MPI_MANUFACTPAGE_DEVID_1030ZC_53C1035: u16 = 0x0033;
const MPI_MANUFACTPAGE_DEVID_53C1035ZC: u16 = 0x0041;
const MPI2_MFGPAGE_DEVID_SAS2004: u16 = 0x0070;
const MPI2_MFGPAGE_DEVID_SAS2008: u16 = 0x0072;
const MPI2_MFGPAGE_DEVID_SAS2108_1: u16 = 0x0074;
const MPI2_MFGPAGE_DEVID_SAS2108_2: u16 = 0x0076;
const MPI2_MFGPAGE_DEVID_SAS2108_3: u16 = 0x0077;
const MPI2_MFGPAGE_DEVID_SAS2116_1: u16 = 0x0064;
const MPI2_MFGPAGE_DEVID_SAS2116_2: u16 = 0x0065;
const MPI2_MFGPAGE_DEVID_SSS6200: u16 = 0x007e;
const MPI2_MFGPAGE_DEVID_SAS2208_1: u16 = 0x0080;
const MPI2_MFGPAGE_DEVID_SAS2208_2: u16 = 0x0081;
const MPI2_MFGPAGE_DEVID_SAS2208_3: u16 = 0x0082;
const MPI2_MFGPAGE_DEVID_SAS2208_4: u16 = 0x0083;
const MPI2_MFGPAGE_DEVID_SAS2208_5: u16 = 0x0084;
const MPI2_MFGPAGE_DEVID_SAS2208_6: u16 = 0x0085;
const MPI2_MFGPAGE_DEVID_SAS2308_1: u16 = 0x0086;
const MPI2_MFGPAGE_DEVID_SAS2308_2: u16 = 0x0087;
const MPI2_MFGPAGE_DEVID_SAS2308_3: u16 = 0x006e;
const MPI25_MFGPAGE_DEVID_SAS3004: u16 = 0x0096;
const MPI25_MFGPAGE_DEVID_SAS3008: u16 = 0x0097;
const MPI25_MFGPAGE_DEVID_SAS3108_1: u16 = 0x0090;
const MPI25_MFGPAGE_DEVID_SAS3108_2: u16 = 0x0091;
const MPI25_MFGPAGE_DEVID_SAS3108_5: u16 = 0x0094;
const MPI25_MFGPAGE_DEVID_SAS3108_6: u16 = 0x0095;
const MPI2_IOCSTATUS_MASK: u16 = 0x7fff;
const MPI2_IOCSTATUS_SUCCESS: u16 = 0x0000;

fn mpt2command_ioctl() -> libc::c_ulong {
    ioc(
        IOC_READ | IOC_WRITE,
        MPT2_MAGIC_NUMBER,
        MPT2COMMAND_NR,
        mem::size_of::<Mpt2IoctlCommandBase>() as u64,
    ) as libc::c_ulong
}

fn ioc(dir: u64, kind: u64, nr: u64, size: u64) -> u64 {
    (dir << IOC_DIRSHIFT) | (kind << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpt2IoctlHeader {
    ioc_number: u32,
    port_number: u32,
    max_data_size: u32,
}

#[repr(C)]
struct Mpt2IoctlCommandBase {
    hdr: Mpt2IoctlHeader,
    timeout: u32,
    reply_frame_buf_ptr: *mut libc::c_void,
    data_in_buf_ptr: *mut libc::c_void,
    data_out_buf_ptr: *mut libc::c_void,
    sense_data_ptr: *mut libc::c_void,
    max_reply_bytes: u32,
    data_in_size: u32,
    data_out_size: u32,
    max_sense_bytes: u32,
    data_sge_offset: u32,
    mf: [u8; 1],
}

#[repr(C)]
struct Mpt2IoctlCommand {
    hdr: Mpt2IoctlHeader,
    timeout: u32,
    reply_frame_buf_ptr: *mut libc::c_void,
    data_in_buf_ptr: *mut libc::c_void,
    data_out_buf_ptr: *mut libc::c_void,
    sense_data_ptr: *mut libc::c_void,
    max_reply_bytes: u32,
    data_in_size: u32,
    data_out_size: u32,
    max_sense_bytes: u32,
    data_sge_offset: u32,
    mf: [u8; MPT2_MAX_REQUEST_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2ConfigPageHeader {
    page_version: u8,
    page_length: u8,
    page_number: u8,
    page_type: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2ConfigRequestNoSge {
    action: u8,
    sgl_flags: u8,
    chain_offset: u8,
    function: u8,
    ext_page_length: u16,
    ext_page_type: u8,
    msg_flags: u8,
    vp_id: u8,
    vf_id: u8,
    reserved1: u16,
    reserved2: u8,
    proxy_vf_id: u8,
    reserved4: u16,
    reserved3: u32,
    header: Mpi2ConfigPageHeader,
    page_address: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2ConfigReply {
    action: u8,
    sgl_flags: u8,
    msg_length: u8,
    function: u8,
    ext_page_length: u16,
    ext_page_type: u8,
    msg_flags: u8,
    vp_id: u8,
    vf_id: u8,
    reserved1: u16,
    reserved2: u16,
    ioc_status: u16,
    ioc_log_info: u32,
    header: Mpi2ConfigPageHeader,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2IocFactsRequest {
    reserved1: u16,
    chain_offset: u8,
    function: u8,
    reserved2: u16,
    reserved3: u8,
    msg_flags: u8,
    vp_id: u8,
    vf_id: u8,
    reserved4: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2IocFactsReply {
    msg_version: u16,
    msg_length: u8,
    function: u8,
    header_version: u16,
    ioc_number: u8,
    msg_flags: u8,
    vp_id: u8,
    vf_id: u8,
    reserved1: u16,
    ioc_exceptions: u16,
    ioc_status: u16,
    ioc_log_info: u32,
    max_chain_depth: u8,
    who_init: u8,
    number_of_ports: u8,
    max_msix_vectors: u8,
    request_credit: u16,
    product_id: u16,
    ioc_capabilities: u32,
    fw_version: u32,
    ioc_request_frame_size: u16,
    ioc_max_chain_segment_size: u16,
    max_initiators: u16,
    max_targets: u16,
    max_sas_expanders: u16,
    max_enclosures: u16,
    protocol_flags: u16,
    high_priority_credit: u16,
    max_reply_descriptor_post_queue_depth: u16,
    reply_frame_size: u8,
    max_volumes: u8,
    max_dev_handle: u16,
    max_persistent_entries: u16,
    min_dev_handle: u16,
    reserved4: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2IocPage0 {
    header: Mpi2ConfigPageHeader,
    reserved1: u32,
    reserved2: u32,
    vendor_id: u16,
    device_id: u16,
    revision_id: u8,
    reserved3: u8,
    reserved4: u16,
    class_code: u32,
    subsystem_vendor_id: u16,
    subsystem_id: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mpi2IoUnitPage7 {
    header: Mpi2ConfigPageHeader,
    current_power_mode: u8,
    previous_power_mode: u8,
    pcie_width: u8,
    pcie_speed: u8,
    processor_state: u32,
    power_management_capabilities: u32,
    ioc_temperature: u16,
    ioc_temperature_units: u8,
    ioc_speed: u8,
    board_temperature: u16,
    board_temperature_units: u8,
    reserved3: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_hba_celsius_to_hwmon_units() {
        assert_eq!(hba_temperature_to_millicelsius(55, 0x02), Some(55_000));
    }

    #[test]
    fn converts_hba_fahrenheit_to_hwmon_units() {
        assert_eq!(hba_temperature_to_millicelsius(131, 0x01), Some(55_000));
    }

    #[test]
    fn rejects_missing_hba_temperature() {
        assert_eq!(hba_temperature_to_millicelsius(55, 0x00), None);
    }

    #[test]
    fn mpt2command_ioctl_matches_linux_iowr_layout() {
        assert_eq!(mpt2command_ioctl(), 0xc048_4c14);
    }

    #[test]
    fn names_sas2308_d1_like_hba_client() {
        assert_eq!(
            get_chip_name_rev(MPI2_MFGPAGE_DEVID_SAS2308_2, 0x05),
            "SAS2308 D1"
        );
        assert_eq!(
            get_chip_name_rev(MPI2_MFGPAGE_DEVID_SAS2308_1, 0x05),
            "SAS2308 D1"
        );
    }

    #[test]
    fn fills_unknown_revision_like_hba_client() {
        assert_eq!(
            get_chip_name_rev(MPI2_MFGPAGE_DEVID_SAS2308_2, 0x06),
            "SAS2308 06"
        );
    }

    #[test]
    fn fills_unknown_device_like_hba_client() {
        assert_eq!(get_chip_name_rev(0x1234, 0x56), "1234 56");
    }

    #[test]
    fn normalizes_zc_device_ids_like_hba_client() {
        assert_eq!(
            normalize_lsi_device_id(MPI_MANUFACTPAGE_DEVID_53C1030ZC),
            0x0030
        );
    }

    #[test]
    fn renders_hba_label_template_placeholders() {
        let identity = HbaIocIdentity {
            portname: "ioc0".to_string(),
            chip: "LSI Logic SAS2308 D1".to_string(),
            version: "14000700".to_string(),
        };

        assert_eq!(
            render_hba_label("{portname} {chip} fw {version}", &identity),
            "ioc0 LSI Logic SAS2308 D1 fw 14000700"
        );
    }

    #[test]
    fn formats_firmware_version_like_lsiutil() {
        assert_eq!(format!("{:08x}", 0x1400_0700_u32), "14000700");
    }
}
