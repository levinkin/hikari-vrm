mod behavior;
mod codec;
pub mod engine_ffi;
mod motion;
mod vrma;

use behavior::Behavior;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::io::{self, BufRead, BufReader, IoSlice, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
enum Lane {
    Render = 0,
    RenderSplit = 1,
}

type Frames = (HashMap<((usize, usize), u8), Arc<Vec<u8>>>, [u8; 4]);

enum Job {
    Tick { wants: Arc<Vec<((usize, usize), u8)>>, frames: HashMap<((usize, usize), u8), Arc<Vec<u8>>>, reply: Sender<Frames> },
    Step { mouth: f32, size: (usize, usize), codec_id: u8, reply: Sender<(Vec<u8>, [u8; 4])> },
    React { kind: String, reply: Sender<Value> },
    Wake,
    Background { bytes: Option<Arc<Vec<u8>>>, content_type: String, reply: Sender<i32> },
    Switch { scene: Scene, reply: Sender<Scene> },
}

#[repr(C)]
struct KEvent {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: isize,
    udata: *mut c_void,
}

#[repr(C)]
struct TimeSpec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn kqueue() -> i32;
    fn kevent(kq: i32, changelist: *const KEvent, nchanges: i32, eventlist: *mut KEvent, nevents: i32, timeout: *const TimeSpec) -> i32;
}

const SPLIT_SIZE: (usize, usize) = (320, 240);
const MOUTHS: usize = 256;
const REST_ANGLE_MAX: u64 = 100;
const WINDOW_TURN_DEGREES: f32 = 90.0;
const DEFAULT_VIEW: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
const CODECS: [&str; 6] = ["jpeg", "pal256", "rgb565", "split", "qoi", "mjpeg"];
const JPEG: u8 = 0;
const MJPEG: u8 = 5;
const MAX_DT: Duration = Duration::from_millis(100);
const TARGET_SWEEP: Duration = Duration::from_secs(1);
const MAX_HEAD_BYTES: u64 = 16 << 10;
const MAX_IMAGE_BYTES: u64 = 5 << 20;
const MAX_CONNECTIONS: usize = 64;
const HEAD_DEADLINE: Duration = Duration::from_secs(5);
const BODY_DEADLINE: Duration = Duration::from_secs(30);
const STREAM_IDLE: Duration = Duration::from_secs(30);
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

static PORT: AtomicU16 = AtomicU16::new(0);
static STOPPED: AtomicBool = AtomicBool::new(false);
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

struct App {
    jobs: Sender<Job>,
    timer_queue: i32,
    ready: AtomicBool,
    models: BTreeMap<String, PathBuf>,
    model: Mutex<Model>,
    size: (usize, usize),
    idle: Mutex<IdleSettings>,
    broken: Mutex<Vec<String>>,
    bg_lock: Mutex<()>,
    settings_lock: Mutex<()>,
    bg_dir: PathBuf,
    motions: Arc<Vec<motion::Motion>>,
    views: Mutex<Views>,
    background: Mutex<Option<(Arc<Vec<u8>>, String)>>,
    mouths: Mutex<Mouths>,
    state: Mutex<ModelState>,
    asleep: AtomicBool,
    head_lead_ms: AtomicU64,
    params: Mutex<Vec<(String, f32)>>,
    rest_angle: AtomicU64,
    shared: Mutex<Shared>,
    picture: (Mutex<Picture>, Condvar),
    lane_locks: [Mutex<()>; 2],
    rendering: [AtomicBool; 2],
}

#[derive(Clone, PartialEq)]
struct ModelState {
    face: String,
    pose: String,
    face_given: bool,
    generation: u64,
}

struct Mouths {
    owner: usize,
    t0: Instant,
    fps: f64,
    ring: [(usize, f32); MOUTHS],
}

impl Mouths {
    fn mouth_now(&self, now: Instant) -> f32 {
        let at = (now.saturating_duration_since(self.t0).as_secs_f64() * self.fps) as usize;
        let (frame, mouth) = self.ring[at % MOUTHS];
        if frame == at { mouth } else { -1.0 }
    }
}

struct EngineHandle(*mut engine_ffi::Engine);

unsafe impl Send for EngineHandle {}
unsafe impl Sync for EngineHandle {}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let engine = self.0;
        unsafe { engine_ffi::engine_destroy(engine) };
    }
}

struct Shared {
    engine: Option<Arc<EngineHandle>>,
    name: String,
    bg_clock_ms: f64,
}

struct Picture {
    seq: u64,
    head: [u8; 4],
    frames: HashMap<((usize, usize), u8), Arc<Vec<u8>>>,
    wants: Vec<((usize, usize), u8, f64)>,
    window_fps: f64,
}

#[derive(Clone)]
struct Model {
    name: String,
    states: Vec<String>,
    actions: Vec<String>,
    params: Vec<(String, f32, f32)>,
}

struct Scene {
    model: Model,
    engine: Arc<EngineHandle>,
    state: LaneState,
    asleep: bool,
}

unsafe impl Send for Scene {}

struct Views {
    live: HashMap<String, [f32; 4]>,
    saved: HashMap<String, [f32; 4]>,
    path: PathBuf,
}

struct LaneState {
    behavior: Behavior,
    view: [f32; 4],
    idle_generation: u64,
    state_generation: u64,
}

struct IdleSettings {
    actions: Vec<String>,
    every: Value,
    face: String,
    idle_loop: String,
    generation: u64,
    path: PathBuf,
}

struct WindowLoop<'a> {
    app: &'a App,
    name: String,
    engine: Option<Arc<EngineHandle>>,
    window: *mut engine_ffi::Window,
    frame_seconds: f32,
    last: Instant,
    second: Instant,
    frames: u32,
    mouth_frames: u32,
    mouth_max: f32,
    width: i32,
    height: i32,
    share: bool,
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
}

struct Timed {
    stream: TcpStream,
    deadline: Option<Instant>,
}

struct Slot;

struct Body {
    reader: BufReader<Timed>,
    remaining: u64,
    chunked: bool,
    done: bool,
}

struct Inbound {
    pcm: Vec<u8>,
    read: usize,
    base: usize,
    cap: usize,
    channels: usize,
    partial: Vec<u8>,
    ended: bool,
    closed: bool,
    t0: Option<Instant>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let wait = match self.deadline {
            Some(deadline) => deadline.saturating_duration_since(Instant::now()),
            None => STREAM_IDLE,
        };
        if wait.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(wait))?;
        self.stream.read(buf)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.remaining == 0 {
            if !self.chunked {
                self.done = true;
                return Ok(0);
            }
            let mut line = String::new();
            if read_chunk_line(&mut self.reader, &mut line)? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let size_text = line.split(';').next().unwrap_or_default().trim();
            let size = u64::from_str_radix(size_text, 16).map_err(io::Error::other)?;
            if size == 0 {
                let mut trailer = 0;
                loop {
                    line.clear();
                    let n = read_chunk_line(&mut self.reader, &mut line)?;
                    trailer += n;
                    if trailer > 4096 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "chunk trailer too long"));
                    }
                    if n == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                self.done = true;
                return Ok(0);
            }
            self.remaining = size;
        }
        let want = buf.len().min(self.remaining as usize);
        let n = self.reader.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.remaining -= n as u64;
        if self.chunked && self.remaining == 0 {
            let mut crlf = String::new();
            read_chunk_line(&mut self.reader, &mut crlf)?;
            if crlf != "\r\n" && crlf != "\n" {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "chunk missing CRLF"));
            }
        }
        Ok(n)
    }
}

fn read_chunk_line(reader: &mut BufReader<Timed>, line: &mut String) -> io::Result<usize> {
    let n = reader.by_ref().take(4096).read_line(line)?;
    if n == 4096 && !line.ends_with('\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "chunk line too long"));
    }
    Ok(n)
}

fn param(req: &Request, name: &str, default: &str) -> String {
    for (key, value) in &req.query {
        if key == name {
            if value.is_empty() {
                break;
            }
            return value.clone();
        }
    }
    default.to_string()
}

fn single_param<'a>(req: &'a Request, name: &str) -> Result<Option<&'a str>, String> {
    let mut found = None;
    for (key, value) in &req.query {
        if key != name {
            continue;
        }
        if found.is_some() {
            return Err(format!("hikari-vrm: {} given more than once\n", name));
        }
        found = Some(value.as_str());
    }
    Ok(found)
}

fn bool_param(req: &Request, name: &str) -> Result<Option<bool>, String> {
    match single_param(req, name)? {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(format!("hikari-vrm: {} must be true or false\n", name)),
    }
}

fn header(req: &Request, name: &str) -> String {
    for (key, value) in &req.headers {
        if key == name {
            return value.clone();
        }
    }
    String::new()
}

fn js_number(text: &str) -> f64 {
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}').to_ascii_lowercase();
    if trimmed.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0b", 2), ("0o", 8)] {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return u128::from_str_radix(digits, radix).map_or(f64::INFINITY, |v| v as f64);
        }
    }
    trimmed.parse().unwrap_or(f64::NAN)
}

fn iso_now() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let day_ms = ms.rem_euclid(86_400_000);
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", year, month, day, day_ms / 3_600_000, day_ms / 60_000 % 60, day_ms / 1000 % 60, day_ms % 1000)
}

const STREAM_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n";
const MJPEG_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";

fn send_body(out: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut response = format!("HTTP/1.1 {} {}\r\n", status, reason).into_bytes();
    if !content_type.is_empty() {
        response.extend(format!("content-type: {}\r\n", content_type).into_bytes());
    }
    response.extend(format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes());
    response.extend_from_slice(body);
    out.write_all(&response).ok();
}

