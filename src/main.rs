use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::{Duration, Instant};

use std::os::raw::{c_int, c_long, c_ulong, c_void};

const FBIOGET_VSCREENINFO: c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: c_ulong = 0x4602;
const PROT_READ: c_int = 0x1;
const PROT_WRITE: c_int = 0x2;
const MAP_SHARED: c_int = 0x01;
const MAP_FAILED: *mut c_void = -1isize as *mut c_void;
const LOGICAL_WIDTH: usize = 1280;
const LOGICAL_HEIGHT: usize = 480;
const PHYSICAL_WIDTH: usize = 600;
const PHYSICAL_HEIGHT: usize = 1280;
const DASHBOARD_TICK_SECS: u64 = 3;
const CHAIN_POLL_SECS: u64 = 30;
const HISTORY_SAMPLES: usize = 80;

#[inline]
fn physical_position(logical_x: usize, logical_y: usize) -> (usize, usize) {
    debug_assert!(logical_x < LOGICAL_WIDTH && logical_y < LOGICAL_HEIGHT);
    (PHYSICAL_HEIGHT - 1 - logical_x, logical_y)
}

// RetroDeck dashboard palette: surface, controls, Wi-Fi, volume, title, accent.
const BLACK: Color = Color::new(0, 0, 0);
const PANEL: Color = Color::new(28, 28, 28);
const PANEL_ALT: Color = Color::new(48, 48, 48);
const WHITE: Color = Color::new(238, 238, 238);
const MUTED: Color = Color::new(148, 148, 148);
const CYAN: Color = Color::new(95, 135, 175);
const GREEN: Color = Color::new(135, 175, 135);
const AMBER: Color = Color::new(255, 255, 175);
const RED: Color = Color::new(175, 135, 135);
const BLUE: Color = Color::new(254, 108, 39);

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: c_long,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: usize,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: usize,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

#[derive(Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
}

impl Color {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

struct Framebuffer {
    _file: File,
    ptr: *mut u8,
    len: usize,
    width: usize,
    height: usize,
    line_length: usize,
    bits_per_pixel: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    canvas: Vec<u16>,
    row: Vec<u16>,
}

impl Framebuffer {
    fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("open {path}: {e}"))?;
        let fd = file.as_raw_fd();
        let mut var = FbVarScreeninfo::default();
        let mut fix = FbFixScreeninfo::default();
        let var_result = unsafe { ioctl(fd, FBIOGET_VSCREENINFO, &mut var) };
        let fix_result = unsafe { ioctl(fd, FBIOGET_FSCREENINFO, &mut fix) };
        if var_result < 0 || fix_result < 0 {
            return Err("framebuffer ioctl failed".to_string());
        }
        let stride = fix.line_length as usize;
        let len = fix.smem_len as usize;
        let required = stride.saturating_mul(PHYSICAL_HEIGHT);
        if var.xres as usize != PHYSICAL_WIDTH
            || var.yres as usize != PHYSICAL_HEIGHT
            || var.bits_per_pixel != 16
            || stride < PHYSICAL_WIDTH * 2
            || !stride.is_multiple_of(2)
            || len < required
            || var.red.offset != 11
            || var.red.length != 5
            || var.green.offset != 5
            || var.green.length != 6
            || var.blue.offset != 0
            || var.blue.length != 5
            || var.transp.length != 0
        {
            return Err("unsupported framebuffer; expected rotated 600x1280 RGB565".to_string());
        }
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == MAP_FAILED {
            return Err("framebuffer mmap failed".to_string());
        }
        Ok(Self {
            _file: file,
            ptr: ptr.cast(),
            len,
            width: LOGICAL_WIDTH,
            height: LOGICAL_HEIGHT,
            line_length: stride,
            bits_per_pixel: var.bits_per_pixel,
            red: var.red,
            green: var.green,
            blue: var.blue,
            transp: var.transp,
            canvas: vec![0; LOGICAL_WIDTH * LOGICAL_HEIGHT],
            row: vec![0; LOGICAL_HEIGHT],
        })
    }

    fn pack_channel(value: u8, field: FbBitfield) -> u32 {
        if field.length == 0 {
            return 0;
        }
        let max = if field.length >= 32 {
            u32::MAX
        } else {
            (1u32 << field.length) - 1
        };
        let scaled = ((value as u64 * max as u64 + 127) / 255) as u32;
        scaled << field.offset
    }

    fn pixel_value(&self, color: Color) -> u16 {
        (Self::pack_channel(color.r, self.red)
            | Self::pack_channel(color.g, self.green)
            | Self::pack_channel(color.b, self.blue)
            | Self::pack_channel(255, self.transp)) as u16
    }

    fn pixel(&mut self, x: usize, y: usize, color: Color) {
        if x < self.width && y < self.height {
            self.canvas[y * self.width + x] = self.pixel_value(color);
        }
    }

    fn fill(&mut self, color: Color) {
        let value = self.pixel_value(color);
        self.canvas.fill(value);
    }

    fn rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        for yy in y.min(self.height)..y_end {
            for xx in x.min(self.width)..x_end {
                self.pixel(xx, yy, color);
            }
        }
    }

    fn text(&mut self, x: usize, y: usize, text: &str, scale: usize, color: Color) {
        let mut cursor = x;
        for byte in text.bytes() {
            self.glyph(cursor, y, byte, scale, color);
            cursor = cursor.saturating_add(6usize.saturating_mul(scale));
            if cursor >= self.width {
                break;
            }
        }
    }

    fn glyph(&mut self, x: usize, y: usize, byte: u8, scale: usize, color: Color) {
        let glyph = glyph(byte);
        for (column, bits) in glyph.iter().enumerate() {
            for row in 0..7usize {
                if bits & (1 << row) == 0 {
                    continue;
                }
                self.rect(
                    x.saturating_add(column.saturating_mul(scale)),
                    y.saturating_add(row.saturating_mul(scale)),
                    scale,
                    scale,
                    color,
                );
            }
        }
    }

    fn publish(&mut self) {
        for logical_x in 0..LOGICAL_WIDTH {
            let (physical_row, _) = physical_position(logical_x, 0);
            let destination =
                unsafe { self.ptr.add(physical_row * self.line_length).cast::<u16>() };
            for logical_y in 0..LOGICAL_HEIGHT {
                self.row[logical_y] = self.canvas[logical_y * LOGICAL_WIDTH + logical_x];
            }
            unsafe {
                std::ptr::copy_nonoverlapping(self.row.as_ptr(), destination, LOGICAL_HEIGHT);
            }
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.ptr.cast(), self.len);
        }
    }
}

