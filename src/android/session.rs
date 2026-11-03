//! Launch scrcpy-server on the device, exchange the handshake, and keep the
//! video and control sockets running.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use arc_swap::ArcSwapOption;

use super::adb::Adb;
use super::control::ControlMessage;
use super::packet::{self, VideoHeader, CODEC_H264};
use crate::video::{Decoder, FrameStore};

/// scrcpy-server 5.x, embedded at build time by build.rs.
pub const SERVER_JAR: &[u8] = include_bytes!(env!("SCRCPY_SERVER_JAR_PATH"));
pub const SERVER_VERSION: &str = env!("SCRCPY_SERVER_VERSION");

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PACKET_BYTES: u32 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dims {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Streaming,
    Disconnected,
    Failed,
}

impl SessionState {
    fn as_u8(self) -> u8 {
        match self {
            Self::Connecting => 0,
            Self::Streaming => 1,
            Self::Disconnected => 2,
            Self::Failed => 3,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Streaming,
            2 => Self::Disconnected,
            3 => Self::Failed,
            _ => Self::Connecting,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Streaming => "streaming",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

pub type LogSink = Arc<dyn Fn(LogLevel, String) + Send + Sync>;

pub fn log_sink<F>(f: F) -> LogSink
where
    F: Fn(LogLevel, String) + Send + Sync + 'static,
{
    Arc::new(f)
}

#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    pub max_fps: u32,
    pub max_size: u32,
    pub video_bit_rate: u32,
    pub stay_awake: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            max_fps: 0,
            max_size: 0,
            video_bit_rate: 8_000_000,
            stay_awake: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub serial: String,
    pub device_name: String,
    pub state: &'static str,
    pub dims: Option<Dims>,
    pub uptime_s: u64,
    pub frame_age_ms: Option<u64>,
}

pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub age_ms: u64,
}

struct ControlWriter {
    stream: TcpStream,
    sequence: u64,
}

pub struct Session {
    pub serial: String,
    pub device_name: String,
    pub options: SessionOptions,
    pub started: Instant,
    pub(crate) dims: Arc<ArcSwapOption<Dims>>,
    pub(crate) frames: Arc<FrameStore>,
    pub(crate) state: Arc<AtomicU8>,
    control: Mutex<ControlWriter>,
    action_lock: Mutex<()>,
    clipboard_wait: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    log: LogSink,
    stop: Arc<AtomicBool>,
    shutdown_streams: Mutex<Vec<TcpStream>>,
    child: Mutex<Option<Child>>,
    cleanup: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    pending_clipboard: AtomicU64,
}

pub struct Handshake {
    pub video: TcpStream,
    pub control: TcpStream,
    pub device_name: String,
    pub codec_id: u32,
}

/// Cleans up a half-started session: kills the adb shell child and removes the
/// tunnel mapping unless ownership was transferred to a live Session.
#[derive(Default)]
struct SetupGuard {
    child: Option<Child>,
    cleanup: Vec<Box<dyn FnOnce() + Send>>,
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for cleanup in self.cleanup.drain(..) {
            cleanup();
        }
    }
}

