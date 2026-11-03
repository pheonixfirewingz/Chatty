//! PID 1 duties: bring up the filesystems, make the model drive read-only, and report
//! status over the network. A failure never exits, because PID 1 exiting panics the
//! kernel and reboots with no record of why. The status port explains it instead.

use crate::{
    Error, Result,
    catalog::Catalog,
    invalid, monitor,
    plan::{Budget, GIB, Plan},
};
use rustix::{
    fs::{Mode, OFlags, StatVfsMountFlags, open, statvfs},
    mount::{MountFlags, mount},
};
use std::{
    ffi::CStr,
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const DISK: &str = "/dev/nvme0n1";
const DISK_RO_FLAG: &str = "/sys/block/nvme0n1/ro";
const PARTITION: &str = "/dev/nvme0n1p3";
const MOUNT_POINT: &str = "/model";
const MOUNT_OPTIONS: &CStr = c"subvol=/home,rescue=nologreplay";
const VRAM_TOTAL: &str = "/sys/class/drm/card0/device/mem_info_vram_total";
const DRIVE_WAIT: Duration = Duration::from_secs(30);

/// Run as PID 1. Does not return.
pub fn run() -> ! {
    let mut status = String::new();
    let model_dir = match bring_up() {
        Ok((report, model_dir)) => {
            status.push_str(&report);
            Some(model_dir)
        }
        Err(error) => {
            let _ = writeln!(status, "boot failed: {error}");
            None
        }
    };
    log_line(&status);
    start_monitor();
    crate::server::serve(model_dir, &status)
}

/// Show machine load on the attached monitor. It runs beside the server, so a display
/// problem costs only the display.
fn start_monitor() {
    let spawned = thread::Builder::new().name("monitor".to_owned()).spawn(|| {
        if let Err(error) = monitor::run() {
            log_line(&format!("monitor stopped: {error}"));
        }
    });
    if let Err(error) = spawned {
        log_line(&format!("monitor did not start: {error}"));
    }
}

fn bring_up() -> Result<(String, PathBuf)> {
    mount_pseudo("proc", "/proc", "proc")?;
    mount_pseudo("sysfs", "/sys", "sysfs")?;
    mount_pseudo("devtmpfs", "/dev", "devtmpfs")?;

    #[cfg(feature = "debug-agent")]
    open_gpu_to_debug_jobs();

    let start = Instant::now();
    while !Path::new(PARTITION).exists() {
        if start.elapsed() > DRIVE_WAIT {
            return invalid(format!("{PARTITION} did not appear"));
        }
        thread::sleep(Duration::from_millis(100));
    }

    protect_drive()?;
    hold_stock_top_clocks();

    let cmdline = fs::read_to_string("/proc/cmdline")
        .map_err(|error| Error::Io("/proc/cmdline".into(), error))?;
    let Some(model) = model_path(&cmdline) else {
        return invalid("chatty.model is not on the kernel command line");
    };

    let model_dir = Path::new(MOUNT_POINT).join(model);
    #[cfg(feature = "debug-agent")]
    expose_model_to_debug_jobs(&model_dir);

    let mut report = String::new();
    let _ = writeln!(report, "drive read-only: {DISK}");
    match (Catalog::open(&model_dir), read_trimmed(VRAM_TOTAL)) {
        (Ok(catalog), vram) => {
            let mut budget = Budget::default();
            if let Some(bytes) = vram.as_deref().and_then(|text| text.parse().ok()) {
                budget.vram = bytes;
            }
            let plan = Plan::new(&catalog, budget)?;
            let _ = writeln!(report, "model: {} GiB on drive", plan.drive_total / GIB);
            let _ = writeln!(
                report,
                "experts cached: {} VRAM, {} RAM, of {}",
                plan.vram_expert_slots, plan.ram_expert_slots, plan.expert_total
            );
        }
        (Err(error), _) => {
            let _ = writeln!(report, "model not ready: {error}");
        }
    }
    let _ = writeln!(
        report,
        "gpu vram bytes: {}",
        read_trimmed(VRAM_TOTAL).as_deref().unwrap_or("missing")
    );
    let _ = writeln!(
        report,
        "kfd: {}",
        if Path::new("/dev/kfd").exists() {
            "present"
        } else {
            "missing"
        }
    );
    Ok((report, model_dir))
}

/// Inference arrives as bursts of short dispatches, which the default governor treats as a light
/// load. Hold the top of the stock clock table. Never overclock: this sets no clock or voltage
/// beyond the stock table and leaves the power limit at its default. A failure only costs speed.
fn hold_stock_top_clocks() {
    const LEVEL: &str = "/sys/class/drm/card0/device/power_dpm_force_performance_level";
    if let Err(error) = fs::write(LEVEL, "high") {
        log_line(&format!("{LEVEL}: {error}"));
    }
}

/// Debug jobs run unprivileged, but they still need the GPU. The disk nodes stay root-only.
#[cfg(feature = "debug-agent")]
fn open_gpu_to_debug_jobs() {
    use std::os::unix::fs::PermissionsExt;
    for node in ["/dev/kfd", "/dev/dri/renderD128"] {
        if let Err(error) = fs::set_permissions(node, fs::Permissions::from_mode(0o666)) {
            log_line(&format!("{node}: {error}"));
        }
    }
}

/// Bind-mount only the read-only model directory at `/debug-model` so unprivileged debug
/// jobs (`uid 65534`) can read model weights without traversing `0700` home directories.
#[cfg(feature = "debug-agent")]
fn expose_model_to_debug_jobs(model_dir: &Path) {
    const TARGET: &str = "/debug-model";
    if let Err(error) = fs::create_dir_all(TARGET) {
        log_line(&format!("{TARGET}: {error}"));
        return;
    }
    if let Err(error) = rustix::mount::mount_bind(model_dir, TARGET) {
        log_line(&format!("bind {TARGET}: {error}"));
    }
}

/// Set the whole disk read-only, which also covers its partitions, then mount the model
/// filesystem and verify both protections. Any failure leaves the filesystem unmounted.
fn protect_drive() -> Result<()> {
    let disk = open(DISK, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .map_err(|error| os(DISK, error))?;
    // SAFETY: BLKROSET reads one int through the pointer, which `Setter` keeps alive
    // for the call, and `disk` is an open block device.
    unsafe { rustix::ioctl::ioctl(&disk, rustix::ioctl::Setter::<BLKROSET, i32>::new(1)) }
        .map_err(|error| os(DISK, error))?;
    if read_trimmed(DISK_RO_FLAG).as_deref() != Some("1") {
        return invalid(format!("{DISK} did not become read-only"));
    }

    mount(
        PARTITION,
        MOUNT_POINT,
        "btrfs",
        MountFlags::RDONLY | MountFlags::NOATIME,
        MOUNT_OPTIONS,
    )
    .map_err(|error| os(PARTITION, error))?;
    let flags = statvfs(MOUNT_POINT)
        .map_err(|error| os(MOUNT_POINT, error))?
        .f_flag;
    if !flags.contains(StatVfsMountFlags::RDONLY) {
        return invalid(format!("{MOUNT_POINT} is not mounted read-only"));
    }
    Ok(())
}

/// `_IO(0x12, 93)` from `linux/fs.h`.
const BLKROSET: rustix::ioctl::Opcode = rustix::ioctl::opcode::none(0x12, 93);

fn mount_pseudo(source: &str, target: &str, kind: &str) -> Result<()> {
    mount(
        source,
        target,
        kind,
        MountFlags::NOSUID | MountFlags::NOEXEC,
        None,
    )
    .map_err(|error| os(target, error))
}

fn os(path: &str, error: rustix::io::Errno) -> Error {
    Error::Io(
        path.into(),
        io::Error::from_raw_os_error(error.raw_os_error()),
    )
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

/// The model directory from `chatty.model=`, relative to the mounted subvolume. A path
/// that could climb out of the mount is refused.
fn model_path(cmdline: &str) -> Option<&str> {
    let value = cmdline
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("chatty.model="))?;
    let relative = value.trim_start_matches('/');
    (!relative.is_empty() && !relative.split('/').any(|part| part == "..")).then_some(relative)
}

/// Kernel log, which netconsole forwards off the machine.
pub(crate) fn log_line(message: &str) {
    if let Ok(mut kmsg) = OpenOptions::new().write(true).open("/dev/kmsg") {
        for line in message.lines() {
            // One write is one log record, so build the whole line first.
            let _ = kmsg.write_all(format!("chatty-metal: {line}\n").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_model_path_on_command_line() {
        let cmdline = "rdinit=/init panic=10 chatty.model=/digitech/models/x ip=1.2.3.4\n";
        assert_eq!(model_path(cmdline), Some("digitech/models/x"));
    }

    #[test]
    fn refuses_missing_empty_or_climbing_model_paths() {
        assert_eq!(model_path("panic=10"), None);
        assert_eq!(model_path("chatty.model=/"), None);
        assert_eq!(model_path("chatty.model=/a/../../etc"), None);
    }
}