struct Machine {
    uptime_seconds: u64,
    load: String,
    load_one: f32,
    mem_available: u64,
    mem_total: u64,
    swap_used: u64,
    swap_total: u64,
    disk_mounted: bool,
}

#[derive(Clone)]
struct Mempool {
    available: bool,
    entries: u64,
    bytes: u64,
    usage: u64,
    limit: u64,
    min_fee: f64,
    unbroadcast: u64,
}

impl Mempool {
    fn unavailable() -> Self {
        Self {
            available: false,
            entries: 0,
            bytes: 0,
            usage: 0,
            limit: 0,
            min_fee: 0.0,
            unbroadcast: 0,
        }
    }

    fn from_rpc(body: &str) -> Self {
        Self {
            available: true,
            entries: json_u64(body, "size").unwrap_or(0),
            bytes: json_u64(body, "bytes").unwrap_or(0),
            usage: json_u64(body, "usage").unwrap_or(0),
            limit: json_u64(body, "maxmempool").unwrap_or(0),
            min_fee: json_f64(body, "mempoolminfee").unwrap_or(0.0),
            unbroadcast: json_u64(body, "unbroadcastcount").unwrap_or(0),
        }
    }
}

#[derive(Clone)]
struct Node {
    rpc_ok: bool,
    process_alive: bool,
    resident_kib: u64,
    blocks: u64,
    headers: u64,
    verification: f64,
    ibd: bool,
    pruned: bool,
    connections: u64,
    network_active: bool,
    disk_bytes: u64,
    mempool: Mempool,
}

struct Snapshot {
    machine: Machine,
    node: Node,
}

struct Dashboard {
    network: NetworkCache,
    chain: ChainCache,
    history: MetricHistory,
}

impl Dashboard {
    fn new() -> Self {
        Self {
            network: NetworkCache::new(),
            chain: ChainCache::new(),
            history: MetricHistory::new(),
        }
    }

    fn collect(&mut self) -> Snapshot {
        let snapshot = Snapshot {
            machine: Machine::collect(),
            node: self.chain.collect(&mut self.network),
        };
        self.history.push(&snapshot);
        snapshot
    }
}

struct MetricHistory {
    load: Vec<u8>,
    memory: Vec<u8>,
    swap: Vec<u8>,
}

