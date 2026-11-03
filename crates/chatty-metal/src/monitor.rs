//! Samples machine load and draws it, htop style, on the attached monitor. This is about
//! 1 Hz of small reads, and only pixels that changed are sent to the GPU.

use crate::{Error, Result, invalid, plan::GIB};
use rustix::{
    fd::OwnedFd,
    fs::{Mode, OFlags, open},
    io::pread,
    ioctl::{Opcode, Updater, ioctl, opcode},
    mm::{MapFlags, ProtFlags, mmap, munmap},
};
use std::{
    ffi::c_void,
    fmt::{self, Write as _},
    fs, io, mem,
    path::Path,
    ptr, slice, thread,
    time::Duration,
};

const DEVICE: &str = "/sys/class/drm/card0/device";
const CARD: &str = "/dev/dri/card0";
const INTERVAL: Duration = Duration::from_secs(1);
const GLYPH_WIDTH: usize = 5;
const GLYPH_HEIGHT: usize = 7;
const CELL_WIDTH: usize = GLYPH_WIDTH + 1;
const CELL_HEIGHT: usize = GLYPH_HEIGHT + 3;
const COLUMNS: usize = 78;
const ROWS: usize = 19;
const CANVAS_WIDTH: usize = COLUMNS * CELL_WIDTH;
const CANVAS_HEIGHT: usize = ROWS * CELL_HEIGHT;
const MAX_SCALE: usize = 8;
const LABEL_COLUMNS: usize = 5;
const VALUE_COLUMNS: usize = 17;

const fn rgb565(rgb: u32) -> u16 {
    let r = ((rgb >> 19) & 0x1F) as u16;
    let g = ((rgb >> 10) & 0x3F) as u16;
    let b = ((rgb >> 3) & 0x1F) as u16;
    (r << 11) | (g << 5) | b
}

/// 8-bpp palette indices. Empty meter space stays `BACKGROUND` (0 = pixels off).
const BACKGROUND: u8 = 0;
const TEXT: u8 = 1;
const DIM: u8 = 2;
const GREEN: u8 = 3;
const YELLOW: u8 = 4;
const RED: u8 = 5;
const BLUE: u8 = 6;

const PALETTE_RGB: [u32; 7] = [
    0x0000_0000,
    0x00D8_DEE9,
    0x0078_8290,
    0x0050_C878,
    0x00E8_C040,
    0x00E8_5050,
    0x0058_A8E8,
];

const PALETTE_565: [u16; 7] = [
    rgb565(PALETTE_RGB[0]),
    rgb565(PALETTE_RGB[1]),
    rgb565(PALETTE_RGB[2]),
    rgb565(PALETTE_RGB[3]),
    rgb565(PALETTE_RGB[4]),
    rgb565(PALETTE_RGB[5]),
    rgb565(PALETTE_RGB[6]),
];

/// Cumulative busy and total jiffies for one logical CPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuTime {
    pub busy: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// Busy percent per logical CPU.
    pub cpus: Vec<u8>,
    pub ram_used: u64,
    /// Buffers and reclaimable cache, which are not counted in `ram_used`.
    pub ram_cache: u64,
    pub ram_total: u64,
    pub gpu_busy: Option<u8>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub gtt_used: Option<u64>,
    pub gtt_total: Option<u64>,
    pub gpu_temp_c: Option<u64>,
    pub gpu_power_w: Option<u64>,
    pub gpu_clock_mhz: Option<u64>,
    pub memory_clock_mhz: Option<u64>,
    pub fan_rpm: Option<u64>,
    pub uptime_seconds: u64,
}

/// Per-CPU lines of `/proc/stat`, in CPU order. Idle and iowait count as not busy.
pub fn parse_cpu_times(stat: &str) -> Vec<CpuTime> {
    let mut out = Vec::new();
    parse_cpu_times_into(stat, &mut out);
    out
}

fn parse_cpu_times_into(stat: &str, out: &mut Vec<CpuTime>) {
    out.clear();
    for line in stat.lines() {
        if !line.starts_with("cpu") || line.starts_with("cpu ") {
            continue;
        }
        // Guest time is already included in user and nice, so stop at steal.
        let mut total = 0u64;
        let mut idle = 0u64;
        for (index, value) in line
            .split_ascii_whitespace()
            .skip(1)
            .filter_map(|word| word.parse::<u64>().ok())
            .take(8)
            .enumerate()
        {
            total = total.saturating_add(value);
            if index == 3 || index == 4 {
                idle = idle.saturating_add(value);
            }
        }
        out.push(CpuTime {
            busy: total - idle.min(total),
            total,
        });
    }
}

pub fn busy_percent(previous: CpuTime, current: CpuTime) -> u8 {
    let total = current.total.saturating_sub(previous.total);
    let busy = current.busy.saturating_sub(previous.busy);
    (busy.min(total) * 100).checked_div(total).unwrap_or(0) as u8
}

/// (used, cache, total) bytes from `/proc/meminfo`, split as htop and procps `free` do.
/// `MemAvailable` is not used because it also subtracts the kernel's free page reserves,
/// which overstates use by hundreds of MiB on an idle machine.
pub fn parse_memory(meminfo: &str) -> (u64, u64, u64) {
    let field = |name: &str| {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|value| value.split_ascii_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |kib| kib.saturating_mul(1024))
    };
    let total = field("MemTotal");
    let free = field("MemFree");
    let buffers = field("Buffers");
    let cached = field("Cached");
    let reclaimable = field("SReclaimable");
    // Cached includes Shmem, which cannot be dropped, so it moves back to used.
    let shared = field("Shmem");
    let unused = free + buffers + cached + reclaimable;
    let used = total
        .checked_sub(unused)
        .unwrap_or(total.saturating_sub(free));
    (
        (used + shared).min(total),
        buffers + (cached + reclaimable).saturating_sub(shared),
        total,
    )
}