fn write_frame(out: &mut TcpStream, pts: f64, head: &[u8], codec_id: u8, data: &[u8]) -> bool {
    let mut prefix = Vec::with_capacity(80);
    if codec_id == MJPEG {
        write!(prefix, "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", data.len()).unwrap();
    } else {
        write!(prefix, "{:x}\r\n", data.len() + head.len() + 9).unwrap();
        prefix.extend((pts as u32).to_be_bytes());
        prefix.extend((data.len() as u32 + 1).to_be_bytes());
        prefix.extend_from_slice(head);
        prefix.push(codec_id);
    }
    let mut parts = [IoSlice::new(&prefix), IoSlice::new(data), IoSlice::new(b"\r\n")];
    let mut rest = &mut parts[..];
    while !rest.is_empty() {
        match out.write_vectored(rest) {
            Ok(0) => return false,
            Ok(n) => IoSlice::advance_slices(&mut rest, n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

fn window_mouth(pcm: &[u8], start: usize, end: usize) -> f32 {
    if end <= start {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for i in start..end {
        let v = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as f64;
        sum += v * v;
    }
    let rms = (sum / (end - start) as f64).sqrt();
    (0.45 * ((rms - 500.0) / 9000.0).clamp(0.0, 1.0)) as f32
}

fn read_inbound(mut body: io::Take<Body>, shared: Arc<(Mutex<Inbound>, Condvar)>) {
    let mut buf = vec![0u8; 65536];
    loop {
        let mut guard = shared.0.lock().unwrap();
        while (guard.pcm.len() - guard.read) / 2 >= guard.cap && !guard.closed {
            guard = shared.1.wait(guard).unwrap();
        }
        if guard.closed {
            return;
        }
        drop(guard);
        match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut guard = shared.0.lock().unwrap();
                let state = &mut *guard;
                if state.t0.is_none() {
                    state.t0 = Some(Instant::now());
                }
                let channels = state.channels;
                state.partial.extend_from_slice(&buf[..n]);
                let whole = state.partial.len() / (channels * 2) * channels * 2;
                for chunk in state.partial[..whole].chunks_exact(channels * 2) {
                    let sum: i32 = chunk.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as i32).sum();
                    state.pcm.extend_from_slice(&((sum / channels as i32) as i16).to_le_bytes());
                }
                state.partial.drain(..whole);
                shared.1.notify_all();
            }
            Err(_) => {
                shared.0.lock().unwrap().closed = true;
                shared.1.notify_all();
                return;
            }
        }
    }
    shared.0.lock().unwrap().ended = true;
    shared.1.notify_all();
}

fn read_wav_header(body: &mut Body) -> Result<(f64, usize, u64), String> {
    let mut riff = [0u8; 12];
    if body.read_exact(&mut riff).is_err() || &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        return Err(format!("hikari-vrm: render body must be a RIFF/WAVE file, got {:?}", String::from_utf8_lossy(&riff[..4])));
    }
    let mut format = None;
    let mut header_bytes = riff.len() as u64;
    loop {
        let mut head = [0u8; 8];
        if body.read_exact(&mut head).is_err() {
            return Err("hikari-vrm: render body must be a wav with a data chunk, got none".to_string());
        }
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if &head[..4] == b"data" {
            let Some((rate, channels)) = format else {
                return Err("hikari-vrm: render body must have a fmt chunk before data, got data first".to_string());
            };
            let limit = if size == 0 || size == u32::MAX { u64::MAX } else { size as u64 };
            return Ok((rate, channels, limit));
        }
        let padded = size as u64 + (size & 1) as u64;
        header_bytes += head.len() as u64 + padded;
        if header_bytes > 65536 {
            return Err("hikari-vrm: wav header too large".to_string());
        }
        let is_fmt = &head[..4] == b"fmt ";
        if is_fmt && size < 16 {
            return Err(format!("hikari-vrm: render body fmt chunk must be 16 bytes or more, got {}", size));
        }
        let mut chunk = [0u8; 16];
        let skip = if is_fmt { padded - 16 } else { padded };
        if (is_fmt && body.read_exact(&mut chunk).is_err()) || !matches!(io::copy(&mut body.by_ref().take(skip), &mut io::sink()), Ok(n) if n == skip) {
            return Err(format!("hikari-vrm: render body must be a complete wav, got a cut {} chunk", String::from_utf8_lossy(&head[..4])));
        }
        if !is_fmt {
            continue;
        }
        let tag = u16::from_le_bytes([chunk[0], chunk[1]]);
        let channels = u16::from_le_bytes([chunk[2], chunk[3]]);
        let rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        let bits = u16::from_le_bytes([chunk[14], chunk[15]]);
        if tag != 1 {
            return Err(format!("hikari-vrm: render body must be pcm wav (format 1), got format {}", tag));
        }
        if bits != 16 {
            return Err(format!("hikari-vrm: render body must be 16-bit pcm wav, got {}-bit", bits));
        }
        if !(1..=2).contains(&channels) {
            return Err(format!("hikari-vrm: render body must have 1 or 2 channels, got {}", channels));
        }
        if !(8000..=96000).contains(&rate) {
            return Err(format!("hikari-vrm: render body rate must be 8000-96000, got {}", rate));
        }
        format = Some((rate as f64, channels as usize));
    }
}

fn request_view(req: &Request) -> [f32; 4] {
    let mut view = ["zoom", "x", "y", "rot"].map(|name| js_number(&param(req, name, "nan")) as f32);
    if !(view[0] > 0.0) {
        view[0] = f32::NAN;
    }
    view
}

fn fps_param(req: &Request) -> Result<f64, String> {
    let value = single_param(req, "fps")?.unwrap_or("60");
    match value.parse::<u32>() {
        Ok(fps @ 1..=240) if value.bytes().all(|byte| byte.is_ascii_digit()) => Ok(fps as f64),
        _ => Err(format!("hikari-vrm: fps must be a whole number from 1 to 240, got {}\n", value)),
    }
}

fn size_param(req: &Request, default: (usize, usize)) -> Result<(usize, usize), String> {
    let Some(value) = single_param(req, "size")? else { return Ok(default) };
    let parsed = value.bytes().all(|b| b.is_ascii_digit() || b == b'x').then_some(value).and_then(|value| value.split_once('x').and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))));
    match parsed {
        Some((w, h)) if (1..=3840).contains(&w) && (1..=2160).contains(&h) => Ok((w, h)),
        _ => Err(format!("hikari-vrm: size must be WxH up to 3840x2160, got {}\n", value)),
    }
}

fn handle_render(app: &App, out: &mut TcpStream, body: io::Take<Body>, fps: f64, (rate, channels): (f64, usize), size: (usize, usize), codec_name: &str, codec_id: u8) {
    let lane = if codec_name == "split" { Lane::RenderSplit } else { Lane::Render };
    let guard = app.lane_locks[lane as usize].lock().unwrap_or_else(PoisonError::into_inner);
    let shared = Arc::new((Mutex::new(Inbound { pcm: Vec::new(), read: 0, base: 0, cap: (rate * 2.0) as usize, channels, partial: Vec::new(), ended: false, closed: false, t0: None }), Condvar::new()));
    let reader_shared = shared.clone();
    let reader = thread::spawn(move || read_inbound(body, reader_shared));
    app.rendering[lane as usize].store(true, Ordering::SeqCst);
    app.jobs.send(Job::Wake).unwrap();
    out.write_all(if codec_id == MJPEG { MJPEG_HEAD } else { STREAM_HEAD }).ok();
    let owner = Arc::as_ptr(&shared) as usize;
    let mut k = 0u64;
    let mut behind_max = 0.0f64;
    loop {
        let mut state = shared.0.lock().unwrap();
        if state.closed {
            break;
        }
        let samples = state.base + (state.pcm.len() - state.read) / 2;
        let start = (k as f64 * rate / fps).floor() as usize;
        let end = ((k + 1) as f64 * rate / fps).floor() as usize;
        if start >= samples && state.ended {
            break;
        }
        if end > samples && !state.ended {
            drop(shared.1.wait(state).unwrap());
            continue;
        }
        let used = end.min(samples);
        let mouth = window_mouth(&state.pcm[state.read..], start - state.base, used - state.base);
        state.read += (used - state.base) * 2;
        state.base = used;
        if state.read * 2 > state.pcm.len() {
            let read = state.read;
            state.pcm.drain(..read);
            state.read = 0;
        }
        let t0 = state.t0.unwrap_or_else(Instant::now);
        drop(state);
        shared.1.notify_all();
        let mut mouths = app.mouths.lock().unwrap_or_else(PoisonError::into_inner);
        if k == 0 {
            mouths.owner = owner;
            mouths.t0 = t0;
            mouths.fps = fps;
            mouths.ring = [(usize::MAX, -1.0); MOUTHS];
        }
        if mouths.owner == owner {
            mouths.ring[k as usize % MOUTHS] = (k as usize, mouth);
        }
        drop(mouths);
        let pts = (k as f64 * 1000.0 / fps).floor();
        let (reply, frame) = channel();
        app.jobs.send(Job::Step { mouth, size, codec_id, reply }).unwrap();
        let (data, head) = frame.recv().unwrap();
        if !write_frame(out, pts, &head, codec_id, &data) {
            break;
        }
        behind_max = behind_max.max(t0.elapsed().as_secs_f64() * 1000.0 - pts);
        k += 1;
    }
    let reading = {
        let mut state = shared.0.lock().unwrap();
        let reading = !state.ended && !state.closed;
        state.closed = true;
        reading
    };
    shared.1.notify_all();
    if reading {
        out.shutdown(Shutdown::Read).ok();
    }
    reader.join().ok();
    app.rendering[lane as usize].store(false, Ordering::SeqCst);
    drop(guard);
    out.write_all(if codec_id == MJPEG { b"--frame--\r\n".as_slice() } else { b"0\r\n\r\n".as_slice() }).ok();
    println!("hikari-vrm: fps={} frames={} behind_ms_max={}", fps, k, behind_max.round());
}