impl MetricHistory {
    fn new() -> Self {
        Self {
            load: Vec::with_capacity(HISTORY_SAMPLES),
            memory: Vec::with_capacity(HISTORY_SAMPLES),
            swap: Vec::with_capacity(HISTORY_SAMPLES),
        }
    }

    fn push(&mut self, snapshot: &Snapshot) {
        push_sample(&mut self.load, (snapshot.machine.load_one * 25.0) as u8);
        push_sample(
            &mut self.memory,
            percent(
                snapshot
                    .machine
                    .mem_total
                    .saturating_sub(snapshot.machine.mem_available),
                snapshot.machine.mem_total,
            ),
        );
        push_sample(
            &mut self.swap,
            percent(snapshot.machine.swap_used, snapshot.machine.swap_total),
        );
    }
}

fn push_sample(samples: &mut Vec<u8>, value: u8) {
    if samples.len() == HISTORY_SAMPLES {
        samples.remove(0);
    }
    samples.push(value.min(100));
}

fn percent(used: u64, total: u64) -> u8 {
    used.saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8
}

impl Machine {
    fn collect() -> Self {
        let uptime_seconds = read_text("/proc/uptime")
            .and_then(|s| {
                s.split_whitespace()
                    .next()
                    .map(|v| v.parse::<f64>().unwrap_or(0.0) as u64)
            })
            .unwrap_or(0);
        let load = read_text("/proc/loadavg")
            .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|| "- - -".to_string());
        let load_one = load
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.0);
        let meminfo = read_text("/proc/meminfo").unwrap_or_default();
        let mem_total = meminfo_value(&meminfo, "MemTotal");
        let mem_available = meminfo_value(&meminfo, "MemAvailable");
        let swap_total = meminfo_value(&meminfo, "SwapTotal");
        let swap_free = meminfo_value(&meminfo, "SwapFree");
        let disk_mounted = read_text("/proc/mounts")
            .map(|s| {
                s.lines()
                    .any(|line| line.split_whitespace().nth(1) == Some("/mnt/bitcoin-node"))
            })
            .unwrap_or(false);
        Self {
            uptime_seconds,
            load,
            load_one,
            mem_available,
            mem_total,
            swap_used: swap_total.saturating_sub(swap_free),
            swap_total,
            disk_mounted,
        }
    }
}

struct NetworkCache {
    connections: u64,
    active: bool,
    next_poll: Instant,
}

impl NetworkCache {
    fn new() -> Self {
        Self {
            connections: 0,
            active: false,
            next_poll: Instant::now(),
        }
    }

    fn refresh(&mut self, rpc: &Rpc) {
        let now = Instant::now();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + Duration::from_secs(120);
        if let Ok(network) = rpc.call("getnetworkinfo") {
            self.connections = json_u64(&network, "connections").unwrap_or(0);
            self.active = json_bool(&network, "networkactive").unwrap_or(false);
        }
    }
}

struct ChainCache {
    snapshot: Option<Node>,
    next_poll: Instant,
}

impl ChainCache {
    fn new() -> Self {
        Self {
            snapshot: None,
            next_poll: Instant::now(),
        }
    }

    fn collect(&mut self, network_cache: &mut NetworkCache) -> Node {
        let now = Instant::now();
        if now >= self.next_poll {
            self.next_poll = now + Duration::from_secs(CHAIN_POLL_SECS);
            self.snapshot = Some(Node::poll(network_cache));
        }
        let mut node = self.snapshot.clone().unwrap_or_else(Node::unavailable);
        let core_rss = core_process_rss_kib();
        node.process_alive = core_rss.is_some();
        node.resident_kib = core_rss.unwrap_or(0);
        if !node.process_alive {
            node.rpc_ok = false;
        }
        node
    }
}

fn status_kib(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key)
            .then(|| value.split_whitespace().next()?.parse::<u64>().ok())
            .flatten()
    })
}

fn core_process_rss_kib() -> Option<u64> {
    let entries = std::fs::read_dir("/proc").ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let name = entry.file_name();
        if !name
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let command = std::fs::read_to_string(entry.path().join("cmdline")).ok()?;
        if !command.contains("/bitcoind") {
            return None;
        }
        let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
        status_kib(&status, "VmRSS")
    })
}

impl Node {
    fn unavailable() -> Self {
        let core_rss = core_process_rss_kib();
        Self {
            rpc_ok: false,
            process_alive: core_rss.is_some(),
            resident_kib: core_rss.unwrap_or(0),
            blocks: 0,
            headers: 0,
            verification: 0.0,
            ibd: true,
            pruned: false,
            connections: 0,
            network_active: false,
            disk_bytes: 0,
            mempool: Mempool::unavailable(),
        }
    }