fn open_read(path: impl AsRef<Path>) -> Option<OwnedFd> {
    open(
        path.as_ref(),
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()
}

fn pread_str<'a>(fd: &Option<OwnedFd>, buffer: &'a mut [u8]) -> &'a str {
    let Some(fd) = fd else {
        return "";
    };
    let len = pread(fd, &mut *buffer, 0).unwrap_or(0);
    std::str::from_utf8(&buffer[..len]).unwrap_or("")
}

fn pread_number(fd: &Option<OwnedFd>) -> Option<u64> {
    let mut buffer = [0u8; 64];
    pread_str(fd, &mut buffer).trim().parse().ok()
}

struct Sampler {
    stat: Option<OwnedFd>,
    meminfo: Option<OwnedFd>,
    uptime: Option<OwnedFd>,
    gpu_busy: Option<OwnedFd>,
    vram_used: Option<OwnedFd>,
    vram_total: Option<u64>,
    gtt_used: Option<OwnedFd>,
    gtt_total: Option<u64>,
    temp: Option<OwnedFd>,
    power: Option<OwnedFd>,
    gpu_clock: Option<OwnedFd>,
    memory_clock: Option<OwnedFd>,
    fan: Option<OwnedFd>,
    previous: Vec<CpuTime>,
    current: Vec<CpuTime>,
}

impl Sampler {
    fn new() -> Self {
        let device = Path::new(DEVICE);
        let hwmon = fs::read_dir(device.join("hwmon"))
            .ok()
            .and_then(|mut dir| dir.next()?.ok())
            .map(|entry| entry.path());
        let sensor = |name: &str| hwmon.as_ref().and_then(|dir| open_read(dir.join(name)));
        Self {
            stat: open_read("/proc/stat"),
            meminfo: open_read("/proc/meminfo"),
            uptime: open_read("/proc/uptime"),
            gpu_busy: open_read(device.join("gpu_busy_percent")),
            vram_used: open_read(device.join("mem_info_vram_used")),
            vram_total: pread_number(&open_read(device.join("mem_info_vram_total"))),
            gtt_used: open_read(device.join("mem_info_gtt_used")),
            gtt_total: pread_number(&open_read(device.join("mem_info_gtt_total"))),
            temp: sensor("temp1_input"),
            power: sensor("power1_average").or_else(|| sensor("power1_input")),
            gpu_clock: sensor("freq1_input"),
            memory_clock: sensor("freq2_input"),
            fan: sensor("fan1_input"),
            previous: Vec::new(),
            current: Vec::new(),
        }
    }

    fn sample_into(&mut self, sample: &mut Sample) {
        let mut buffer = [0u8; 4096];
        parse_cpu_times_into(pread_str(&self.stat, &mut buffer), &mut self.current);
        sample.cpus.clear();
        for (cpu, &now) in self.current.iter().enumerate() {
            let prev = self.previous.get(cpu).copied().unwrap_or(now);
            sample.cpus.push(busy_percent(prev, now));
        }
        mem::swap(&mut self.previous, &mut self.current);

        let (ram_used, ram_cache, ram_total) = parse_memory(pread_str(&self.meminfo, &mut buffer));
        let mut up_buf = [0u8; 64];
        let uptime = pread_str(&self.uptime, &mut up_buf);

        sample.ram_used = ram_used;
        sample.ram_cache = ram_cache;
        sample.ram_total = ram_total;
        sample.gpu_busy = pread_number(&self.gpu_busy).map(|percent| percent.min(100) as u8);
        sample.vram_used = pread_number(&self.vram_used);
        sample.vram_total = self.vram_total;
        sample.gtt_used = pread_number(&self.gtt_used);
        sample.gtt_total = self.gtt_total;
        sample.gpu_temp_c = pread_number(&self.temp).map(|millidegrees| millidegrees / 1000);
        sample.gpu_power_w = pread_number(&self.power).map(|microwatts| microwatts / 1_000_000);
        sample.gpu_clock_mhz = pread_number(&self.gpu_clock).map(|hertz| hertz / 1_000_000);
        sample.memory_clock_mhz = pread_number(&self.memory_clock).map(|hertz| hertz / 1_000_000);
        sample.fan_rpm = pread_number(&self.fan);
        sample.uptime_seconds = uptime
            .split('.')
            .next()
            .and_then(|seconds| seconds.parse().ok())
            .unwrap_or(0);
    }
}

struct LineBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> LineBuf<N> {
    fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for LineBuf<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut take = text.len().min(N - self.len);
        while !text.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// Rows of a 5x7 glyph, top first, with the left pixel in bit 4.
fn glyph(character: char) -> [u8; GLYPH_HEIGHT] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 27, 17],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        _ => [0; GLYPH_HEIGHT],
    }
}