impl Session {
    pub fn start(
        serial: String,
        adb: &Adb,
        options: SessionOptions,
        log: LogSink,
    ) -> Result<Arc<Session>> {
        let device_state = adb
            .device_state(&serial)
            .with_context(|| format!("device {serial} is not attached"))?;
        if !device_state.is_usable() {
            bail!(
                "device {serial} is {}; accept the USB debugging prompt on the device",
                device_state.as_str()
            );
        }

        let jar_path = ensure_local_jar()?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("bind local tunnel port")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let scid = random_scid();
        let socket_name = format!("scrcpy_{scid:08x}");

        // Anything below may fail half-way (push, server start, handshake). The
        // guard removes the tunnel mapping and kills the server shell on every
        // early return.
        let mut guard = SetupGuard::default();
        let mut listener = Some(listener);
        let mut tunnel_forward = false;
        let mut forward_port = port;

        match adb.reverse(&serial, &socket_name, port) {
            Ok(()) => {
                log(
                    LogLevel::Debug,
                    format!("reverse tunnel localabstract:{socket_name} -> tcp:{port}"),
                );
                let adb = adb.clone();
                let serial = serial.clone();
                let socket_name = socket_name.clone();
                guard.cleanup.push(Box::new(move || {
                    let _ = adb.reverse_remove(&serial, &socket_name);
                }));
            }
            Err(err) => {
                log(
                    LogLevel::Warn,
                    format!("adb reverse failed ({err}); falling back to adb forward"),
                );
                listener = None;
                forward_port = free_port()?;
                adb.forward(&serial, &socket_name, forward_port)
                    .context("set up adb forward tunnel")?;
                let adb = adb.clone();
                let serial = serial.clone();
                guard.cleanup.push(Box::new(move || {
                    let _ = adb.forward_remove(&serial, forward_port);
                }));
                tunnel_forward = true;
            }
        }

        let remote_jar = remote_jar_path();
        let already_pushed = adb
            .shell_stdout(
                &serial,
                &format!("stat -c %s {remote_jar}"),
                Duration::from_secs(5),
            )
            .map(|size| size.trim() == SERVER_JAR.len().to_string())
            .unwrap_or(false);
        if !already_pushed {
            adb.push(&serial, &jar_path, &remote_jar)
                .context("push scrcpy-server to the device")?;
        }

        let command = server_command(&remote_jar, scid, &options, tunnel_forward);
        log(LogLevel::Debug, format!("adb shell {command}"));
        let mut child = adb.spawn_shell(&serial, &command)?;

        let mut log_threads = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            log_threads.push(spawn_line_logger(stdout, LogLevel::Info, log.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            log_threads.push(spawn_line_logger(stderr, LogLevel::Warn, log.clone()));
        }
        guard.child = Some(child);

        let handshake = if tunnel_forward {
            handshake_forward(forward_port, HANDSHAKE_TIMEOUT)?
        } else {
            handshake_reverse(
                listener.as_ref().expect("reverse listener"),
                HANDSHAKE_TIMEOUT,
            )?
        };

        if handshake.codec_id != CODEC_H264 {
            bail!(
                "device selected unsupported codec {} (expected h264)",
                packet::codec_name(handshake.codec_id)
            );
        }

        let child = guard.child.take();
        let cleanup = std::mem::take(&mut guard.cleanup);
        let session = attach(
            serial,
            handshake.device_name,
            handshake.video,
            handshake.control,
            options,
            log,
            child,
            cleanup,
        )?;
        session.threads.lock().unwrap().extend(log_threads);
        Ok(session)
    }

    pub fn state(&self) -> SessionState {
        SessionState::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn dims(&self) -> Option<Dims> {
        self.dims.load_full().map(|dims| *dims)
    }

    pub fn frames(&self) -> &FrameStore {
        &self.frames
    }

    pub fn status(&self) -> Status {
        Status {
            serial: self.serial.clone(),
            device_name: self.device_name.clone(),
            state: self.state().as_str(),
            dims: self.dims(),
            uptime_s: self.started.elapsed().as_secs(),
            frame_age_ms: self.frames.latest().map(|frame| frame.age_ms()),
        }
    }

    pub fn send(&self, message: &ControlMessage) -> Result<()> {
        let bytes = message.encode()?;
        let mut control = self.control.lock().unwrap();
        control
            .stream
            .write_all(&bytes)
            .context("write control message")?;
        control.stream.flush().context("flush control message")?;
        Ok(())
    }

    /// Serializes composite gestures so concurrent tool calls cannot interleave.
    pub(crate) fn lock_actions(&self) -> std::sync::MutexGuard<'_, ()> {
        self.action_lock.lock().unwrap()
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        let mut control = self.control.lock().unwrap();
        control.sequence += 1;
        control.sequence
    }

    pub fn begin_clipboard_wait(&self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        *self.clipboard_wait.lock().unwrap() = Some(tx);
        self.pending_clipboard.fetch_add(1, Ordering::Relaxed);
        rx
    }

    pub fn cancel_clipboard_wait(&self) {
        *self.clipboard_wait.lock().unwrap() = None;
    }

    pub fn screenshot(&self) -> Result<Screenshot> {
        let frame = self
            .frames
            .latest()
            .ok_or_else(|| anyhow!("no video frame received yet; is the device still unlocked?"))?;
        let png = crate::video::encode_png(&frame)?;
        Ok(Screenshot {
            png,
            width: frame.width,
            height: frame.height,
            age_ms: frame.age_ms(),
        })
    }

    pub fn reset_video(&self) -> Result<()> {
        self.send(&ControlMessage::ResetVideo)
    }

    pub fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        self.state
            .store(SessionState::Disconnected.as_u8(), Ordering::Relaxed);
        for stream in self.shutdown_streams.lock().unwrap().iter() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for cleanup in self.cleanup.lock().unwrap().drain(..) {
            cleanup();
        }
        let threads: Vec<_> = self.threads.lock().unwrap().drain(..).collect();
        for thread in threads {
            let _ = thread.join();
        }
        (self.log)(LogLevel::Info, format!("session {} stopped", self.serial));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // stop() takes &self and joins threads; on drop the object is still alive.
        self.stop();
    }
}