    fn poll(network_cache: &mut NetworkCache) -> Self {
        let cookie = match read_text("/mnt/bitcoin-node/.cookie") {
            Some(value) => value.trim().to_string(),
            None => return Self::unavailable(),
        };
        let rpc = Rpc::new(cookie);
        let chain = match rpc.call("getblockchaininfo") {
            Ok(body) => body,
            Err(_) => return Self::unavailable(),
        };
        let mempool = rpc
            .call("getmempoolinfo")
            .map(|body| Mempool::from_rpc(&body))
            .unwrap_or_else(|_| Mempool::unavailable());
        network_cache.refresh(&rpc);
        Self {
            rpc_ok: true,
            process_alive: true,
            resident_kib: core_process_rss_kib().unwrap_or(0),
            blocks: json_u64(&chain, "blocks").unwrap_or(0),
            headers: json_u64(&chain, "headers").unwrap_or(0),
            verification: json_f64(&chain, "verificationprogress").unwrap_or(0.0),
            ibd: json_bool(&chain, "initialblockdownload").unwrap_or(true),
            pruned: json_bool(&chain, "pruned").unwrap_or(false),
            connections: network_cache.connections,
            network_active: network_cache.active,
            disk_bytes: json_u64(&chain, "size_on_disk").unwrap_or(0),
            mempool,
        }
    }
}

struct Rpc {
    cookie: String,
}

impl Rpc {
    fn new(cookie: String) -> Self {
        Self { cookie }
    }

    fn call(&self, method: &str) -> Result<String, String> {
        let mut stream = TcpStream::connect_timeout(
            &"127.0.0.1:8332"
                .parse::<std::net::SocketAddr>()
                .map_err(|e| e.to_string())?,
            Duration::from_millis(350),
        )
        .map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_millis(700)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(350)))
            .map_err(|e| e.to_string())?;
        let auth = base64(self.cookie.as_bytes());
        let body = format!(
            "{{\"jsonrpc\":\"1.0\",\"id\":\"dash\",\"method\":\"{method}\",\"params\":[]}}"
        );
        let request = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = Vec::with_capacity(4096);
        stream
            .take(64 * 1024)
            .read_to_end(&mut response)
            .map_err(|e| e.to_string())?;
        let response = String::from_utf8_lossy(&response);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .ok_or_else(|| "bad HTTP response".to_string())?;
        if body.contains("\"error\":null") || body.contains("\"result\":") {
            Ok(body.to_string())
        } else {
            Err("RPC error".to_string())
        }
    }
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn meminfo_value(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name != key {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
        .unwrap_or(0)
        / 1024
}

fn json_number_start<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let after = body.split_once(&needle)?.1;
    let after = after.split_once(':')?.1.trim_start();
    Some(after)
}

fn json_u64(body: &str, key: &str) -> Option<u64> {
    let value = json_number_start(body, key)?;
    let end = value
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(value.len());
    value[..end].parse().ok()
}

fn json_f64(body: &str, key: &str) -> Option<f64> {
    let value = json_number_start(body, key)?;
    let end = value
        .bytes()
        .position(|b| {
            !(b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'e' || b == b'E' || b == b'+')
        })
        .unwrap_or(value.len());
    value[..end].parse().ok()
}