fn put_view(app: &App, req: &Request, out: &mut TcpStream, save: bool) {
    let name = app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone();
    let request = request_view(req);
    let mut views = app.views.lock().unwrap_or_else(PoisonError::into_inner);
    let base = views.live.get(&name).or(views.saved.get(&name)).copied().unwrap_or(DEFAULT_VIEW);
    let view: [f32; 4] = std::array::from_fn(|i| if request[i].is_finite() { request[i] } else { base[i] });
    if view != base {
        views.live.insert(name.clone(), view);
        println!("hikari-vrm: view model={} zoom={} x={} y={} rot={}", name, view[0], view[1], view[2], view[3]);
    }
    if save {
        if let Err(line) = save_view(&mut views, &[(name, view)]) {
            send_body(out, 500, "text/plain", format!("{}\n", line).as_bytes());
            return;
        }
    }
    send_body(out, 204, "", b"");
}

fn describe_model(name: &str, expressions: &[(String, i32)], motions: &[motion::Motion]) -> Model {
    let mut states: Vec<String> = expressions.iter().map(|(name, _)| name.clone()).collect();
    states.sort();
    states.dedup();
    let mut actions: Vec<String> = motions.iter().map(|motion| motion.name.clone()).chain(states.iter().cloned()).collect();
    actions.sort();
    actions.dedup();
    Model { name: name.to_string(), states, actions, params: Vec::new() }
}

fn post_state(app: &App, req: &Request, out: &mut TcpStream) {
    let known = app.model.lock().unwrap_or_else(PoisonError::into_inner).states.clone();
    let asked: Vec<(String, String)> = req.query.iter().filter(|(key, _)| key == "face" || key == "pose").cloned().collect();
    if let Some((_, name)) = asked.iter().find(|(_, name)| name != "none" && !known.contains(name)) {
        send_body(out, 400, "text/plain", format!("hikari-vrm: unknown state {}\n", name).as_bytes());
        return;
    }
    let lead = req.query.iter().find(|(key, _)| key == "head_lead_ms").map(|(_, value)| value.parse::<u64>().ok().filter(|ms| *ms <= 1500).ok_or(value));
    if let Some(Err(value)) = lead {
        send_body(out, 400, "text/plain", format!("hikari-vrm: head_lead_ms must be an integer 0..1500, got {}\n", value).as_bytes());
        return;
    }
    if let Some(Ok(ms)) = lead {
        app.head_lead_ms.store(ms, Ordering::SeqCst);
    }
    let mut current = app.state.lock().unwrap_or_else(PoisonError::into_inner);
    let mut state = current.clone();
    let given = !asked.is_empty();
    for (key, name) in asked {
        if key == "face" {
            state.face = name;
            state.face_given = true;
        } else {
            state.pose = name;
        }
    }
    if state != *current {
        state.generation += 1;
    }
    if lead.is_none() || given {
        app.jobs.send(Job::Wake).unwrap();
    }
    let head_lead_ms = app.head_lead_ms.load(Ordering::SeqCst);
    println!("hikari-vrm: state face={} pose={} head_lead_ms={}", state.face, state.pose, head_lead_ms);
    *current = state.clone();
    drop(current);
    send_body(out, 200, "application/json", json!({ "face": state.face, "pose": state.pose, "asleep": app.asleep.load(Ordering::SeqCst), "head_lead_ms": head_lead_ms }).to_string().as_bytes());
}

fn read_json_object(body: Body, out: &mut TcpStream, message: &[u8]) -> Option<serde_json::Map<String, Value>> {
    let mut text = String::new();
    let read = body.take(65537).read_to_string(&mut text);
    if text.len() > 65536 {
        send_body(out, 413, "", b"");
        out.shutdown(Shutdown::Both).ok();
        return None;
    }
    match read.ok().and_then(|_| serde_json::from_str(&text).ok()) {
        Some(Value::Object(fields)) => Some(fields),
        _ => {
            send_body(out, 400, "text/plain", message);
            None
        }
    }
}

fn post_params(app: &App, body: Body, out: &mut TcpStream) {
    let Some(fields) = read_json_object(body, out, b"hikari-vrm: POST /params needs a JSON object\n") else { return };
    let model = app.model.lock().unwrap_or_else(PoisonError::into_inner);
    let known = &model.params;
    let unknown: Vec<&str> = fields.keys().filter(|id| !known.iter().any(|row| &row.0 == *id)).map(String::as_str).collect();
    if !unknown.is_empty() {
        send_body(out, 400, "text/plain", format!("hikari-vrm: unknown params {}\n", unknown.join(",")).as_bytes());
        return;
    }
    if let Some((id, _)) = fields.iter().find(|(_, value)| !value.is_null() && !value.is_number()) {
        send_body(out, 400, "text/plain", format!("hikari-vrm: {} must be a number or null\n", id).as_bytes());
        return;
    }
    let mut params = app.params.lock().unwrap_or_else(PoisonError::into_inner);
    for (id, value) in &fields {
        params.retain(|row| &row.0 != id);
        if let (Some(value), Some(row)) = (value.as_f64(), known.iter().find(|row| &row.0 == id)) {
            params.push((id.clone(), (value as f32).clamp(row.1, row.2)));
        }
    }
    let held: serde_json::Map<String, Value> = params.iter().map(|(id, value)| (id.clone(), json!(value))).collect();
    drop(params);
    drop(model);
    send_body(out, 200, "application/json", Value::Object(held).to_string().as_bytes());
}

fn handle_action(app: &App, out: &mut TcpStream, name: &str) {
    let unknown = format!("hikari-vrm: unknown action {}\n", name);
    if !app.model.lock().unwrap_or_else(PoisonError::into_inner).actions.iter().any(|known| known == name) {
        send_body(out, 400, "text/plain", unknown.as_bytes());
        return;
    }
    let mid_reply = app.rendering[Lane::Render as usize].load(Ordering::SeqCst) || app.rendering[Lane::RenderSplit as usize].load(Ordering::SeqCst);
    let (reply, played) = channel();
    app.jobs.send(Job::React { kind: name.to_string(), reply }).unwrap();
    let played = played.recv().unwrap();
    if !played.is_object() {
        send_body(out, 400, "text/plain", unknown.as_bytes());
        return;
    }
    send_body(out, 200, "application/json", played.to_string().as_bytes());
    println!("hikari-vrm: action {} motion={} mid_reply={}", name, played["motion"].as_str().unwrap_or("undefined"), mid_reply);
}