/// Fixed 1x 8-bpp HUD canvas (`468x190` palette indices, ~87 KiB). Scaling happens only
/// for the dirty horizontal spans sent to VRAM, so RAM use never grows with resolution.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            width: CANVAS_WIDTH,
            height: CANVAS_HEIGHT,
            pixels: vec![BACKGROUND; CANVAS_WIDTH * CANVAS_HEIGHT],
        }
    }

    fn rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u8) {
        let right = x.saturating_add(width).min(self.width);
        for row in y..y.saturating_add(height).min(self.height) {
            let start = row * self.width;
            if x < right {
                self.pixels[start + x..start + right].fill(color);
            }
        }
    }

    /// Draw text at a character cell.
    fn text(&mut self, column: usize, row: usize, text: &str, color: u8) {
        let top = row * CELL_HEIGHT;
        for (index, character) in text.chars().enumerate() {
            let left = (column + index) * CELL_WIDTH;
            if left >= self.width || top >= self.height {
                break;
            }
            for (y, bits) in glyph(character).into_iter().enumerate() {
                let py = top + y;
                if py >= self.height {
                    break;
                }
                let row_start = py * self.width;
                for x in 0..GLYPH_WIDTH {
                    let px = left + x;
                    if px < self.width && bits & (1 << (GLYPH_WIDTH - 1 - x)) != 0 {
                        self.pixels[row_start + px] = color;
                    }
                }
            }
        }
    }

    /// Left, top, width, and height of the bar in a meter that is `columns` wide.
    fn bar(&self, column: usize, row: usize, columns: usize) -> (usize, usize, usize, usize) {
        (
            (column + LABEL_COLUMNS) * CELL_WIDTH,
            row * CELL_HEIGHT,
            columns.saturating_sub(LABEL_COLUMNS + VALUE_COLUMNS) * CELL_WIDTH,
            GLYPH_HEIGHT,
        )
    }

    /// Draw `label [bar] value`, as htop does, with the bar `columns` wide.
    fn meter(
        &mut self,
        column: usize,
        row: usize,
        label: &str,
        percent: u8,
        value: &str,
        columns: usize,
    ) {
        let (_, _, width, _) = self.bar(column, row, columns);
        let filled = width * usize::from(percent.min(100)) / 100;
        self.meter_filled(column, row, label, filled, percent, value, columns);
    }

    #[allow(clippy::too_many_arguments)]
    fn meter_filled(
        &mut self,
        column: usize,
        row: usize,
        label: &str,
        filled: usize,
        percent: u8,
        value: &str,
        columns: usize,
    ) {
        self.text(column, row, label, BLUE);
        let (left, top, width, height) = self.bar(column, row, columns);
        let color = match percent {
            0..=59 => GREEN,
            60..=84 => YELLOW,
            _ => RED,
        };
        self.rect(left, top, filled.min(width), height, color);
        self.text(column + columns - VALUE_COLUMNS + 1, row, value, TEXT);
    }

    pub fn draw(&mut self, sample: &Sample) {
        self.pixels.fill(BACKGROUND);
        let uptime = sample.uptime_seconds;
        let mut title = LineBuf::<48>::new();
        let _ = write!(
            title,
            "CHATTY-METAL  UP {:02}:{:02}:{:02}",
            uptime / 3600,
            uptime / 60 % 60,
            uptime % 60
        );
        self.text(0, 0, title.as_str(), TEXT);

        let half = COLUMNS / 2;
        let cpu_rows = sample.cpus.len().div_ceil(2);
        for (cpu, &percent) in sample.cpus.iter().enumerate() {
            let (column, row) = if cpu < cpu_rows {
                (0, cpu)
            } else {
                (half, cpu - cpu_rows)
            };
            let mut label = LineBuf::<8>::new();
            let mut value = LineBuf::<8>::new();
            let _ = write!(label, "CPU{cpu}");
            let _ = write!(value, "{percent:3}%");
            self.meter(
                column,
                row + 2,
                label.as_str(),
                percent,
                value.as_str(),
                half - 1,
            );
        }

        let mut row = cpu_rows + 3;
        self.memory_meter(row, "RAM", Some(sample.ram_used), Some(sample.ram_total));
        self.cache_segment(row, sample);
        row += 2;
        let busy = sample.gpu_busy.unwrap_or(0);
        let mut gpu_value = LineBuf::<8>::new();
        let label = match sample.gpu_busy {
            Some(value) => {
                let _ = write!(gpu_value, "{value:3}%");
                gpu_value.as_str()
            }
            None => "N/A",
        };
        self.meter(0, row, "GPU", busy, label, COLUMNS);
        row += 1;
        self.memory_meter(row, "VRAM", sample.vram_used, sample.vram_total);
        row += 1;
        self.memory_meter(row, "GTT", sample.gtt_used, sample.gtt_total);
        row += 2;

        let mut details = LineBuf::<96>::new();
        let mut add = |name: &str, value: Option<u64>, unit: &str| {
            if let Some(value) = value {
                let _ = write!(details, "{name} {value}{unit}  ");
            }
        };
        add("TEMP", sample.gpu_temp_c, "C");
        add("POWER", sample.gpu_power_w, "W");
        add("CORE", sample.gpu_clock_mhz, "MHZ");
        add("MEM", sample.memory_clock_mhz, "MHZ");
        add("FAN", sample.fan_rpm, "RPM");
        self.text(0, row, details.as_str(), TEXT);
    }

    /// Extend the RAM bar with cache in a dim color, as htop stacks its memory meter.
    fn cache_segment(&mut self, row: usize, sample: &Sample) {
        let (left, top, width, height) = self.bar(0, row, COLUMNS);
        let used = fill_pixels(sample.ram_used, sample.ram_total, width);
        let cache = fill_pixels(sample.ram_cache, sample.ram_total, width);
        self.rect(left + used, top, cache.min(width - used), height, DIM);
    }

    fn memory_meter(&mut self, row: usize, label: &str, used: Option<u64>, total: Option<u64>) {
        match (used, total) {
            (Some(used), Some(total)) => {
                let (_, _, width, _) = self.bar(0, row, COLUMNS);
                let mut text = LineBuf::<24>::new();
                write_bytes(&mut text, used, total);
                self.meter_filled(
                    0,
                    row,
                    label,
                    fill_pixels(used, total, width),
                    percent(used, total),
                    text.as_str(),
                    COLUMNS,
                );
            }
            _ => self.meter(0, row, label, 0, "N/A", COLUMNS),
        }
    }
}