fn json_bool(body: &str, key: &str) -> Option<bool> {
    let value = json_number_start(body, key)?;
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut index = 0;
    while index < input.len() {
        let a = input[index] as u32;
        let b = input.get(index + 1).copied().unwrap_or(0) as u32;
        let c = input.get(index + 2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b << 8) | c;
        output.push(TABLE[((triple >> 18) & 63) as usize] as char);
        output.push(TABLE[((triple >> 12) & 63) as usize] as char);
        output.push(if index + 1 < input.len() {
            TABLE[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if index + 2 < input.len() {
            TABLE[(triple & 63) as usize] as char
        } else {
            '='
        });
        index += 3;
    }
    output
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds / 3_600) % 24;
    let minutes = (seconds / 60) % 60;
    if days > 0 {
        format!("{days}d {hours:02}h {minutes:02}m")
    } else {
        format!("{hours:02}h {minutes:02}m")
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000_000 {
        format!("{}T", bytes / 1_000_000_000_000)
    } else if bytes >= 1_000_000_000 {
        format!("{}G", bytes / 1_000_000_000)
    } else if bytes >= 1_000_000 {
        format!("{}M", bytes / 1_000_000)
    } else {
        format!("{}K", bytes / 1_000)
    }
}

fn fit(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn draw_panel(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    title: &str,
    scale: usize,
) {
    fb.rect(x, y, w, h, PANEL);
    fb.rect(x, y, w, scale.max(1), CYAN);
    fb.text(x + 4 * scale, y + 4 * scale, title, scale, WHITE);
}

fn draw_line(fb: &mut Framebuffer, x: usize, y: usize, text: &str, color: Color, scale: usize) {
    fb.text(x, y, text, scale, color);
}

fn draw_history(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    samples: &[u8],
    color: Color,
) {
    fb.rect(x, y, width, height, BLACK);
    fb.rect(x, y + height / 2, width, 1, PANEL_ALT);
    if samples.is_empty() || width == 0 || height == 0 {
        return;
    }
    let slots = HISTORY_SAMPLES.min(width);
    let count = samples.len().min(slots);
    let start = samples.len() - count;
    let column_width = (width / slots).max(1);
    let first_slot = slots - count;
    for (index, value) in samples[start..].iter().enumerate() {
        let bar_height = height.saturating_sub(2) * (*value as usize) / 100;
        if bar_height == 0 {
            continue;
        }
        let bar_x = x + (first_slot + index) * column_width;
        fb.rect(
            bar_x,
            y + height - bar_height,
            column_width.saturating_sub(1).max(1),
            bar_height,
            color,
        );
    }
}

fn draw_meter(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    value: u8,
    color: Color,
) {
    fb.rect(x, y, width, height, BLACK);
    let filled = width.saturating_mul(value as usize) / 100;
    if filled > 0 {
        fb.rect(x, y, filled, height, color);
    }
}

fn render(fb: &mut Framebuffer, snapshot: &Snapshot, history: &MetricHistory) {
    let scale = if fb.width >= 600 && fb.height >= 400 {
        2
    } else {
        1
    };
    let margin = 5 * scale;
    let line = 9 * scale;
    fb.fill(BLACK);

    let header_h = 18 * scale;
    fb.rect(0, 0, fb.width, header_h, PANEL_ALT);
    fb.text(margin, 5 * scale, "BITCOIN NODE", scale, WHITE);
    let status = if snapshot.node.rpc_ok {
        "ONLINE"
    } else if snapshot.node.process_alive {
        "RUNNING"
    } else {
        "STARTING"
    };
    let status_color = if snapshot.node.rpc_ok {
        GREEN
    } else if snapshot.node.process_alive {
        AMBER
    } else {
        RED
    };
    let status_x = fb.width.saturating_sub((status.len() * 6 + 7) * scale);
    fb.rect(
        status_x.saturating_sub(3 * scale),
        4 * scale,
        4 * scale,
        9 * scale,
        status_color,
    );
    fb.text(status_x + 4 * scale, 5 * scale, status, scale, status_color);

    let body_y = header_h + margin;
    let panel_gap = margin;
    let panel_h = 74 * scale;
    let panel_w = (fb.width.saturating_sub(margin * 2 + panel_gap)) / 2;
    let left_x = margin;
    let right_x = left_x + panel_w + panel_gap;
    draw_panel(fb, left_x, body_y, panel_w, panel_h, "MACHINE", scale);
    draw_panel(fb, right_x, body_y, panel_w, panel_h, "NODE", scale);

    let machine_color = if !snapshot.machine.disk_mounted {
        RED
    } else if snapshot.machine.mem_available < 16 {
        AMBER
    } else {
        GREEN
    };
    let node_color = if !snapshot.node.rpc_ok {
        if snapshot.node.process_alive {
            AMBER
        } else {
            RED
        }
    } else if snapshot.node.ibd || snapshot.node.headers > snapshot.node.blocks {
        AMBER
    } else {
        GREEN
    };
    let text_x = left_x + 4 * scale;
    let mut y = body_y + 17 * scale;
    draw_line(
        fb,
        text_x,
        y,
        &format!("UP {}", format_uptime(snapshot.machine.uptime_seconds)),
        WHITE,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!("LOAD {}", fit(&snapshot.machine.load, 18)),
        MUTED,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!(
            "RAM {} / {}M",
            snapshot.machine.mem_available, snapshot.machine.mem_total
        ),
        machine_color,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!(
            "SWAP {} / {}M",
            snapshot.machine.swap_used, snapshot.machine.swap_total
        ),
        if snapshot.machine.swap_used > 384 {
            AMBER
        } else {
            MUTED
        },
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        if snapshot.machine.disk_mounted {
            "SSD MOUNTED"
        } else {
            "SSD MISSING"
        },
        machine_color,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!("FB {}x{} {}b", fb.width, fb.height, fb.bits_per_pixel),
        MUTED,
        scale,
    );

    let text_x = right_x + 4 * scale;
    let mut y = body_y + 17 * scale;
    draw_line(
        fb,
        text_x,
        y,
        &format!("CORE v31.0.0  RAM {}M", snapshot.node.resident_kib / 1024),
        WHITE,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        if snapshot.node.rpc_ok {
            "RPC READY"
        } else {
            "RPC WAITING"
        },
        node_color,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        if snapshot.node.rpc_ok {
            format!("CHAIN {} / {}", snapshot.node.blocks, snapshot.node.headers)
        } else {
            "CHAIN WAITING".to_string()
        }
        .as_str(),
        node_color,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        if snapshot.node.rpc_ok {
            format!("SYNC {:>5.2}%", snapshot.node.verification * 100.0)
        } else {
            "SYNC WAITING".to_string()
        }
        .as_str(),
        if snapshot.node.ibd { AMBER } else { GREEN },
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!(
            "PEERS {} {}",
            snapshot.node.connections,
            if snapshot.node.network_active {
                "NET"
            } else {
                "OFF"
            }
        ),
        MUTED,
        scale,
    );
    y += line;
    draw_line(
        fb,
        text_x,
        y,
        &format!(
            "{} {}",
            if snapshot.node.pruned {
                "PRUNED"
            } else {
                "ARCHIVAL"
            },
            format_size(snapshot.node.disk_bytes),
        ),
        if snapshot.node.pruned { RED } else { GREEN },
        scale,
    );

    let help_y = body_y + panel_h + panel_gap;
    let help_h = fb.height.saturating_sub(help_y + 16 * scale + margin);
    let total_w = fb.width.saturating_sub(margin * 2);
    let history_w = total_w.saturating_mul(3) / 5;
    let transaction_x = margin + history_w + panel_gap;
    let transaction_w = total_w.saturating_sub(history_w + panel_gap);
    draw_panel(fb, margin, help_y, history_w, help_h, "LIVE METRICS", scale);
    draw_panel(
        fb,
        transaction_x,
        help_y,
        transaction_w,
        help_h,
        "MEMPOOL",
        scale,
    );

    let chart_x = margin + 4 * scale;
    let chart_w = history_w.saturating_sub(8 * scale);
    let chart_h = 12 * scale;
    let chart_step = 23 * scale;
    let mut chart_y = help_y + 17 * scale;
    for (label, samples, color) in [
        (
            format!("LOAD1 {}", fit(&snapshot.machine.load, 18)),
            &history.load,
            CYAN,
        ),
        (
            format!(
                "MEM {}%",
                percent(
                    snapshot
                        .machine
                        .mem_total
                        .saturating_sub(snapshot.machine.mem_available),
                    snapshot.machine.mem_total,
                )
            ),
            &history.memory,
            GREEN,
        ),
        (
            format!(
                "SWAP {}%",
                percent(snapshot.machine.swap_used, snapshot.machine.swap_total)
            ),
            &history.swap,
            AMBER,
        ),
    ] {
        draw_line(fb, chart_x, chart_y, &label, color, scale);
        draw_history(
            fb,
            chart_x,
            chart_y + 9 * scale,
            chart_w,
            chart_h,
            samples,
            color,
        );
        chart_y += chart_step;
    }
    draw_line(
        fb,
        chart_x,
        chart_y,
        "3s SAMPLES   30s NODE RPC",
        MUTED,
        scale,
    );

    let mempool_x = transaction_x + 4 * scale;
    let mempool_w = transaction_w.saturating_sub(8 * scale);
    let mempool = &snapshot.node.mempool;
    if mempool.available {
        draw_line(
            fb,
            mempool_x,
            help_y + 17 * scale,
            &format!(
                "TXS {}  BYTES {}",
                mempool.entries,
                format_size(mempool.bytes)
            ),
            WHITE,
            scale,
        );
        draw_line(
            fb,
            mempool_x,
            help_y + 26 * scale,
            &format!(
                "USAGE {} / {}",
                format_size(mempool.usage),
                format_size(mempool.limit)
            ),
            CYAN,
            scale,
        );
        draw_meter(
            fb,
            mempool_x,
            help_y + 35 * scale,
            mempool_w,
            5 * scale,
            percent(mempool.usage, mempool.limit),
            CYAN,
        );
        draw_line(
            fb,
            mempool_x,
            help_y + 45 * scale,
            &format!("MIN FEE {:.2} SAT/VB", mempool.min_fee * 100_000.0),
            AMBER,
            scale,
        );
        draw_line(
            fb,
            mempool_x,
            help_y + 54 * scale,
            &format!("UNBROADCAST {}", mempool.unbroadcast),
            MUTED,
            scale,
        );
    } else {
        draw_line(
            fb,
            mempool_x,
            help_y + 17 * scale,
            "MEMPOOL WAITING",
            AMBER,
            scale,
        );
        draw_line(
            fb,
            mempool_x,
            help_y + 26 * scale,
            "CORE RPC UNAVAILABLE",
            MUTED,
            scale,
        );
        draw_line(
            fb,
            mempool_x,
            help_y + 35 * scale,
            "RETRY AT NEXT 30S POLL",
            MUTED,
            scale,
        );
    }

    let footer_y = fb.height.saturating_sub(10 * scale);
    fb.rect(
        0,
        footer_y.saturating_sub(2 * scale),
        fb.width,
        2 * scale,
        BLUE,
    );
    draw_line(
        fb,
        margin,
        footer_y,
        "DIRECT FBDEV  3s DRAW  30s RPC  SIGTERM TO EXIT",
        MUTED,
        scale,
    );
}

fn glyph(byte: u8) -> [u8; 5] {
    match byte.to_ascii_uppercase() {
        b'A' => [0x7e, 0x11, 0x11, 0x11, 0x7e],
        b'B' => [0x7f, 0x49, 0x49, 0x49, 0x36],
        b'C' => [0x3e, 0x41, 0x41, 0x41, 0x22],
        b'D' => [0x7f, 0x41, 0x41, 0x22, 0x1c],
        b'E' => [0x7f, 0x49, 0x49, 0x49, 0x41],
        b'F' => [0x7f, 0x09, 0x09, 0x09, 0x01],
        b'G' => [0x3e, 0x41, 0x49, 0x49, 0x7a],
        b'H' => [0x7f, 0x08, 0x08, 0x08, 0x7f],
        b'I' => [0x00, 0x41, 0x7f, 0x41, 0x00],
        b'J' => [0x20, 0x40, 0x41, 0x3f, 0x01],
        b'K' => [0x7f, 0x08, 0x14, 0x22, 0x41],
        b'L' => [0x7f, 0x40, 0x40, 0x40, 0x40],
        b'M' => [0x7f, 0x02, 0x0c, 0x02, 0x7f],
        b'N' => [0x7f, 0x04, 0x08, 0x10, 0x7f],
        b'O' => [0x3e, 0x41, 0x41, 0x41, 0x3e],
        b'P' => [0x7f, 0x09, 0x09, 0x09, 0x06],
        b'Q' => [0x3e, 0x41, 0x51, 0x21, 0x5e],
        b'R' => [0x7f, 0x09, 0x19, 0x29, 0x46],
        b'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        b'T' => [0x01, 0x01, 0x7f, 0x01, 0x01],
        b'U' => [0x3f, 0x40, 0x40, 0x40, 0x3f],
        b'V' => [0x1f, 0x20, 0x40, 0x20, 0x1f],
        b'W' => [0x3f, 0x40, 0x38, 0x40, 0x3f],
        b'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        b'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        b'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        b'0' => [0x3e, 0x45, 0x49, 0x51, 0x3e],
        b'1' => [0x00, 0x41, 0x7f, 0x40, 0x00],
        b'2' => [0x62, 0x51, 0x49, 0x49, 0x46],
        b'3' => [0x22, 0x41, 0x49, 0x49, 0x36],
        b'4' => [0x18, 0x14, 0x12, 0x7f, 0x10],
        b'5' => [0x2f, 0x49, 0x49, 0x49, 0x31],
        b'6' => [0x3e, 0x49, 0x49, 0x49, 0x32],
        b'7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        b'8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        b'9' => [0x26, 0x49, 0x49, 0x49, 0x3e],
        b'.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        b':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        b'/' => [0x60, 0x10, 0x08, 0x04, 0x03],
        b'-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        b'_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        b'%' => [0x63, 0x13, 0x08, 0x64, 0x63],
        b'+' => [0x08, 0x08, 0x3e, 0x08, 0x08],
        b'[' => [0x7f, 0x41, 0x41, 0x00, 0x00],
        b']' => [0x00, 0x00, 0x41, 0x41, 0x7f],
        b'=' => [0x14, 0x14, 0x14, 0x14, 0x14],
        b' ' => [0; 5],
        _ => [0x7f, 0x41, 0x5d, 0x41, 0x7f],
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let info = first.as_deref() == Some("--info");
    let device = if info {
        args.next().unwrap_or_else(|| "/dev/fb0".to_string())
    } else {
        first.unwrap_or_else(|| "/dev/fb0".to_string())
    };
    let mut framebuffer = match Framebuffer::open(&device) {
        Ok(fb) => fb,
        Err(error) => {
            eprintln!("node-dashboard: {error}");
            std::process::exit(1);
        }
    };
    if info {
        println!(
            "{}x{} {}bpp line={} R{}:{} G{}:{} B{}:{} A{}:{}",
            framebuffer.width,
            framebuffer.height,
            framebuffer.bits_per_pixel,
            framebuffer.line_length,
            framebuffer.red.offset,
            framebuffer.red.length,
            framebuffer.green.offset,
            framebuffer.green.length,
            framebuffer.blue.offset,
            framebuffer.blue.length,
            framebuffer.transp.offset,
            framebuffer.transp.length,
        );
        return;
    }
    let mut dashboard = Dashboard::new();
    loop {
        let started = Instant::now();
        let snapshot = dashboard.collect();
        render(&mut framebuffer, &snapshot, &dashboard.history);
        framebuffer.publish();
        let elapsed = started.elapsed();
        if elapsed < Duration::from_secs(DASHBOARD_TICK_SECS) {
            thread::sleep(Duration::from_secs(DASHBOARD_TICK_SECS) - elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rpc_scalars_without_a_json_dependency() {
        let body = r#"{"blocks":960554,"headers":960574,"verificationprogress":0.9999,"initialblockdownload":false,"pruned":false,"size_on_disk":864053269491,"warnings":""}"#;
        assert_eq!(json_u64(body, "blocks"), Some(960554));
        assert_eq!(json_u64(body, "headers"), Some(960574));
        assert_eq!(json_bool(body, "initialblockdownload"), Some(false));
        assert_eq!(json_f64(body, "verificationprogress"), Some(0.9999));
    }

    #[test]
    fn cookie_auth_encoding_is_stable() {
        assert_eq!(base64(b"__cookie__:abc"), "X19jb29raWVfXzphYmM=");
    }

    #[test]
    fn parses_mempool_statistics() {
        let mempool = Mempool::from_rpc(
            r#"{"size":17,"bytes":4920,"usage":18432,"maxmempool":5000000,"mempoolminfee":0.00001000,"unbroadcastcount":2}"#,
        );
        assert!(mempool.available);
        assert_eq!(mempool.entries, 17);
        assert_eq!(mempool.bytes, 4920);
        assert_eq!(mempool.usage, 18432);
        assert_eq!(mempool.limit, 5_000_000);
        assert_eq!(mempool.min_fee, 0.00001);
        assert_eq!(mempool.unbroadcast, 2);
    }

    #[test]
    fn formatting_stays_short_for_the_frame() {
        assert_eq!(format_uptime(90_061), "1d 01h 01m");
        assert_eq!(format_size(864_053_269_491), "864G");
        assert_eq!(fit("ABCDEFGHIJKLMNOPQRSTUVWXYZ", 8), "ABCDEFGH");
    }

    #[test]
    fn logical_landscape_maps_to_the_visible_physical_strip() {
        assert_eq!(physical_position(0, 0), (1279, 0));
        assert_eq!(physical_position(1279, 479), (0, 479));
    }

    #[test]
    fn metric_history_is_bounded_and_percent_is_safe() {
        let mut samples = Vec::new();
        for value in 0..100 {
            push_sample(&mut samples, value);
        }
        assert_eq!(samples.len(), HISTORY_SAMPLES);
        assert_eq!(samples[0], 20);
        assert_eq!(samples[HISTORY_SAMPLES - 1], 99);
        assert_eq!(percent(1, 0), 0);
        assert_eq!(percent(3, 4), 75);
    }

    #[test]
    fn reads_core_resident_memory_from_proc_status() {
        assert_eq!(
            status_kib("Name:\tbitcoind\nVmRSS:\t115712 kB\n", "VmRSS"),
            Some(115712)
        );
        assert_eq!(status_kib("Name:\tbitcoind\n", "VmRSS"), None);
    }
}