fn read_bg_meta(bg_dir: &Path) -> Value {
    let none = json!({ "kind": "none", "name": null, "updated": null });
    match fs::read(bg_dir.join("background.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(none),
        Err(_) => none,
    }
}

fn apply_background(app: &App, bytes: Option<Arc<Vec<u8>>>, content_type: &str) -> i32 {
    let (reply, frames) = channel();
    app.jobs.send(Job::Background { bytes, content_type: content_type.to_string(), reply }).unwrap();
    frames.recv().unwrap()
}

fn clear_bg_files(bg_dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(bg_dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("current.") && name != keep {
            fs::remove_file(entry.path()).ok();
        }
    }
}

fn saved_background(bg_dir: &Path) -> Option<(Vec<u8>, String)> {
    let meta = read_bg_meta(bg_dir);
    if meta["kind"] == "none" {
        return None;
    }
    let bytes = motion::read_capped(&bg_dir.join(meta["file"].as_str().unwrap_or_default()), MAX_IMAGE_BYTES).ok()?;
    Some((bytes, meta["type"].as_str().unwrap_or_default().to_string()))
}

fn restore_background(app: &App) {
    match saved_background(&app.bg_dir) {
        Some((bytes, content_type)) => apply_background(app, Some(Arc::new(bytes)), &content_type),
        None => apply_background(app, None, ""),
    };
}

fn handle_put_background(app: &App, out: &mut TcpStream, mut body: Body, name: &str) {
    let mut bytes = Vec::new();
    if body.by_ref().take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes).is_err() {
        return;
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        send_body(out, 413, "", b"");
        out.shutdown(Shutdown::Both).ok();
        return;
    }
    let (content_type, ext) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ("image/png", "png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ("image/jpeg", "jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("image/gif", "gif")
    } else {
        send_body(out, 400, "text/plain", b"hikari-vrm: background must be png, jpeg or gif\n");
        return;
    };
    let bytes = Arc::new(bytes);
    let _bg_guard = app.bg_lock.lock().unwrap_or_else(PoisonError::into_inner);
    let frames = apply_background(app, Some(bytes.clone()), content_type);
    if frames == 0 {
        send_body(out, 400, "", b"");
        return;
    }
    let file = format!("current.{}.{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(), ext);
    let kind = if content_type == "image/gif" { "gif" } else { "image" };
    let name = if name.is_empty() { file.clone() } else { name.to_string() };
    let meta = json!({ "kind": kind, "name": name, "updated": iso_now(), "file": file, "type": content_type, "frames": frames, "bytes": bytes.len() });
    let saved = fs::create_dir_all(&app.bg_dir).is_ok() && fs::write(app.bg_dir.join(&file), bytes.as_slice()).is_ok();
    if !saved || write_atomic(&app.bg_dir.join("background.json"), meta.to_string().as_bytes()).is_err() {
        fs::remove_file(app.bg_dir.join(&file)).ok();
        restore_background(app);
        send_body(out, 500, "text/plain", b"hikari-vrm: background save failed\n");
        return;
    }
    clear_bg_files(&app.bg_dir, &file);
    send_body(out, 200, "application/json", meta.to_string().as_bytes());
    println!("hikari-vrm: background {} {} frames={} bytes={}", kind, name, frames, bytes.len());
}

fn handle_background(app: &App, req: &Request, body: Body, out: &mut TcpStream, ready: bool) {
    if req.method == "GET" && req.path == "/background" {
        send_body(out, 200, "application/json", read_bg_meta(&app.bg_dir).to_string().as_bytes());
        return;
    }
    if req.method == "GET" {
        match saved_background(&app.bg_dir) {
            Some((bytes, content_type)) => send_body(out, 200, &content_type, &bytes),
            None => send_body(out, 404, "", b""),
        }
        return;
    }
    if !ready {
        send_body(out, 503, "", b"");
        return;
    }
    if req.method == "PUT" && req.path == "/background" {
        handle_put_background(app, out, body, &param(req, "name", ""));
        return;
    }
    if req.method == "DELETE" && req.path == "/background" {
        let _bg_guard = app.bg_lock.lock().unwrap_or_else(PoisonError::into_inner);
        apply_background(app, None, "");
        let meta = json!({ "kind": "none", "name": null, "updated": iso_now() });
        if !(fs::create_dir_all(&app.bg_dir).is_ok() && write_atomic(&app.bg_dir.join("background.json"), meta.to_string().as_bytes()).is_ok()) {
            restore_background(app);
            send_body(out, 500, "text/plain", b"hikari-vrm: background save failed\n");
            return;
        }
        clear_bg_files(&app.bg_dir, "");
        send_body(out, 200, "application/json", meta.to_string().as_bytes());
        return;
    }
    send_body(out, 400, "", b"");
}

fn handle_idle(app: &App, out: &mut TcpStream, fps: f64, size: (usize, usize), codec_id: u8) {
    let key = (size, codec_id);
    let (picture, ticked) = &app.picture;
    let mut seen = {
        let mut shown = picture.lock().unwrap_or_else(PoisonError::into_inner);
        shown.wants.push((size, codec_id, fps));
        shown.seq
    };
    out.write_all(if codec_id == MJPEG { MJPEG_HEAD } else { STREAM_HEAD }).ok();
    let t0 = Instant::now();
    let mut k = 0u64;
    let mut behind_max = 0.0f64;
    loop {
        let pts = (k as f64 * 1000.0 / fps).floor();
        wait_until(app, t0 + Duration::from_secs_f64(pts / 1000.0));
        let mut shown = picture.lock().unwrap_or_else(PoisonError::into_inner);
        while shown.seq == seen || !shown.frames.contains_key(&key) {
            shown = ticked.wait_timeout(shown, Duration::from_secs(1)).unwrap_or_else(PoisonError::into_inner).0;
        }
        seen = shown.seq;
        let (data, head) = (shown.frames[&key].clone(), shown.head);
        drop(shown);
        if !write_frame(out, pts, &head, codec_id, &data) {
            break;
        }
        behind_max = behind_max.max(t0.elapsed().as_secs_f64() * 1000.0 - pts);
        k = (k + 1).max((t0.elapsed().as_secs_f64() * fps) as u64);
    }
    let mut shown = picture.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(index) = shown.wants.iter().position(|want| want.0 == size && want.1 == codec_id && want.2 == fps) {
        shown.wants.swap_remove(index);
    }
    drop(shown);
    out.write_all(if codec_id == MJPEG { b"--frame--\r\n".as_slice() } else { b"0\r\n\r\n".as_slice() }).ok();
    println!("hikari-vrm: fps={} frames={} behind_ms_max={}", fps, k, behind_max.round());
}

fn run_picture(app: Arc<App>) {
    let mut last = Instant::now();
    let (mut start, mut ticks, mut paced) = (last, 0u32, 0.0f64);
    let (mut raw, mut wants, mut spare) = (Vec::new(), Arc::new(Vec::new()), HashMap::new());
    loop {
        let fps = {
            let picture = app.picture.0.lock().unwrap_or_else(PoisonError::into_inner);
            if picture.wants != raw {
                raw.clone_from(&picture.wants);
                let mut sorted: Vec<((usize, usize), u8)> = raw.iter().map(|want| (want.0, want.1)).collect();
                sorted.sort();
                sorted.dedup();
                wants = Arc::new(sorted);
            }
            picture.wants.iter().map(|want| want.2).fold(picture.window_fps, f64::max)
        };
        if fps <= 0.0 {
            thread::sleep(Duration::from_millis(20));
            last = Instant::now();
            paced = 0.0;
            continue;
        }
        ticks += 1;
        let mut deadline = start + Duration::from_secs_f64(ticks as f64 / fps);
        if fps != paced || deadline + Duration::from_secs_f64(1.0 / fps) < Instant::now() {
            (start, ticks, paced) = (last, 1, fps);
            deadline = start + Duration::from_secs_f64(1.0 / fps);
        }
        wait_until(&app, deadline);
        last = Instant::now();
        let (reply, frames) = channel();
        app.jobs.send(Job::Tick { wants: wants.clone(), frames: spare, reply }).unwrap();
        let (encoded, head) = frames.recv().unwrap();
        let mut picture = app.picture.0.lock().unwrap_or_else(PoisonError::into_inner);
        picture.seq += 1;
        picture.head = head;
        spare = std::mem::replace(&mut picture.frames, encoded);
        drop(picture);
        app.picture.1.notify_all();
        spare.clear();
    }
}

fn route(app: &App, req: &Request, body: Body, out: &mut TcpStream) {
    let save = match bool_param(req, "save") {
        Ok(save) => save == Some(true),
        Err(line) => {
            send_body(out, 400, "text/plain", line.as_bytes());
            return;
        }
    };
    let fps = fps_param(req);
    let codec_name = param(req, "codec", "jpeg");
    let codec_id = CODECS.iter().position(|name| *name == codec_name).map(|id| id as u8);
    let codec_ok = codec_id.is_some();
    let ready = app.ready.load(Ordering::SeqCst);
    let invalid = if ready { 400 } else { 503 };
    let streams = matches!((req.method.as_str(), req.path.as_str()), ("POST", "/talk") | ("GET", "/listen"));
    let size = match size_param(req, app.size) {
        Ok(_) if codec_name == "split" => SPLIT_SIZE,
        Ok(size) => size,
        Err(line) if streams => {
            send_body(out, 400, "text/plain", line.as_bytes());
            return;
        }
        Err(_) => app.size,
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/models") => {
            if !ready {
                send_body(out, 503, "", b"");
                return;
            }
            let broken = app.broken.lock().unwrap_or_else(PoisonError::into_inner);
            let names: Vec<&String> = app.models.keys().filter(|name| !broken.contains(name)).collect();
            send_body(out, 200, "application/json", json!({ "model": app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone(), "models": names }).to_string().as_bytes());
        }
        ("POST", "/talk") => {
            let fps = match fps {
                Ok(fps) => fps,
                Err(line) => {
                    send_body(out, 400, "text/plain", line.as_bytes());
                    return;
                }
            };
            if !ready || !codec_ok {
                send_body(out, invalid, "", b"");
                return;
            }
            let mut body = body;
            match read_wav_header(&mut body) {
                Ok((rate, channels, limit)) => handle_render(app, out, body.take(limit), fps, (rate, channels), size, &codec_name, codec_id.unwrap()),
                Err(line) => {
                    println!("{}", line);
                    send_body(out, 400, "text/plain", format!("{}\n", line).as_bytes());
                }
            }
        }
        ("POST", "/state") => post_state(app, req, out),
        ("GET", "/state") => {
            let states = app.model.lock().unwrap_or_else(PoisonError::into_inner).states.clone();
            let state = app.state.lock().unwrap_or_else(PoisonError::into_inner).clone();
            send_body(out, 200, "application/json", json!({ "face": state.face, "pose": state.pose, "asleep": app.asleep.load(Ordering::SeqCst), "head_lead_ms": app.head_lead_ms.load(Ordering::SeqCst), "states": states }).to_string().as_bytes());
        }
        ("POST", "/action") => {
            if !ready {
                send_body(out, 503, "", b"");
                return;
            }
            handle_action(app, out, &param(req, "action", ""));
        }
        ("GET", "/action") => {
            let actions = app.model.lock().unwrap_or_else(PoisonError::into_inner).actions.clone();
            let idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner).actions.clone();
            send_body(out, 200, "application/json", json!({ "actions": actions, "idle": idle }).to_string().as_bytes());
        }
        ("POST", "/params") => {
            if !ready {
                send_body(out, 503, "", b"");
                return;
            }
            post_params(app, body, out);
        }
        ("GET", "/settings") => send_body(out, 200, "application/json", settings_json(app).to_string().as_bytes()),
        ("PUT", "/settings") => put_settings(app, body, out),
        (_, "/background") | (_, "/background/file") => handle_background(app, req, body, out, ready),
        ("GET", "/listen") => {
            let fps = match fps {
                Ok(fps) => fps,
                Err(line) => {
                    send_body(out, 400, "text/plain", line.as_bytes());
                    return;
                }
            };
            if !ready || !codec_ok {
                send_body(out, invalid, "", b"");
                return;
            }
            handle_idle(app, out, fps, size, codec_id.unwrap());
        }
        ("PUT", "/view") => {
            if !ready {
                send_body(out, 503, "", b"");
                return;
            }
            put_view(app, req, out, save);
        }
        ("GET", "/view") => {
            let name = app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone();
            let views = app.views.lock().unwrap_or_else(PoisonError::into_inner);
            let view = views.live.get(&name).or(views.saved.get(&name)).copied().unwrap_or(DEFAULT_VIEW);
            let saved = views.saved.get(&name).map(|v| json!({ "zoom": v[0], "x": v[1], "y": v[2], "rot": v[3] }));
            let default = DEFAULT_VIEW;
            send_body(out, 200, "application/json", json!({ "zoom": view[0], "x": view[1], "y": view[2], "rot": view[3], "saved": saved, "default": { "zoom": default[0], "x": default[1], "y": default[2], "rot": default[3] } }).to_string().as_bytes());
        }
        _ => send_body(out, 404, "", b""),
    }
}

fn wait_until(app: &App, deadline: Instant) {
    let mut event = KEvent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: std::ptr::null_mut() };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let timeout = TimeSpec { tv_sec: left.as_secs() as i64, tv_nsec: left.subsec_nanos() as i64 };
        unsafe { kevent(app.timer_queue, std::ptr::null(), 0, &mut event, 1, &timeout) };
    }
}