/// Accepts the two sockets the server connects to in reverse-tunnel mode.
pub fn handshake_reverse(listener: &TcpListener, timeout: Duration) -> Result<Handshake> {
    let mut video = accept_with_timeout(listener, timeout)?;
    let control = accept_with_timeout(listener, timeout)?;
    video.set_read_timeout(Some(timeout))?;
    let device_name = read_device_name(&mut video)?;
    let codec_id = read_codec_id(&mut video)?;
    video.set_read_timeout(None)?;
    control.set_read_timeout(None)?;
    video.set_nodelay(true)?;
    control.set_nodelay(true)?;
    Ok(Handshake {
        video,
        control,
        device_name,
        codec_id,
    })
}

/// Connects the two sockets in forward-tunnel mode.
pub fn handshake_forward(port: u16, timeout: Duration) -> Result<Handshake> {
    let mut video = connect_with_retry(port, timeout)?;
    video.set_read_timeout(Some(timeout))?;
    let mut dummy = [0u8; 1];
    video
        .read_exact(&mut dummy)
        .context("read scrcpy-server dummy byte")?;
    let control = connect_with_retry(port, timeout)?;
    let device_name = read_device_name(&mut video)?;
    let codec_id = read_codec_id(&mut video)?;
    video.set_read_timeout(None)?;
    control.set_read_timeout(None)?;
    video.set_nodelay(true)?;
    control.set_nodelay(true)?;
    Ok(Handshake {
        video,
        control,
        device_name,
        codec_id,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn attach(
    serial: String,
    device_name: String,
    video: TcpStream,
    control: TcpStream,
    options: SessionOptions,
    log: LogSink,
    child: Option<Child>,
    cleanup: Vec<Box<dyn FnOnce() + Send>>,
) -> Result<Arc<Session>> {
    let mut guard = SetupGuard { child, cleanup };
    let control_read = control.try_clone().context("clone control socket")?;
    let shutdown_video = video.try_clone().context("clone video socket")?;
    let shutdown_control = control.try_clone().context("clone control socket")?;

    let frames = Arc::new(FrameStore::new());
    let dims = Arc::new(ArcSwapOption::empty());
    let state = Arc::new(AtomicU8::new(SessionState::Streaming.as_u8()));
    let stop = Arc::new(AtomicBool::new(false));
    let clipboard_wait = Arc::new(Mutex::new(None));

    let session = Arc::new(Session {
        serial,
        device_name,
        options,
        started: Instant::now(),
        dims: dims.clone(),
        frames: frames.clone(),
        state: state.clone(),
        control: Mutex::new(ControlWriter {
            stream: control,
            sequence: 1,
        }),
        action_lock: Mutex::new(()),
        clipboard_wait: clipboard_wait.clone(),
        log: log.clone(),
        stop: stop.clone(),
        shutdown_streams: Mutex::new(vec![shutdown_video, shutdown_control]),
        child: Mutex::new(guard.child.take()),
        cleanup: Mutex::new(std::mem::take(&mut guard.cleanup)),
        threads: Mutex::new(Vec::new()),
        pending_clipboard: AtomicU64::new(0),
    });

    let video_thread = spawn_video_reader(video, frames, dims, state, stop.clone(), log.clone());
    let control_thread = spawn_control_reader(control_read, clipboard_wait, stop, log.clone());
    session
        .threads
        .lock()
        .unwrap()
        .extend([video_thread, control_thread]);

    log(
        LogLevel::Info,
        format!(
            "session {} ({}) streaming",
            session.serial, session.device_name
        ),
    );
    Ok(session)
}

fn spawn_video_reader(
    mut stream: TcpStream,
    frames: Arc<FrameStore>,
    dims: Arc<ArcSwapOption<Dims>>,
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    log: LogSink,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("scrcpy-video".into())
        .spawn(move || {
            let mut decoder: Option<Decoder> = None;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut header = [0u8; packet::HEADER_SIZE];
                if let Err(err) = stream.read_exact(&mut header) {
                    if !stop.load(Ordering::Relaxed) {
                        log(LogLevel::Error, format!("video stream ended: {err}"));
                        state.store(SessionState::Disconnected.as_u8(), Ordering::Relaxed);
                    }
                    break;
                }
                match packet::parse_header(&header) {
                    VideoHeader::Session(meta) => {
                        if meta.width > 0 && meta.height > 0 {
                            dims.store(Some(Arc::new(Dims {
                                width: meta.width,
                                height: meta.height,
                            })));
                            log(
                                LogLevel::Debug,
                                format!("video session {}x{}", meta.width, meta.height),
                            );
                        }
                    }
                    VideoHeader::Media(meta) => {
                        if meta.size == 0 || meta.size > MAX_PACKET_BYTES {
                            log(
                                LogLevel::Error,
                                format!("implausible video packet size {}", meta.size),
                            );
                            break;
                        }
                        let mut payload = vec![0u8; meta.size as usize];
                        if let Err(err) = stream.read_exact(&mut payload) {
                            if !stop.load(Ordering::Relaxed) {
                                log(LogLevel::Error, format!("video stream truncated: {err}"));
                                state.store(SessionState::Disconnected.as_u8(), Ordering::Relaxed);
                            }
                            break;
                        }
                        // scrcpy sends a codec-config packet before the first
                        // key frame; decoding without any config also works for
                        // in-band SPS/PPS, so create the decoder lazily.
                        if meta.config || decoder.is_none() {
                            match Decoder::new() {
                                Ok(fresh) => decoder = Some(fresh),
                                Err(err) => {
                                    log(LogLevel::Error, format!("decoder init failed: {err}"));
                                    continue;
                                }
                            }
                        }
                        if let Some(decoder) = decoder.as_mut() {
                            match decoder.push(&payload) {
                                Ok(Some(frame)) => frames.store(frame),
                                Ok(None) => {}
                                Err(err) => log(LogLevel::Warn, format!("decode error: {err}")),
                            }
                        }
                    }
                }
            }
        })
        .expect("spawn video reader")
}

fn spawn_control_reader(
    mut stream: TcpStream,
    clipboard_wait: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    stop: Arc<AtomicBool>,
    log: LogSink,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("scrcpy-control".into())
        .spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let mut kind = [0u8; 1];
            if stream.read_exact(&mut kind).is_err() {
                break;
            }
            match kind[0] {
                packet::DEVICE_MSG_CLIPBOARD => {
                    let mut head = [0u8; 12];
                    if stream.read_exact(&mut head).is_err() {
                        break;
                    }
                    let len = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
                    let mut text = vec![0u8; len as usize];
                    if stream.read_exact(&mut text).is_err() {
                        break;
                    }
                    let text = String::from_utf8_lossy(&text).into_owned();
                    if let Some(sender) = clipboard_wait.lock().unwrap().take() {
                        let _ = sender.send(text);
                    } else {
                        log(LogLevel::Debug, "device clipboard changed".into());
                    }
                }
                packet::DEVICE_MSG_ACK_CLIPBOARD => {
                    let mut seq = [0u8; 8];
                    if stream.read_exact(&mut seq).is_err() {
                        break;
                    }
                }
                packet::DEVICE_MSG_UHID_OUTPUT => {
                    let mut size = [0u8; 2];
                    if stream.read_exact(&mut size).is_err() {
                        break;
                    }
                    let size = u16::from_be_bytes(size) as usize;
                    let mut discard = vec![0u8; size];
                    if stream.read_exact(&mut discard).is_err() {
                        break;
                    }
                }
                other => {
                    log(
                        LogLevel::Warn,
                        format!("unexpected device message type {other}"),
                    );
                    break;
                }
            }
        })
        .expect("spawn control reader")
}

