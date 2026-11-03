//! Hardware check run as a debug job, with `metal_run`: proves that a job cannot lift the
//! model drive's protection. It never sends a write. If the job somehow kept any
//! privilege, it stops before trying anything.

use rustix::{
    fs::{Mode, OFlags, open},
    io::Errno,
    ioctl::{Setter, Updater, ioctl, opcode},
    process::getuid,
};
use std::{fs, process::ExitCode};

const BLKROSET: rustix::ioctl::Opcode = opcode::none(0x12, 93);
/// `struct nvme_admin_cmd` is 72 bytes.
const NVME_IOCTL_ADMIN_CMD: rustix::ioctl::Opcode = opcode::read_write::<[u8; 72]>(b'N', 0x41);
/// Identify is a read, so this probe is harmless. It goes through the same kernel gate as
/// every passthrough write, and the gate must refuse it.
const NVME_ADMIN_IDENTIFY: u8 = 0x06;

fn main() -> ExitCode {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok());
    println!("uid: {}, CapEff: {effective:x?}", getuid().as_raw());
    if getuid().is_root() || effective != Some(0) {
        println!("FAIL: job kept privileges, so no probe ran");
        return ExitCode::FAILURE;
    }

    let flag = fs::read_to_string("/sys/block/nvme0n1/ro").unwrap_or_default();
    let mut passed = flag.trim() == "1";
    println!("{} sysfs ro flag: {:?}", verdict(passed), flag.trim());

    let disk = open(
        "/dev/nvme0n1",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    );
    let clear = disk.and_then(|disk| {
        // SAFETY: BLKROSET reads one int from the value `Setter` holds for the call.
        unsafe { ioctl(&disk, Setter::<BLKROSET, i32>::new(0)) }
    });
    passed &= report("BLKROSET 0", clear);

    let controller = open(
        "/dev/nvme0",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    );
    let passthrough = controller.and_then(|controller| {
        let mut command = [0u8; 72];
        command[0] = NVME_ADMIN_IDENTIFY;
        // SAFETY: the buffer is the size the opcode declares. With no data pointer, the
        // kernel has nothing else to touch, and it checks capabilities first.
        unsafe {
            ioctl(
                &controller,
                Updater::<NVME_IOCTL_ADMIN_CMD, [u8; 72]>::new(&mut command),
            )
        }
    });
    passed &= report("NVMe admin passthrough", passthrough);

    let flag = fs::read_to_string("/sys/block/nvme0n1/ro").unwrap_or_default();
    passed &= flag.trim() == "1";
    println!(
        "{} sysfs ro flag after probes: {:?}",
        verdict(flag.trim() == "1"),
        flag.trim()
    );
    println!("{}", if passed { "PASS" } else { "FAIL" });
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn report(name: &str, result: rustix::io::Result<()>) -> bool {
    let denied = matches!(result, Err(Errno::ACCESS | Errno::PERM));
    println!("{} {name}: {result:?}", verdict(denied));
    denied
}

fn verdict(passed: bool) -> &'static str {
    if passed { "ok  " } else { "FAIL" }
}