fn read_head_line(reader: &mut BufReader<Timed>, line: &mut String, left: &mut u64) -> io::Result<usize> {
    let n = reader.by_ref().take(*left).read_line(line)?;
    *left -= n as u64;
    if *left == 0 && !line.ends_with('\n') {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    Ok(n)
}

fn parse_request(reader: &mut BufReader<Timed>) -> io::Result<Request> {
    let mut left = MAX_HEAD_BYTES;
    let mut line = String::new();
    read_head_line(reader, &mut line, &mut left)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(io::ErrorKind::InvalidData)?.to_string();
    let target = parts.next().ok_or(io::ErrorKind::InvalidData)?;
    let (path, query_text) = target.split_once('?').unwrap_or((target, ""));
    let mut query = Vec::new();
    for pair in query_text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let mut decoded = Vec::new();
        for text in [key, value] {
            let bytes = text.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'+' {
                    out.push(b' ');
                    i += 1;
                    continue;
                }
                if bytes[i] == b'%' && i + 3 <= bytes.len() {
                    if let (Some(high), Some(low)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16)) {
                        out.push((high * 16 + low) as u8);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            decoded.push(String::from_utf8_lossy(&out).into_owned());
        }
        let value = decoded.pop().unwrap();
        let key = decoded.pop().unwrap();
        query.push((key, value));
    }
    let mut headers = Vec::new();
    loop {
        let mut header_line = String::new();
        if read_head_line(reader, &mut header_line, &mut left)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if header_line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = header_line.split_once(':') {
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Ok(Request { method, path: path.to_string(), query, headers })
}

fn handle_connection(app: Arc<App>, mut out: TcpStream) {
    out.set_nodelay(true).ok();
    out.set_write_timeout(Some(STREAM_IDLE)).ok();
    let Ok(read_half) = out.try_clone() else { return };
    let Ok(drain) = out.try_clone() else { return };
    let mut reader = BufReader::new(Timed { stream: read_half, deadline: Some(Instant::now() + HEAD_DEADLINE) });
    let req = match parse_request(&mut reader) {
        Ok(req) => req,
        Err(error) => {
            if error.kind() == io::ErrorKind::FileTooLarge {
                send_body(&mut out, 431, "", b"");
            }
            return;
        }
    };
    let streams = matches!((req.method.as_str(), req.path.as_str()), ("POST", "/talk") | ("GET", "/listen"));
    reader.get_mut().deadline = if streams { None } else { Some(Instant::now() + BODY_DEADLINE) };
    if header(&req, "expect").eq_ignore_ascii_case("100-continue") {
        out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok();
    }
    let chunked = header(&req, "transfer-encoding").to_ascii_lowercase().contains("chunked");
    let remaining = if chunked { 0 } else { header(&req, "content-length").parse().unwrap_or(0) };
    let body = Body { reader, remaining, chunked, done: false };
    route(&app, &req, body, &mut out);
    out.shutdown(Shutdown::Write).ok();
    io::copy(&mut Timed { stream: drain, deadline: Some(Instant::now() + DRAIN_DEADLINE) }, &mut io::sink()).ok();
}

fn load_background(engine: *mut engine_ffi::Engine, bytes: Option<&[u8]>, content_type: &str) -> i32 {
    let Some(bytes) = bytes else {
        unsafe { engine_ffi::engine_background_clear(engine) };
        return 1;
    };
    let Ok(expected) = CString::new(content_type) else { return 0 };
    unsafe { engine_ffi::engine_background_load(engine, bytes.as_ptr(), bytes.len(), expected.as_ptr()) }
}

fn sync_state(app: &App, lane: &mut LaneState) {
    let current = app.state.lock().unwrap_or_else(PoisonError::into_inner);
    if lane.state_generation == current.generation {
        return;
    }
    let state = current.clone();
    drop(current);
    lane.state_generation = state.generation;
    lane.behavior.shown = [state.face, state.pose];
}

fn load_views(path: &Path) -> HashMap<String, [f32; 4]> {
    let mut saved = HashMap::new();
    let Ok(text) = fs::read_to_string(path) else { return saved };
    let root: Value = match serde_json::from_str(&text) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("hikari-vrm: views.json {}", error);
            return saved;
        }
    };
    let Some(rows) = root.as_object() else {
        eprintln!("hikari-vrm: views.json must be an object of model names, ignoring it");
        return saved;
    };
    let mut invalid = Vec::new();
    for (name, row) in rows {
        let view = ["zoom", "x", "y", "rot"].map(|key| row[key].as_f64().unwrap_or(f64::NAN) as f32);
        if view.iter().all(|v| v.is_finite()) && view[0] > 0.0 {
            saved.insert(name.clone(), view);
        } else {
            invalid.push(name.as_str());
        }
    }
    if !invalid.is_empty() {
        eprintln!("hikari-vrm: views.json skipped invalid views for {}", invalid.join(", "));
    }
    saved
}

fn save_view(views: &mut Views, changes: &[(String, [f32; 4])]) -> Result<(), String> {
    let mut saved = views.saved.clone();
    saved.extend(changes.iter().cloned());
    let rows: serde_json::Map<String, Value> = saved.iter().map(|(name, v)| (name.clone(), json!({ "zoom": v[0], "x": v[1], "y": v[2], "rot": v[3] }))).collect();
    write_atomic(&views.path, Value::Object(rows).to_string().as_bytes())?;
    views.saved = saved;
    for (name, _) in changes {
        println!("hikari-vrm: saved view for {}", name);
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let mut temp = path.to_path_buf();
    let mut file = None;
    for _ in 0..100 {
        let save = SAVES.fetch_add(1, Ordering::Relaxed) + 1;
        temp = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), save));
        match fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(created) => {
                file = Some(created);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => break,
        }
    }
    let Some(mut file) = file else {
        let line = format!("hikari-vrm: can't write {} (no temp file)", path.display());
        eprintln!("{}", line);
        return Err(line);
    };
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()).and_then(|_| fs::rename(&temp, path)) {
        fs::remove_file(&temp).ok();
        let line = format!("hikari-vrm: can't write {} ({})", path.display(), error);
        eprintln!("{}", line);
        return Err(line);
    }
    Ok(())
}

fn sync_idle(app: &App, state: &mut LaneState) {
    let idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner);
    if state.idle_generation != idle.generation {
        if let Ok(every) = parse_every(&idle.every) {
            state.behavior.set_actions(&idle.actions, every);
        }
        state.idle_generation = idle.generation;
    }
}

fn parse_every(value: &Value) -> Result<(u64, u64), String> {
    let items = match value {
        Value::Array(items) => items.clone(),
        Value::Number(_) => vec![value.clone(), value.clone()],
        _ => Vec::new(),
    };
    let seconds: Vec<u64> = items.iter().filter_map(Value::as_f64).filter(|seconds| *seconds > 0.0 && seconds.fract() == 0.0).map(|seconds| seconds as u64).collect();
    match (items.len(), seconds.as_slice()) {
        (2, &[min, max]) if min <= max => Ok((min, max)),
        _ => Err(format!("hikari-vrm: action_every must be whole seconds above 0 or [min, max], got {}", value)),
    }
}

fn parse_idle(idle_actions: Option<&Value>, action_every: Option<&Value>, actions: &[String], every: &Value, valid: &[String]) -> Result<(Vec<String>, Value, Vec<String>, Vec<String>), String> {
    let names: Vec<String> = match idle_actions {
        None => actions.to_vec(),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => items.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(other) => return Err(format!("hikari-vrm: idle_actions in settings.json must be a list of action names, got {}", other)),
    };
    let every = action_every.unwrap_or(every).clone();
    parse_every(&every)?;
    let (banned, names): (Vec<String>, Vec<String>) = names.into_iter().partition(|name| name == "sleep");
    let (known, unknown) = names.into_iter().partition(|name| valid.contains(name));
    Ok((known, every, unknown, banned))
}

fn settings_json(app: &App) -> Value {
    let idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner);
    json!({ "model": app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone(), "size": [app.size.0, app.size.1], "idle_actions": idle.actions, "action_every": idle.every, "face": idle.face, "idle_loop": idle.idle_loop, "rest_angle": app.rest_angle.load(Ordering::SeqCst) })
}

fn put_settings(app: &App, body: Body, out: &mut TcpStream) {
    let Some(fields) = read_json_object(body, out, b"hikari-vrm: PUT /settings needs a JSON object\n") else { return };
    let _settings_guard = app.settings_lock.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(key) = fields.keys().find(|key| ["size", "port", "idle_loop"].contains(&key.as_str())) {
        send_body(out, 400, "text/plain", format!("hikari-vrm: {} is restart-only, edit settings.json and restart\n", key).as_bytes());
        return;
    }
    if let Some(key) = fields.keys().find(|key| !["idle_actions", "action_every", "model", "face", "rest_angle"].contains(&key.as_str())) {
        send_body(out, 400, "text/plain", format!("hikari-vrm: unknown setting {}\n", key).as_bytes());
        return;
    }
    let rest_angle = match fields.get("rest_angle") {
        None => None,
        Some(Value::Number(angle)) if angle.as_u64().is_some_and(|angle| angle <= REST_ANGLE_MAX) => angle.as_u64(),
        Some(other) => {
            send_body(out, 400, "text/plain", format!("hikari-vrm: rest_angle must be an integer 0..{}, got {}\n", REST_ANGLE_MAX, other).as_bytes());
            return;
        }
    };
    let rest_only = rest_angle.is_some() && fields.len() == 1;
    let model = match fields.get("model") {
        None => None,
        Some(Value::String(name)) if app.models.contains_key(name) => Some(name.clone()),
        Some(other) => {
            send_body(out, 400, "text/plain", format!("hikari-vrm: unknown model {}\n", other).as_bytes());
            return;
        }
    };
    let current = app.model.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let switch_to = model.filter(|name| *name != current.name);
    let _bg_guard = switch_to.as_ref().map(|_| app.bg_lock.lock().unwrap_or_else(PoisonError::into_inner));
    let scene = match switch_to.as_ref().map(|name| build_scene(app, name)) {
        None => None,
        Some(Ok(scene)) => Some(scene),
        Some(Err(line)) => {
            send_body(out, 500, "text/plain", format!("{}\n", line).as_bytes());
            return;
        }
    };
    let target = scene.as_ref().map_or(&current, |scene| &scene.model);
    let (face, actions, every) = match save_settings(app, fields, target) {
        Ok(checked) => checked,
        Err((status, line)) => {
            if let Some(scene) = scene {
                println!("hikari-vrm: model {} dropped", scene.model.name);
            }
            send_body(out, status, "text/plain", line.as_bytes());
            return;
        }
    };
    if let Some(scene) = scene {
        let (reply, old) = channel();
        app.jobs.send(Job::Switch { scene, reply }).unwrap();
        if let Ok(old) = old.recv() {
            println!("hikari-vrm: model {} dropped", old.model.name);
        }
    }
    let mut idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner);
    println!("hikari-vrm: idle actions={} action_every={}", actions.join(","), every);
    idle.actions = actions;
    idle.every = every;
    if !rest_only {
        idle.generation += 1;
    }
    if let Some(angle) = rest_angle {
        app.rest_angle.store(angle, Ordering::SeqCst);
    }
    if let Some(face) = face {
        idle.face = face.clone();
        let mut state = app.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.face_given && state.face != face {
            state.face = face;
            state.generation += 1;
        }
    }
    drop(idle);
    send_body(out, 200, "application/json", settings_json(app).to_string().as_bytes());
}