fn fill_pixels(used: u64, total: u64, width: usize) -> usize {
    (u128::from(used.min(total)) * width as u128)
        .checked_div(u128::from(total))
        .unwrap_or(0) as usize
}

fn percent(used: u64, total: u64) -> u8 {
    (u128::from(used.min(total)) * 100)
        .checked_div(u128::from(total))
        .unwrap_or(0) as u8
}

fn write_bytes(out: &mut impl fmt::Write, used: u64, total: u64) {
    let gib = |value: u64| value as f64 / GIB as f64;
    if used < GIB {
        let _ = write!(out, "{}M/{:.1}G", used >> 20, gib(total));
    } else {
        let _ = write!(out, "{:.1}/{:.1}G", gib(used), gib(total));
    }
}

#[cfg(test)]
fn bytes(used: u64, total: u64) -> String {
    let mut out = String::new();
    write_bytes(&mut out, used, total);
    out
}

fn os(path: &str, error: rustix::io::Errno) -> Error {
    Error::Io(
        path.into(),
        io::Error::from_raw_os_error(error.raw_os_error()),
    )
}

// Layouts from include/uapi/drm/drm_mode.h.
#[repr(C)]
#[derive(Default)]
struct CardResources {
    fb_ids: u64,
    crtc_ids: u64,
    connector_ids: u64,
    encoder_ids: u64,
    fb_count: u32,
    crtc_count: u32,
    connector_count: u32,
    encoder_count: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders: u64,
    modes: u64,
    props: u64,
    prop_values: u64,
    mode_count: u32,
    prop_count: u32,
    encoder_count: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    mode_type: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct GetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct AddFramebuffer {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    depth: u32,
    handle: u32,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct DestroyDumb {
    handle: u32,
}

#[repr(C)]
#[derive(Default)]
struct CrtcLut {
    crtc_id: u32,
    gamma_size: u32,
    red: u64,
    green: u64,
    blue: u64,
}

#[repr(C)]
#[derive(Default)]
struct SetCrtc {
    connectors: u64,
    connector_count: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

const GETRESOURCES: Opcode = opcode::read_write::<CardResources>(b'd', 0xA0);
const GETCRTC: Opcode = opcode::read_write::<SetCrtc>(b'd', 0xA1);
const SETCRTC: Opcode = opcode::read_write::<SetCrtc>(b'd', 0xA2);
const SETGAMMA: Opcode = opcode::read_write::<CrtcLut>(b'd', 0xA5);
const GETENCODER: Opcode = opcode::read_write::<GetEncoder>(b'd', 0xA6);
const GETCONNECTOR: Opcode = opcode::read_write::<GetConnector>(b'd', 0xA7);
const ADDFB: Opcode = opcode::read_write::<AddFramebuffer>(b'd', 0xAE);
const RMFB: Opcode = opcode::read_write::<u32>(b'd', 0xAF);
const CREATE_DUMB: Opcode = opcode::read_write::<CreateDumb>(b'd', 0xB2);
const MAP_DUMB: Opcode = opcode::read_write::<MapDumb>(b'd', 0xB3);
const DESTROY_DUMB: Opcode = opcode::read_write::<DestroyDumb>(b'd', 0xB4);

const CONNECTOR_CONNECTED: u32 = 1;
const MODE_FLAG_INTERLACE: u32 = 1 << 4;
const MODE_FLAG_DOUBLESCAN: u32 = 1 << 5;
/// Far more than any real card has, so a bad reply cannot cause a huge allocation.
const MAX_IDS: u32 = 64;
const MAX_MODES: u32 = 512;
const MAX_GAMMA: u32 = 4096;

/// The smallest progressive mode (by 256-byte pitch-aligned scanout footprint, then
/// refresh rate) that fits the text, leaving maximum VRAM and memory bandwidth to
/// inference. A display too small for the text uses its largest mode.
fn choose_mode(modes: &[ModeInfo]) -> Option<ModeInfo> {
    let area = |mode: &ModeInfo| u64::from(mode.hdisplay) * u64::from(mode.vdisplay);
    let aligned_pixels =
        |mode: &ModeInfo| u64::from(mode.hdisplay).div_ceil(128) * 128 * u64::from(mode.vdisplay);
    modes
        .iter()
        .filter(|mode| {
            usize::from(mode.hdisplay) >= CANVAS_WIDTH
                && usize::from(mode.vdisplay) >= CANVAS_HEIGHT
                && mode.flags & (MODE_FLAG_INTERLACE | MODE_FLAG_DOUBLESCAN) == 0
        })
        .min_by_key(|mode| (aligned_pixels(mode), mode.vrefresh, area(mode), mode.clock))
        .or_else(|| modes.iter().max_by_key(|mode| area(mode)))
        .copied()
}

/// Integer scale (`1..=MAX_SCALE`) and top-left pixel offset that centers the HUD.
fn display_layout(width: usize, height: usize) -> (usize, usize, usize) {
    let scale = (width / CANVAS_WIDTH)
        .min(height / CANVAS_HEIGHT)
        .clamp(1, MAX_SCALE);
    let offset_x = width.saturating_sub(CANVAS_WIDTH * scale) / 2;
    let offset_y = height.saturating_sub(CANVAS_HEIGHT * scale) / 2;
    (scale, offset_x, offset_y)
}

/// Diff `canvas` against `shown`, emit only each row's changed `[first..=last]` span scaled
/// to display coordinates, and swap buffers so `shown` tracks the screen with no copy.
fn blit_changes(
    width: usize,
    height: usize,
    canvas: &mut Canvas,
    shown: &mut Vec<u8>,
    mut write_span: impl FnMut(usize, usize, &[u8]),
) {
    let (scale, offset_x, offset_y) = display_layout(width, height);
    let max_rows = (height.saturating_sub(offset_y) / scale).min(CANVAS_HEIGHT);
    let max_cols = (width.saturating_sub(offset_x) / scale).min(CANVAS_WIDTH);
    if max_cols > 0 {
        let mut scaled = [BACKGROUND; CANVAS_WIDTH * MAX_SCALE];
        for row in 0..max_rows {
            let start = row * CANVAS_WIDTH;
            let new = &canvas.pixels[start..start + max_cols];
            let old = &shown[start..start + max_cols];
            let Some(first) = new.iter().zip(old).position(|(a, b)| a != b) else {
                continue;
            };
            let last = first
                + new[first..]
                    .iter()
                    .zip(&old[first..])
                    .rposition(|(a, b)| a != b)
                    .unwrap_or(0);
            let span = &new[first..=last];
            let scaled_len = span.len() * scale;
            for (index, &pixel) in span.iter().enumerate() {
                scaled[index * scale..(index + 1) * scale].fill(pixel);
            }
            let dst_x = offset_x + first * scale;
            let dst_y = offset_y + row * scale;
            for dy in 0..scale {
                write_span(dst_y + dy, dst_x, &scaled[..scaled_len]);
            }
        }
    }
    mem::swap(&mut canvas.pixels, shown);
}

/// Issue one read-write ioctl. Every caller passes a type matching its opcode, and any
/// pointers inside refer to live buffers sized by the counts beside them.
macro_rules! drm {
    ($card:expr, $opcode:expr, $value:expr) => {
        // SAFETY: see the macro's documentation.
        unsafe { ioctl($card, Updater::<{ $opcode }, _>::new($value)) }
            .map_err(|error| os(CARD, error))
    };
}

enum Scanout {
    /// 8-bpp `DRM_FORMAT_C8` indexed framebuffer driven with our 7-color `SETGAMMA` LUT.
    Indexed(*mut u8),
    /// 16-bpp `DRM_FORMAT_RGB565` fallback for GPUs (such as AMD DCN) without 8-bpp planes.
    Rgb565(*mut u16),
}

/// The scanout buffer of a lit display.
struct Display {
    /// Keeps the device open, and with it the mode set and the mapping's buffer.
    _card: OwnedFd,
    scanout: Scanout,
    stride: usize,
    width: usize,
    height: usize,
}

impl Display {
    /// Light the first connected display. `Ok(None)` means none is attached.
    fn open() -> Result<Option<Self>> {
        let card = open(CARD, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
            .map_err(|error| os(CARD, error))?;

        let mut resources = CardResources::default();
        drm!(&card, GETRESOURCES, &mut resources)?;
        if resources.connector_count > MAX_IDS
            || resources.crtc_count > MAX_IDS
            || resources.encoder_count > MAX_IDS
        {
            return invalid("display reports too many objects");
        }
        let mut connectors = vec![0u32; resources.connector_count as usize];
        let mut crtcs = vec![0u32; resources.crtc_count as usize];
        let mut encoders = vec![0u32; resources.encoder_count as usize];
        resources = CardResources {
            connector_ids: connectors.as_mut_ptr() as u64,
            crtc_ids: crtcs.as_mut_ptr() as u64,
            encoder_ids: encoders.as_mut_ptr() as u64,
            connector_count: connectors.len() as u32,
            crtc_count: crtcs.len() as u32,
            encoder_count: encoders.len() as u32,
            ..CardResources::default()
        };
        drm!(&card, GETRESOURCES, &mut resources)?;

        for &connector_id in &connectors {
            let mut probe = GetConnector {
                connector_id,
                ..GetConnector::default()
            };
            drm!(&card, GETCONNECTOR, &mut probe)?;
            if probe.connection != CONNECTOR_CONNECTED
                || probe.mode_count == 0
                || probe.mode_count > MAX_MODES
            {
                continue;
            }
            let mut modes = vec![ModeInfo::default(); probe.mode_count as usize];
            let mut connector_encoders = vec![0u32; probe.encoder_count.min(MAX_IDS) as usize];
            let mut info = GetConnector {
                connector_id,
                modes: modes.as_mut_ptr() as u64,
                mode_count: modes.len() as u32,
                encoders: connector_encoders.as_mut_ptr() as u64,
                encoder_count: connector_encoders.len() as u32,
                ..GetConnector::default()
            };
            drm!(&card, GETCONNECTOR, &mut info)?;
            modes.truncate(info.mode_count as usize);
            let Some(mode) = choose_mode(&modes) else {
                continue;
            };
            let Some(crtc_id) = find_crtc(&card, &connector_encoders, &crtcs)? else {
                continue;
            };

            // Prefer 8-bpp `C8` scanout; fall back to 16-bpp `RGB565` if the plane rejects C8.
            let (scanout, stride) = match light_mode(&card, connector_id, crtc_id, mode, 8) {
                Ok(opened) => opened,
                Err(_) => light_mode(&card, connector_id, crtc_id, mode, 16)?,
            };
            return Ok(Some(Self {
                _card: card,
                scanout,
                stride,
                width: mode.hdisplay.into(),
                height: mode.vdisplay.into(),
            }));
        }
        Ok(None)
    }

    /// Copy only the changed horizontal spans onto the scanout buffer.
    fn present(&self, canvas: &mut Canvas, shown: &mut Vec<u8>) {
        let mut rgb_row = [0u16; CANVAS_WIDTH * MAX_SCALE];
        blit_changes(self.width, self.height, canvas, shown, |row, col, span| {
            let offset = row * self.stride + col;
            match self.scanout {
                Scanout::Indexed(memory) => {
                    // SAFETY: `blit_changes` bounds `row < self.height` and `col + span.len() <= self.width <= self.stride`.
                    unsafe {
                        ptr::copy_nonoverlapping(span.as_ptr(), memory.add(offset), span.len());
                    }
                }
                Scanout::Rgb565(memory) => {
                    for (dst, &index) in rgb_row[..span.len()].iter_mut().zip(span) {
                        *dst = PALETTE_565.get(usize::from(index)).copied().unwrap_or(0);
                    }
                    // SAFETY: `blit_changes` bounds `row < self.height` and `col + span.len() <= self.width <= self.stride`.
                    unsafe {
                        ptr::copy_nonoverlapping(rgb_row.as_ptr(), memory.add(offset), span.len());
                    }
                }
            }
        });
    }
}

fn light_mode(
    card: &OwnedFd,
    connector_id: u32,
    crtc_id: u32,
    mode: ModeInfo,
    bpp: u32,
) -> Result<(Scanout, usize)> {
    let mut dumb = CreateDumb {
        width: mode.hdisplay.into(),
        height: mode.vdisplay.into(),
        bpp,
        ..CreateDumb::default()
    };
    drm!(card, CREATE_DUMB, &mut dumb)?;

    let mut framebuffer = AddFramebuffer {
        width: dumb.width,
        height: dumb.height,
        pitch: dumb.pitch,
        bpp,
        depth: bpp,
        handle: dumb.handle,
        ..AddFramebuffer::default()
    };
    if let Err(error) = drm!(card, ADDFB, &mut framebuffer) {
        let mut destroy = DestroyDumb {
            handle: dumb.handle,
        };
        let _ = drm!(card, DESTROY_DUMB, &mut destroy);
        return Err(error);
    }

    let mut map = MapDumb {
        handle: dumb.handle,
        ..MapDumb::default()
    };
    if let Err(error) = drm!(card, MAP_DUMB, &mut map) {
        let mut fb_id = framebuffer.fb_id;
        let _ = drm!(card, RMFB, &mut fb_id);
        let mut destroy = DestroyDumb {
            handle: dumb.handle,
        };
        let _ = drm!(card, DESTROY_DUMB, &mut destroy);
        return Err(error);
    }

    let size = dumb.size as usize;
    // SAFETY: the kernel gave this offset for a buffer of `dumb.size` bytes.
    let memory = match unsafe {
        mmap(
            ptr::null_mut::<c_void>(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            card,
            map.offset,
        )
    } {
        Ok(memory) => memory,
        Err(error) => {
            let mut fb_id = framebuffer.fb_id;
            let _ = drm!(card, RMFB, &mut fb_id);
            let mut destroy = DestroyDumb {
                handle: dumb.handle,
            };
            let _ = drm!(card, DESTROY_DUMB, &mut destroy);
            return Err(os(CARD, error));
        }
    };

    // Clear before showing it (`0` is `BACKGROUND` in both 8-bpp `C8` and 16-bpp `RGB565`).
    // SAFETY: the mapping is `size` bytes, and it stays mapped until either `munmap` or exit.
    unsafe { slice::from_raw_parts_mut(memory.cast::<u8>(), size) }.fill(0);

    let mut set = SetCrtc {
        connectors: ptr::from_ref(&connector_id) as u64,
        connector_count: 1,
        crtc_id,
        fb_id: framebuffer.fb_id,
        mode_valid: 1,
        mode,
        ..SetCrtc::default()
    };
    if let Err(error) = drm!(card, SETCRTC, &mut set) {
        // SAFETY: `memory` and `size` came from `mmap` above and are not used again.
        let _ = unsafe { munmap(memory, size) };
        let mut fb_id = framebuffer.fb_id;
        let _ = drm!(card, RMFB, &mut fb_id);
        let mut destroy = DestroyDumb {
            handle: dumb.handle,
        };
        let _ = drm!(card, DESTROY_DUMB, &mut destroy);
        return Err(error);
    }

    if bpp == 8 {
        load_palette_lut(card, crtc_id);
        Ok((Scanout::Indexed(memory.cast()), dumb.pitch as usize))
    } else {
        Ok((
            Scanout::Rgb565(memory.cast()),
            dumb.pitch as usize / size_of::<u16>(),
        ))
    }
}

fn load_palette_lut(card: &OwnedFd, crtc_id: u32) {
    let mut crtc = SetCrtc {
        crtc_id,
        ..SetCrtc::default()
    };
    if drm!(card, GETCRTC, &mut crtc).is_err()
        || crtc.gamma_size == 0
        || crtc.gamma_size > MAX_GAMMA
    {
        return;
    }
    let len = crtc.gamma_size as usize;
    let mut red = vec![0u16; len];
    let mut green = vec![0u16; len];
    let mut blue = vec![0u16; len];
    for (index, &rgb) in PALETTE_RGB.iter().enumerate().take(len) {
        let expand = |byte: u32| ((byte as u16) << 8) | (byte as u16);
        red[index] = expand((rgb >> 16) & 0xFF);
        green[index] = expand((rgb >> 8) & 0xFF);
        blue[index] = expand(rgb & 0xFF);
    }
    let mut lut = CrtcLut {
        crtc_id,
        gamma_size: crtc.gamma_size,
        red: red.as_ptr() as u64,
        green: green.as_ptr() as u64,
        blue: blue.as_ptr() as u64,
    };
    let _ = drm!(card, SETGAMMA, &mut lut);
}

fn find_crtc(card: &OwnedFd, encoders: &[u32], crtcs: &[u32]) -> Result<Option<u32>> {
    for &encoder_id in encoders {
        let mut encoder = GetEncoder {
            encoder_id,
            ..GetEncoder::default()
        };
        drm!(card, GETENCODER, &mut encoder)?;
        if encoder.crtc_id != 0 {
            return Ok(Some(encoder.crtc_id));
        }
        for (index, &crtc) in crtcs.iter().enumerate() {
            if index < 32 && encoder.possible_crtcs & (1 << index) != 0 {
                return Ok(Some(crtc));
            }
        }
    }
    Ok(None)
}

/// Light the display and refresh it until the appliance stops. Returns early, leaving
/// no thread behind, when no display is attached or it cannot be driven.
pub fn run() -> Result<()> {
    let Some(display) = Display::open()? else {
        return invalid("no connected display");
    };
    let mut canvas = Canvas::new();
    let mut shown = vec![BACKGROUND; canvas.pixels.len()];
    let mut sampler = Sampler::new();
    let mut sample = Sample::default();
    loop {
        sampler.sample_into(&mut sample);
        canvas.draw(&sample);
        display.present(&mut canvas, &mut shown);
        thread::sleep(INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        Sample {
            cpus: vec![0, 50, 75, 100],
            ram_used: 8 * GIB,
            ram_total: 64 * GIB,
            gpu_busy: Some(90),
            vram_used: Some(GIB),
            vram_total: Some(24 * GIB),
            gpu_temp_c: Some(55),
            uptime_seconds: 3725,
            ..Sample::default()
        }
    }

    #[test]
    fn struct_sizes_match_the_kernel_abi() {
        assert_eq!(size_of::<CardResources>(), 64);
        assert_eq!(size_of::<GetConnector>(), 80);
        assert_eq!(size_of::<ModeInfo>(), 68);
        assert_eq!(size_of::<GetEncoder>(), 20);
        assert_eq!(size_of::<CreateDumb>(), 32);
        assert_eq!(size_of::<AddFramebuffer>(), 28);
        assert_eq!(size_of::<MapDumb>(), 16);
        assert_eq!(size_of::<DestroyDumb>(), 4);
        assert_eq!(size_of::<CrtcLut>(), 32);
        assert_eq!(size_of::<SetCrtc>(), 104);
    }

    #[test]
    fn parses_per_cpu_times_and_skips_the_total_line() {
        let stat =
            "cpu  9 9 9 9 9 9 9 9 9 9\ncpu0 10 0 10 70 10 0 0 0 5 0\ncpu1 1 2 3 4\nintr 1 2\n";
        assert_eq!(
            parse_cpu_times(stat),
            vec![
                CpuTime {
                    busy: 20,
                    total: 100
                },
                CpuTime { busy: 6, total: 10 }
            ]
        );
    }

    #[test]
    fn busy_percent_uses_the_change_and_survives_odd_counters() {
        let previous = CpuTime {
            busy: 100,
            total: 1000,
        };
        assert_eq!(
            busy_percent(
                previous,
                CpuTime {
                    busy: 150,
                    total: 1100
                }
            ),
            50
        );
        assert_eq!(busy_percent(previous, previous), 0);
        assert_eq!(busy_percent(previous, CpuTime { busy: 0, total: 0 }), 0);
        assert_eq!(
            busy_percent(
                previous,
                CpuTime {
                    busy: 500,
                    total: 1001
                }
            ),
            100
        );
    }

    #[test]
    fn parses_memory_as_htop_does() {
        // Read from the idle appliance. MemTotal - MemAvailable would claim 737,216 kB.
        let meminfo = "MemTotal:       65780832 kB\nMemFree:        64829512 kB\n\
                       MemAvailable:   65043616 kB\nBuffers:               4 kB\n\
                       Cached:           751880 kB\nShmem:              5108 kB\n\
                       SReclaimable:       6356 kB\n";
        assert_eq!(
            parse_memory(meminfo),
            (198_188 * 1024, 753_132 * 1024, 65_780_832 * 1024)
        );
        assert_eq!(parse_memory(""), (0, 0, 0));
    }

    #[test]
    fn memory_survives_counters_that_exceed_the_total() {
        // Fall back to total - free when the page cache counters overlap, as htop does.
        let meminfo = "MemTotal: 1000 kB\nMemFree: 900 kB\nCached: 500 kB\n";
        assert_eq!(parse_memory(meminfo), (100 * 1024, 500 * 1024, 1000 * 1024));
    }

    #[test]
    fn ram_bar_stacks_cache_after_used() {
        let mut canvas = Canvas::new();
        let sample = Sample {
            ram_used: 25,
            ram_cache: 50,
            ram_total: 100,
            ..Sample::default()
        };
        canvas.cache_segment(0, &sample);
        let (left, top, width, _) = canvas.bar(0, 0, COLUMNS);
        let row = &canvas.pixels[top * canvas.width..(top + 1) * canvas.width];
        assert_eq!(row[left + width / 4 - 1], BACKGROUND);
        assert_eq!(row[left + width / 4], DIM);
        assert_eq!(row[left + width * 3 / 4 - 1], DIM);
        assert_eq!(row[left + width * 3 / 4], BACKGROUND);
    }

    #[test]
    fn prefers_the_cheapest_mode_that_fits_the_text() {
        let mode = |hdisplay, vdisplay, vrefresh, flags| ModeInfo {
            hdisplay,
            vdisplay,
            vrefresh,
            flags,
            ..ModeInfo::default()
        };
        // Both 640x480 and 720x400 align to 307,200 pitch-aligned pixels on GPU; 640x480@60
        // wins on lower refresh rate (60 Hz vs 70 Hz), saving scanout bandwidth.
        let chosen = choose_mode(&[
            mode(3840, 2160, 60, 0),
            mode(1920, 1080, 60, 0),
            mode(1280, 720, 50, 0),
            mode(720, 400, 70, 0),
            mode(640, 480, 75, 0),
            mode(640, 480, 60, 0),
            mode(640, 480, 30, MODE_FLAG_INTERLACE),
            mode(320, 240, 60, MODE_FLAG_DOUBLESCAN),
            mode(320, 240, 60, 0),
        ])
        .unwrap();
        assert_eq!(
            (
                chosen.hdisplay,
                chosen.vdisplay,
                chosen.vrefresh,
                chosen.flags
            ),
            (640, 480, 60, 0)
        );

        // Even a narrow 480x620 mode fits the 468x190 HUD when it is the smallest mode.
        let chosen = choose_mode(&[mode(800, 600, 60, 0), mode(480, 620, 60, 0)]).unwrap();
        assert_eq!((chosen.hdisplay, chosen.vdisplay), (480, 620));

        let chosen = choose_mode(&[mode(320, 240, 60, 0), mode(400, 300, 60, 0)]).unwrap();
        assert_eq!(chosen.hdisplay, 400);
        assert!(choose_mode(&[]).is_none());
    }

    #[test]
    fn scales_text_to_the_display_and_fits_every_row() {
        let mut canvas = Canvas::new();
        let mut sample = sample();
        sample.cpus = vec![100; 16];
        canvas.draw(&sample);
        assert_eq!(canvas.pixels.len(), CANVAS_WIDTH * CANVAS_HEIGHT);

        for (width, height) in [
            (480, 620),
            (640, 480),
            (720, 400),
            (1280, 720),
            (1920, 1080),
            (3840, 2160),
            (1, 1),
        ] {
            let mut screen = vec![BACKGROUND; width * height];
            let mut shown = vec![BACKGROUND; canvas.pixels.len()];
            blit_changes(width, height, &mut canvas, &mut shown, |row, col, span| {
                let start = row * width + col;
                screen[start..start + span.len()].copy_from_slice(span);
            });
            canvas.pixels.copy_from_slice(&shown);
        }
        assert_eq!(display_layout(3840, 2160), (8, 48, 320));
        assert_eq!(display_layout(1920, 1080), (4, 24, 160));
        assert_eq!(display_layout(1280, 720), (2, 172, 170));
        assert_eq!(display_layout(720, 400), (1, 126, 105));
        assert_eq!(display_layout(640, 480), (1, 86, 145));
        assert_eq!(display_layout(480, 620), (1, 6, 215));
    }

    #[test]
    fn dirty_span_blit_matches_full_render_and_skips_unchanged_pixels() {
        let (width, height) = (1280, 720);
        let mut screen = vec![BACKGROUND; width * height];
        let mut canvas = Canvas::new();
        let mut shown = vec![BACKGROUND; canvas.pixels.len()];

        let mut current = sample();
        canvas.draw(&current);
        blit_changes(width, height, &mut canvas, &mut shown, |row, col, span| {
            let start = row * width + col;
            screen[start..start + span.len()].copy_from_slice(span);
        });

        // Identical redraw sends zero pixels.
        let mut written = 0usize;
        canvas.draw(&current);
        blit_changes(width, height, &mut canvas, &mut shown, |_, _, span| {
            written += span.len();
        });
        assert_eq!(written, 0);

        // One-second uptime tick sends only the changed digit span, matching a fresh blit.
        current.uptime_seconds += 1;
        canvas.draw(&current);
        blit_changes(width, height, &mut canvas, &mut shown, |row, col, span| {
            written += span.len();
            let start = row * width + col;
            screen[start..start + span.len()].copy_from_slice(span);
        });
        assert!(written > 0 && written <= 140, "written={written}");

        let mut fresh_screen = vec![BACKGROUND; width * height];
        let mut fresh_canvas = Canvas::new();
        let mut fresh_shown = vec![BACKGROUND; fresh_canvas.pixels.len()];
        fresh_canvas.draw(&current);
        blit_changes(
            width,
            height,
            &mut fresh_canvas,
            &mut fresh_shown,
            |row, col, span| {
                let start = row * width + col;
                fresh_screen[start..start + span.len()].copy_from_slice(span);
            },
        );
        assert_eq!(screen, fresh_screen);
    }

    #[test]
    fn meter_fills_in_proportion_and_colors_by_load() {
        let mut canvas = Canvas::new();
        canvas.meter(0, 0, "X", 50, "", COLUMNS);
        let row = &canvas.pixels[..canvas.width];
        assert_eq!(
            row.iter().filter(|&&pixel| pixel == GREEN).count(),
            (COLUMNS - 22) * CELL_WIDTH / 2
        );

        let mut canvas = Canvas::new();
        canvas.meter(0, 0, "X", 100, "", COLUMNS);
        assert!(canvas.pixels.contains(&RED));

        let mut canvas = Canvas::new();
        canvas.meter(0, 0, "", 0, "", COLUMNS);
        assert!(canvas.pixels.iter().all(|&pixel| pixel == BACKGROUND));
    }

    #[test]
    fn every_character_the_screen_uses_has_a_glyph() {
        for character in
            "CHATTY-METAL UP GPU VRAM GTT RAM CPU TEMP POWER CORE MEM FAN MHZ RPM N/A 0123456789.:%/G"
                .chars()
        {
            if character != ' ' {
                assert_ne!(glyph(character), [0; GLYPH_HEIGHT], "{character}");
            }
        }
    }

    #[test]
    fn formats_small_use_in_mebibytes() {
        assert_eq!(bytes(35 << 20, 24 * GIB), "35M/24.0G");
        assert_eq!(bytes(2 * GIB, 64 * GIB), "2.0/64.0G");
    }

    #[test]
    fn missing_gpu_data_still_draws() {
        let mut canvas = Canvas::new();
        canvas.draw(&Sample::default());
        canvas.draw(&Sample {
            cpus: vec![10; 64],
            ..sample()
        });
    }
}