fn spawn_line_logger(
    pipe: impl Read + Send + 'static,
    level: LogLevel,
    log: LogSink,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let text = line.trim_end();
                    if !text.is_empty() {
                        log(level, format!("[server] {text}"));
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn accept_with_timeout(listener: &TcpListener, timeout: Duration) -> Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for scrcpy-server to connect");
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => return Err(err).context("accept scrcpy connection"),
        }
    }
}

fn connect_with_retry(port: u16, timeout: Duration) -> Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err).context("connect to scrcpy-server");
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn read_device_name(stream: &mut TcpStream) -> Result<String> {
    let mut buf = [0u8; 64];
    stream
        .read_exact(&mut buf)
        .context("read device name from scrcpy-server")?;
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).trim().to_string())
}

fn read_codec_id(stream: &mut TcpStream) -> Result<u32> {
    let mut buf = [0u8; 4];
    stream
        .read_exact(&mut buf)
        .context("read codec id from scrcpy-server")?;
    Ok(u32::from_be_bytes(buf))
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn random_scid() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    ((nanos ^ (pid << 24) ^ (nanos >> 19)) as u32) & 0x7fff_ffff
}

pub fn remote_jar_path() -> String {
    format!("/data/local/tmp/scrcpy-agent-{SERVER_VERSION}.jar")
}