fn save_settings(app: &App, fields: serde_json::Map<String, Value>, target: &Model) -> Result<(Option<String>, Vec<String>, Value), (u16, String)> {
    let (path, actions, every) = {
        let idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner);
        (idle.path.clone(), idle.actions.clone(), idle.every.clone())
    };
    let face = match fields.get("face") {
        None => None,
        Some(Value::String(name)) if target.states.contains(name) => Some(name.clone()),
        Some(other) => return Err((400, format!("hikari-vrm: unknown state {}\n", other.as_str().map_or(other.to_string(), str::to_string)))),
    };
    let (actions, every) = match parse_idle(fields.get("idle_actions"), fields.get("action_every"), &actions, &every, &target.actions) {
        Ok((_, _, _, banned)) if !banned.is_empty() => return Err((400, format!("hikari-vrm: {} can't be an idle action\n", banned[0]))),
        Ok((_, _, unknown, _)) if !unknown.is_empty() => return Err((400, format!("hikari-vrm: unknown idle actions {}\n", unknown.join(", ")))),
        Ok((actions, every, _, _)) => (actions, every),
        Err(line) => return Err((400, format!("{}\n", line))),
    };
    let mut file = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(fields)) => fields,
            _ => return Err((500, format!("hikari-vrm: {} is not a JSON object, not rewriting it\n", path.display()))),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err((500, format!("hikari-vrm: can't read {} ({}), not rewriting it\n", path.display(), error))),
    };
    file.extend(fields);
    let mut pretty = serde_json::to_string_pretty(&Value::Object(file)).unwrap_or_default();
    pretty.push('\n');
    write_atomic(&path, pretty.as_bytes()).map_err(|line| (500, format!("{}\n", line)))?;
    Ok((face, actions, every))
}

fn create_engine(path: &Path, size: (usize, usize)) -> Result<*mut engine_ffi::Engine, String> {
    let file_name = path.file_name().map_or(String::new(), |name| name.to_string_lossy().into_owned());
    let Ok(model) = CString::new(path.to_string_lossy().as_bytes()) else { return Err(format!("hikari-vrm: model {} has a NUL in its path", file_name)) };
    let mut error = [0 as c_char; 512];
    let engine = unsafe { engine_ffi::engine_create(model.as_ptr(), size.0 as i32, size.1 as i32, error.as_mut_ptr(), error.len() as i32) };
    if engine.is_null() {
        error[error.len() - 1] = 0;
        return Err(format!("hikari-vrm: model {} failed to load: {}", file_name, unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()));
    }
    Ok(engine)
}

fn build_state(app: &App, name: &str) -> Result<(Arc<EngineHandle>, LaneState), String> {
    let path = app.models.get(name).ok_or(format!("hikari-vrm: unknown model {}", name))?;
    let engine = Arc::new(EngineHandle(create_engine(path, app.size)?));
    let gpu = unsafe { CStr::from_ptr(engine_ffi::engine_gpu_name(engine.0)) }.to_string_lossy().into_owned();
    println!("hikari-vrm: page ready gpu={}", gpu);
    if let Some((bytes, content_type)) = app.background.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        load_background(engine.0, Some(&bytes), &content_type);
    }
    let views = app.views.lock().unwrap_or_else(PoisonError::into_inner);
    let view = views.live.get(name).or(views.saved.get(name)).copied().unwrap_or(DEFAULT_VIEW);
    drop(views);
    println!("hikari-vrm: view model={} zoom={} x={} y={} rot={}", name, view[0], view[1], view[2], view[3]);
    let idle_loop = app.idle.lock().unwrap_or_else(PoisonError::into_inner).idle_loop.clone();
    let mut behavior = Behavior::new(engine.0, app.motions.clone(), &idle_loop, app.rest_angle.load(Ordering::SeqCst) as f64);
    unsafe { engine_ffi::engine_view_set(engine.0, view[0], view[1], view[2], view[3]) };
    behavior.update(0.0, -1.0);
    Ok((engine, LaneState { behavior, view, idle_generation: 0, state_generation: 0 }))
}

fn build_scene(app: &App, name: &str) -> Result<Scene, String> {
    let started = Instant::now();
    let (engine, state) = match build_state(app, name) {
        Ok(built) => built,
        Err(line) => {
            eprintln!("{}", line);
            app.broken.lock().unwrap_or_else(PoisonError::into_inner).push(name.to_string());
            return Err(line);
        }
    };
    let mut model = describe_model(name, &state.behavior.expressions, &app.motions);
    let idle_loop = app.idle.lock().unwrap_or_else(PoisonError::into_inner).idle_loop.clone();
    let skeletal_idle = app.motions.iter().any(|motion| motion.name == idle_loop);
    model.actions.retain(|action| !(skeletal_idle && *action == idle_loop));
    for index in 0..unsafe { engine_ffi::engine_param_count(engine.0) } {
        let (mut minimum, mut maximum, mut default_value) = (0.0f32, 0.0f32, 0.0f32);
        let id = unsafe { engine_ffi::engine_param_at(engine.0, index, &mut minimum, &mut maximum, &mut default_value) };
        if !id.is_null() {
            model.params.push((unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned(), minimum, maximum));
        }
    }
    println!("hikari-vrm: model {} ready ms={} params={} expressions={}", name, started.elapsed().as_millis(), model.params.len(), model.states.len());
    Ok(Scene { model, engine, state, asleep: false })
}

impl Scene {
    fn render(&self, size: (usize, usize)) -> Cow<'_, [u8]> {
        let view = self.state.view;
        let pixels = unsafe { engine_ffi::engine_render(self.engine.0, size.0 as i32, size.1 as i32, view[0], view[1], view[2], view[3], self.state.behavior.bg_clock_ms as f32) };
        if pixels.is_null() {
            eprintln!("hikari-vrm: render {}x{} failed", size.0, size.1);
            return Cow::Owned(vec![0; size.0 * size.1 * 4]);
        }
        Cow::Borrowed(unsafe { std::slice::from_raw_parts(pixels, size.0 * size.1 * 4) })
    }

    fn prepare(&mut self, app: &App) {
        let name = self.model.name.clone();
        let state = &mut self.state;
        sync_idle(app, state);
        sync_state(app, state);
        state.behavior.head_lead_ms = app.head_lead_ms.load(Ordering::SeqCst) as f32;
        state.behavior.hold_params(&app.params.lock().unwrap_or_else(PoisonError::into_inner));
        state.behavior.rest_angle = app.rest_angle.load(Ordering::SeqCst) as f64;
        let views = app.views.lock().unwrap_or_else(PoisonError::into_inner);
        state.view = views.live.get(&name).or(views.saved.get(&name)).copied().unwrap_or(DEFAULT_VIEW);
    }

    fn sleep(&mut self, app: &App) -> Value {
        if self.asleep {
            return json!({ "action": "sleep", "motion": "sleep", "expression": null });
        }
        let played = self.state.behavior.sleep_begin();
        if played.is_object() {
            self.asleep = true;
            app.asleep.store(true, Ordering::SeqCst);
            println!("hikari-vrm: asleep");
        }
        played
    }

    fn wake(&mut self, app: &App) {
        if !std::mem::take(&mut self.asleep) {
            return;
        }
        self.state.behavior.sleep_end();
        app.asleep.store(false, Ordering::SeqCst);
        println!("hikari-vrm: wake");
    }
}

fn run_render(app: Arc<App>, jobs: Receiver<Job>, port: u16) {
    let wanted = app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone();
    let Ok(mut scene) = build_scene(&app, &wanted) else { std::process::exit(1) };
    *app.model.lock().unwrap_or_else(PoisonError::into_inner) = scene.model.clone();
    let mut idle = app.idle.lock().unwrap_or_else(PoisonError::into_inner);
    let (known, unknown): (Vec<String>, Vec<String>) = idle.actions.iter().cloned().partition(|name| scene.model.actions.contains(name));
    if !unknown.is_empty() {
        eprintln!("hikari-vrm: settings.json idle_actions skipped unknown {}", unknown.join(", "));
    }
    println!("hikari-vrm: idle actions={} action_every={}", known.join(","), idle.every);
    idle.actions = known;
    idle.generation += 1;
    if idle.face != "none" && !scene.model.states.contains(&idle.face) {
        println!("hikari-vrm: settings.json face {} is not an expression of {}, resting face none", idle.face, wanted);
        idle.face = "none".to_string();
        app.state.lock().unwrap_or_else(PoisonError::into_inner).face = "none".to_string();
    }
    drop(idle);
    let mut shared = app.shared.lock().unwrap_or_else(PoisonError::into_inner);
    shared.engine = Some(scene.engine.clone());
    shared.name = scene.model.name.clone();
    drop(shared);
    app.ready.store(true, Ordering::SeqCst);
    println!("hikari-vrm: listening 127.0.0.1:{} models={} model={} core={}", port, app.models.len(), scene.model.name, hv::VERSION);
    let (mut advanced, mut swept) = (Instant::now(), Instant::now());
    loop {
        let job = match jobs.recv_timeout(TARGET_SWEEP.saturating_sub(swept.elapsed())) {
            Ok(job) => Some(job),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if swept.elapsed() >= TARGET_SWEEP {
            unsafe { engine_ffi::engine_sweep(scene.engine.0) };
            swept = Instant::now();
        }
        let Some(job) = job else { continue };
        let now = Instant::now();
        match job {
            Job::Tick { wants, mut frames, reply } => {
                let mouth = app.mouths.lock().unwrap_or_else(PoisonError::into_inner).mouth_now(now);
                scene.prepare(&app);
                let mut shared = app.shared.lock().unwrap_or_else(PoisonError::into_inner);
                scene.state.behavior.update(now.duration_since(advanced).min(MAX_DT).as_secs_f32() * 1000.0, mouth);
                advanced = now;
                shared.bg_clock_ms = scene.state.behavior.bg_clock_ms;
                drop(shared);
                let head = scene.state.behavior.head;
                for (index, &(size, _)) in wants.iter().enumerate() {
                    if index > 0 && wants[index - 1].0 == size {
                        continue;
                    }
                    let pixels = scene.render(size);
                    for &(_, codec_id) in wants.iter().filter(|want| want.0 == size) {
                        let data = match frames.get(&(size, if codec_id == MJPEG { JPEG } else { codec_id })) {
                            Some(data) => data.clone(),
                            None => Arc::new(codec::encode(&pixels, size.0, size.1, CODECS[codec_id as usize])),
                        };
                        frames.insert((size, codec_id), data);
                    }
                }
                reply.send((frames, head)).ok();
            }
            Job::Step { mouth, size, codec_id, reply } => {
                scene.prepare(&app);
                scene.state.behavior.update(now.duration_since(advanced).min(MAX_DT).as_secs_f32() * 1000.0, mouth);
                advanced = now;
                let data = codec::encode(&scene.render(size), size.0, size.1, CODECS[codec_id as usize]);
                reply.send((data, scene.state.behavior.head)).ok();
            }
            Job::React { kind, reply } => {
                if kind == "sleep" {
                    reply.send(scene.sleep(&app)).ok();
                    continue;
                }
                scene.wake(&app);
                reply.send(scene.state.behavior.react(&kind)).ok();
            }
            Job::Wake => scene.wake(&app),
            Job::Background { bytes, content_type, reply } => {
                let frames = load_background(scene.engine.0, bytes.as_deref().map(Vec::as_slice), &content_type);
                if frames > 0 {
                    *app.background.lock().unwrap_or_else(PoisonError::into_inner) = bytes.map(|bytes| (bytes, content_type));
                }
                reply.send(frames).ok();
            }
            Job::Switch { scene: next, reply } => {
                let model = next.model.name.clone();
                let mut shared = app.shared.lock().unwrap_or_else(PoisonError::into_inner);
                let old = std::mem::replace(&mut scene, next);
                shared.engine = Some(scene.engine.clone());
                shared.name = model.clone();
                shared.bg_clock_ms = 0.0;
                let mut current = app.model.lock().unwrap_or_else(PoisonError::into_inner);
                *current = scene.model.clone();
                app.params.lock().unwrap_or_else(PoisonError::into_inner).clear();
                drop(current);
                app.views.lock().unwrap_or_else(PoisonError::into_inner).live.remove(&model);
                let face = app.idle.lock().unwrap_or_else(PoisonError::into_inner).face.clone();
                let mut state = app.state.lock().unwrap_or_else(PoisonError::into_inner);
                *state = ModelState { face, pose: "none".to_string(), face_given: false, generation: state.generation + 1 };
                drop(state);
                app.asleep.store(false, Ordering::SeqCst);
                drop(shared);
                println!("hikari-vrm: model switched to {}", model);
                reply.send(old).ok();
            }
        }
    }
}

fn discover_models(exe_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut models = BTreeMap::new();
    for root in [exe_dir.join("models"), PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../models"))] {
        let Ok(files) = fs::read_dir(&root) else { continue };
        for file in files.flatten() {
            let file_name = file.file_name().to_string_lossy().into_owned();
            if !file_name.starts_with('.') && file_name.ends_with(".vrm") && file.path().is_file() {
                models.entry(file_name).or_insert(file.path());
            }
        }
    }
    models
}

fn load_motions(exe_dir: &Path) -> Vec<motion::Motion> {
    let built = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../actions"));
    let actions = if exe_dir.join("actions").is_dir() { exe_dir.join("actions") } else { built };
    let mut motions: Vec<motion::Motion> = Vec::new();
    for dir in [actions, exe_dir.join("animations")] {
        let mut paths: Vec<PathBuf> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|extension| extension == "vrma")).collect();
        paths.sort();
        let mut loaded = 0;
        for path in paths {
            match vrma::load(&path) {
                Ok(clip) => {
                    motions.retain(|motion| motion.name != clip.name);
                    motions.push(clip);
                    loaded += 1;
                }
                Err(line) => eprintln!("hikari-vrm: skipped clip {}", line),
            }
        }
        println!("hikari-vrm: clips dir={} files={}", dir.display(), loaded);
    }
    motions
}

fn read_settings(exe_dir: &Path) -> (Value, PathBuf) {
    let paths = [exe_dir.join("settings.json"), PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../settings.json"))];
    let Some((text, path)) = paths.iter().find_map(|path| fs::read_to_string(path).ok().map(|text| (text, path.clone()))) else { return (Value::Null, paths[0].clone()) };
    match serde_json::from_str(&text) {
        Ok(Value::Object(settings)) => (Value::Object(settings), path),
        Ok(_) => {
            eprintln!("hikari-vrm: {} must be a JSON object", path.display());
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("hikari-vrm: {} is not valid JSON ({})", path.display(), error);
            std::process::exit(1);
        }
    }
}

fn setting_size(settings: &Value, key: &str, default: (usize, usize)) -> (usize, usize) {
    let Some(value) = settings.get(key) else { return default };
    let parts = match value.as_array().map(Vec::as_slice) {
        Some([width, height]) => (width.as_u64(), height.as_u64()),
        _ => (None, None),
    };
    match parts {
        (Some(width), Some(height)) if (1..=3840).contains(&width) && (1..=2160).contains(&height) => (width as usize, height as usize),
        _ => {
            eprintln!("hikari-vrm: {} in settings.json must be [width, height] with positive integers up to [3840, 2160], got {}", key, value);
            std::process::exit(1);
        }
    }
}

fn setting_model(settings: &Value, models: &BTreeMap<String, PathBuf>) -> String {
    let first = models.keys().next().cloned().unwrap_or_default();
    match &settings["model"] {
        Value::Null => first,
        Value::String(name) if name.is_empty() => first,
        Value::String(name) if models.contains_key(name) => name.to_string(),
        other => {
            eprintln!("hikari-vrm: model {} in settings.json is not a .vrm in models/", other);
            std::process::exit(1);
        }
    }
}

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    run(std::env::current_exe().unwrap().parent().unwrap().to_path_buf(), &args);
}

#[no_mangle]
pub unsafe extern "C" fn hikari_start(data_directory: *const c_char, port: u16) {
    let exe_dir = PathBuf::from(unsafe { CStr::from_ptr(data_directory) }.to_string_lossy().into_owned());
    let args = ["hikari".to_string(), "--headless".to_string(), "--port".to_string(), port.to_string()];
    thread::spawn(move || run(exe_dir, &args));
}

#[no_mangle]
pub extern "C" fn hikari_stop() {
    if !STOPPED.swap(true, Ordering::SeqCst) {
        TcpStream::connect(("127.0.0.1", PORT.load(Ordering::SeqCst))).ok();
    }
}