fn ensure_local_jar() -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("scrcpy-agent");
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("scrcpy-server-{SERVER_VERSION}.jar"));
    let needs_write = match fs::metadata(&path) {
        Ok(meta) => meta.len() != SERVER_JAR.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        fs::write(&path, SERVER_JAR).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(path)
}

fn server_command(
    remote_jar: &str,
    scid: u32,
    options: &SessionOptions,
    tunnel_forward: bool,
) -> String {
    format!(
        "CLASSPATH={remote_jar} app_process / com.genymobile.scrcpy.Server {version} \
         scid={scid:08x} log_level=info video=true audio=false control=true \
         max_size={max_size} max_fps={max_fps} video_bit_rate={bit_rate} \
         tunnel_forward={tunnel_forward} send_dummy_byte=true send_device_meta=true \
         send_frame_meta=true send_stream_meta=true cleanup=true power_on=true \
         clipboard_autosync=false stay_awake={stay_awake}",
        version = SERVER_VERSION,
        scid = scid,
        max_size = options.max_size,
        max_fps = options.max_fps,
        bit_rate = options.video_bit_rate,
        tunnel_forward = tunnel_forward,
        stay_awake = options.stay_awake,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_contains_required_options() {
        let command = server_command(
            "/data/local/tmp/x.jar",
            0x1234,
            &SessionOptions::default(),
            false,
        );
        assert!(command.contains("CLASSPATH=/data/local/tmp/x.jar"));
        assert!(command.contains("com.genymobile.scrcpy.Server 5.0.1"));
        assert!(command.contains("scid=00001234"));
        assert!(command.contains("clipboard_autosync=false"));
        assert!(command.contains("audio=false"));
    }

    #[test]
    fn session_state_roundtrip() {
        for state in [
            SessionState::Connecting,
            SessionState::Streaming,
            SessionState::Disconnected,
            SessionState::Failed,
        ] {
            assert_eq!(SessionState::from_u8(state.as_u8()), state);
        }
    }
}