fn run(exe_dir: PathBuf, args: &[String]) {
    let (settings, settings_path) = read_settings(&exe_dir);
    let mut port_arg = None;
    let mut window = true;
    let mut fps = 60u32;
    let mut share = true;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--port" => {
                let value = rest.next().cloned().unwrap_or_default();
                port_arg = port_arg.or(Some(value));
            }
            "--window" => window = true,
            "--headless" => window = false,
            "--fps" => match rest.next().and_then(|value| value.parse().ok()) {
                Some(value @ 1..=240) => fps = value,
                _ => {
                    eprintln!("hikari-vrm: --fps expects 1..=240");
                    std::process::exit(2);
                }
            },
            "--no-share" => share = false,
            other => {
                eprintln!("hikari-vrm: unknown argument {}", other);
                std::process::exit(2);
            }
        }
    }
    let port = match &settings["port"] {
        _ if port_arg.is_some() => {
            let text = port_arg.unwrap_or_default();
            match text.parse::<u16>() {
                Ok(port) if port != 0 && text.bytes().all(|byte| byte.is_ascii_digit()) => port,
                _ => {
                    eprintln!("hikari-vrm: --port must be 1 to 65535, got {}", text);
                    std::process::exit(2);
                }
            }
        }
        Value::Null => 8794,
        value => match value.as_u64().and_then(|port| u16::try_from(port).ok()).filter(|port| *port != 0) {
            Some(port) => port,
            None => {
                eprintln!("hikari-vrm: port in settings.json must be 1-65535, got {}", value);
                std::process::exit(1);
            }
        },
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("hikari-vrm: can't listen on port {} ({}); another hikari-vrm may be running, or set a free port in settings.json", port, error);
            std::process::exit(1);
        }
    };
    PORT.store(port, Ordering::SeqCst);
    let (jobs, job_queue) = channel();
    let built_bg_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../background"));
    let models = discover_models(&exe_dir);
    if models.is_empty() {
        eprintln!("hikari-vrm: no .vrm models in {}; copy a .vrm file there", exe_dir.join("models").display());
        std::process::exit(1);
    }
    let motions = load_motions(&exe_dir);
    let name = setting_model(&settings, &models);
    let requested: Vec<String> = settings["idle_actions"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    let (idle_actions, action_every, _, banned) = parse_idle(settings.get("idle_actions"), settings.get("action_every"), &[], &json!(15), &requested).unwrap_or_else(|line| {
        eprintln!("{}", line);
        std::process::exit(1);
    });
    for name in banned {
        println!("hikari-vrm: {} can't be an idle action", name);
    }
    let face = match &settings["face"] {
        Value::Null => "neutral".to_string(),
        Value::String(face) => face.clone(),
        other => {
            println!("hikari-vrm: settings.json face {} is not an expression name, using neutral", other);
            "neutral".to_string()
        }
    };
    let mut idle_loop = settings["idle_loop"].as_str().unwrap_or("").to_string();
    if !idle_loop.is_empty() && !motions.iter().any(|motion| motion.name == idle_loop) || !settings["idle_loop"].is_null() && !settings["idle_loop"].is_string() {
        println!("hikari-vrm: settings.json idle_loop {} is not an action file, using none", settings["idle_loop"]);
        idle_loop = String::new();
    }
    let idle_animation = match &settings["idle_animation"] {
        Value::Null => "idle".to_string(),
        Value::String(name) => name.strip_suffix(".vrma").unwrap_or(name).to_string(),
        other => other.to_string(),
    };
    if (idle_loop.is_empty() || !settings["idle_animation"].is_null()) && motions.iter().any(|motion| motion.name == idle_animation) {
        idle_loop = idle_animation;
    } else if !settings["idle_animation"].is_null() {
        eprintln!("hikari-vrm: settings.json idle_animation {} is not a loaded animations/*.vrma clip, using the built-in rest pose", settings["idle_animation"]);
    }
    let mut rest_angle = settings["rest_angle"].as_u64().unwrap_or(10);
    if settings.get("rest_angle").is_some() && (!settings["rest_angle"].is_u64() || rest_angle > REST_ANGLE_MAX) {
        println!("hikari-vrm: settings.json rest_angle {} is not an integer 0..{}, using 10", settings["rest_angle"], REST_ANGLE_MAX);
        rest_angle = 10;
    }
    let bg_dir = if built_bg_dir.is_dir() { built_bg_dir } else { exe_dir.join("background") };
    let now = Instant::now();
    let app = Arc::new(App {
        jobs,
        timer_queue: unsafe { kqueue() },
        ready: AtomicBool::new(false),
        model: Mutex::new(Model { name, states: Vec::new(), actions: Vec::new(), params: Vec::new() }),
        size: setting_size(&settings, "size", (960, 720)),
        idle: Mutex::new(IdleSettings { actions: idle_actions, every: action_every, face: face.clone(), idle_loop, generation: 1, path: settings_path }),
        models,
        broken: Mutex::new(Vec::new()),
        bg_lock: Mutex::new(()),
        settings_lock: Mutex::new(()),
        background: Mutex::new(saved_background(&bg_dir).map(|(bytes, content_type)| (Arc::new(bytes), content_type))),
        bg_dir,
        motions: Arc::new(motions),
        views: Mutex::new(Views { live: HashMap::new(), saved: load_views(&exe_dir.join("views.json")), path: exe_dir.join("views.json") }),
        mouths: Mutex::new(Mouths { owner: 0, t0: now, fps: 0.0, ring: [(usize::MAX, -1.0); MOUTHS] }),
        state: Mutex::new(ModelState { face, pose: "none".to_string(), face_given: false, generation: 1 }),
        asleep: AtomicBool::new(false),
        head_lead_ms: AtomicU64::new(0),
        params: Mutex::new(Vec::new()),
        rest_angle: AtomicU64::new(rest_angle),
        shared: Mutex::new(Shared { engine: None, name: String::new(), bg_clock_ms: 0.0 }),
        picture: (Mutex::new(Picture { seq: 0, head: [0; 4], frames: HashMap::new(), wants: Vec::new(), window_fps: 0.0 }), Condvar::new()),
        lane_locks: Default::default(),
        rendering: Default::default(),
    });
    let render_app = app.clone();
    thread::spawn(move || run_render(render_app, job_queue, port));
    let picture_app = app.clone();
    thread::spawn(move || run_picture(picture_app));
    if !window {
        accept_connections(&app, listener);
        return;
    }
    let accept_app = app.clone();
    thread::spawn(move || accept_connections(&accept_app, listener));
    run_window(&app, app.size, fps, share);
}

fn accept_connections(app: &Arc<App>, listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        if STOPPED.load(Ordering::SeqCst) {
            break;
        }
        let mut stream = stream;
        if CONNECTIONS.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
            stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
            send_body(&mut stream, 503, "", b"");
            continue;
        }
        let slot = Slot;
        let connection_app = app.clone();
        if thread::Builder::new().spawn(move || {
            let _slot = slot;
            handle_connection(connection_app, stream);
        }).is_err() {
            eprintln!("hikari-vrm: connection thread spawn failed");
        }
    }
}

extern "C" fn window_step(context: *mut c_void) {
    let frame = unsafe { &mut *context.cast::<WindowLoop>() };
    let shared = frame.app.shared.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.name != frame.name {
        frame.name = shared.name.clone();
        println!("hikari-vrm: window model={}", frame.name);
    }
    frame.engine = shared.engine.clone();
    let bg_clock_ms = shared.bg_clock_ms;
    drop(shared);
    let Some(engine) = frame.engine.as_ref().map(|engine| engine.0) else { return };
    let (mut pan_x, mut pan_y, mut zoom, mut cursor_x, mut cursor_y, mut turn, mut keys) = (0.0f32, 0.0f32, 1.0f32, 0.0f32, 0.0f32, 0.0f32, 0);
    let moved = unsafe { engine_ffi::window_input(frame.window, &mut pan_x, &mut pan_y, &mut zoom, &mut cursor_x, &mut cursor_y, &mut turn, &mut keys) };
    let mut views = frame.app.views.lock().unwrap_or_else(PoisonError::into_inner);
    let base = views.live.get(&frame.name).or(views.saved.get(&frame.name)).copied().unwrap_or(DEFAULT_VIEW);
    let mut view = base;
    if moved {
        let rot = (base[3] + turn * WINDOW_TURN_DEGREES * frame.last.elapsed().as_secs_f32().min(0.1) + 180.0).rem_euclid(360.0) - 180.0;
        view = [base[0] * zoom, zoom * (base[1] + pan_x) + (1.0 - zoom) * cursor_x * 0.5, zoom * (base[2] + pan_y) + (1.0 - zoom) * cursor_y * 0.5, rot];
        if keys & 1 != 0 {
            view = DEFAULT_VIEW;
        }
        if view != base {
            views.live.insert(frame.name.clone(), view);
            println!("hikari-vrm: view model={} zoom={} x={} y={} rot={}", frame.name, view[0], view[1], view[2], view[3]);
        }
        if keys & 2 != 0 {
            save_view(&mut views, &[(frame.name.clone(), view)]).ok();
        }
    }
    drop(views);
    unsafe { engine_ffi::engine_view_set(engine, view[0], view[1], view[2], view[3]) };
    let now = Instant::now();
    let mouth = frame.app.mouths.lock().unwrap_or_else(PoisonError::into_inner).mouth_now(now);
    frame.last = now;
    if mouth >= 0.0 {
        frame.mouth_frames += 1;
        frame.mouth_max = frame.mouth_max.max(mouth);
    }
    if unsafe { engine_ffi::engine_present(engine, frame.window, bg_clock_ms as f32, frame.frame_seconds, frame.share) } {
        frame.frames += 1;
    } else {
        thread::sleep(Duration::from_millis(10));
    }
    if now - frame.second >= Duration::from_secs(1) {
        println!("hikari-vrm: window size={}x{} fps={}", frame.width, frame.height, frame.frames);
        if frame.mouth_frames > 0 {
            println!("hikari-vrm: window mouth frames={} open_max={:.3}", frame.mouth_frames, frame.mouth_max);
        }
        frame.frames = 0;
        frame.mouth_frames = 0;
        frame.mouth_max = 0.0;
        frame.second = now;
    }
}

fn run_window(app: &App, size: (usize, usize), fps: u32, share: bool) {
    while !app.ready.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
    let name = app.model.lock().unwrap_or_else(PoisonError::into_inner).name.clone();
    let window = unsafe { engine_ffi::window_open(c"hikari-vrm".as_ptr(), size.0 as i32, size.1 as i32) };
    if window.is_null() {
        eprintln!("hikari-vrm: window_open failed");
        std::process::exit(1);
    }
    app.picture.0.lock().unwrap_or_else(PoisonError::into_inner).window_fps = fps as f64;
    let now = Instant::now();
    let mut frame = WindowLoop { app, name, engine: None, window, frame_seconds: 1.0 / fps as f32, last: now, second: now, frames: 0, mouth_frames: 0, mouth_max: 0.0, width: 0, height: 0, share };
    let frame_ptr = std::ptr::addr_of_mut!(frame);
    loop {
        while unsafe { engine_ffi::window_poll(window, std::ptr::addr_of_mut!((*frame_ptr).width), std::ptr::addr_of_mut!((*frame_ptr).height), window_step, frame_ptr.cast()) } {
            window_step(frame_ptr.cast());
        }
        let name = unsafe { (*frame_ptr).name.clone() };
        let views = app.views.lock().unwrap_or_else(PoisonError::into_inner);
        let view = views.live.get(&name).or(views.saved.get(&name)).copied().unwrap_or(DEFAULT_VIEW);
        let saved = views.saved.get(&name).copied().unwrap_or(DEFAULT_VIEW);
        drop(views);
        if view == saved {
            break;
        }
        let model = CString::new(name.as_str()).unwrap_or_default();
        match unsafe { engine_ffi::window_ask_save(window, model.as_ptr()) } {
            1 => {
                if save_view(&mut app.views.lock().unwrap_or_else(PoisonError::into_inner), &[(name, view)]).is_ok() {
                    break;
                }
            }
            0 => break,
            _ => {}
        }
    }
    unsafe { engine_ffi::window_close(window) };
    std::process::exit(0);
}
