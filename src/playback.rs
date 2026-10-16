//! Bundled libmpv native GPU playback; software output is for previews and tests.
//! Seek previews use a separate bundled mpv instance, never the playback player.
use crate::logging::{Level, Print};
use libloading::Library;
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    path::PathBuf,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct AudioTrack {
    pub id: i64,
    pub title: String,
    /// Linear amplitude, not a percentage. Supported range is 0..=10.
    pub gain: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Fatal,
    Error,
    Warning,
    Info,
    Debug,
    Trace,
    Unknown,
}

impl DiagnosticSeverity {
    fn from_level(level: &str) -> Self {
        match level {
            "fatal" => Self::Fatal,
            "error" => Self::Error,
            "warn" => Self::Warning,
            "info" => Self::Info,
            "v" | "debug" => Self::Debug,
            "trace" => Self::Trace,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub prefix: String,
    pub message: String,
}

const MAX_DIAGNOSTICS: usize = 16;
const MAX_DIAGNOSTIC_CHARS: usize = 1024;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioOutputMode {
    #[default]
    Stereo,
    System,
    HeadsetCompatible,
}

#[cfg(test)]
impl AudioOutputMode {
    fn channels(self) -> &'static str {
        match self {
            Self::Stereo => "stereo",
            Self::System => "auto-safe",
            Self::HeadsetCompatible => "stereo",
        }
    }

    fn device(self) -> &'static str {
        match self {
            Self::HeadsetCompatible => "openal",
            _ => "auto",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub path: Option<PathBuf>,
    /// Latched only after decoder startup/restart completes during active play.
    pub playback_started: bool,
    pub duration: f64,
    pub position: f64,
    pub paused: bool,
    pub ended: bool,
    /// mpv volume percentage, 0..=100.
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub effective_muted: bool,
    #[cfg(test)]
    pub audio_output: AudioOutputMode,
    pub video_output: Option<String>,
    pub hardware_decoder: Option<String>,
    pub dropped_frames: Option<f64>,
    pub decoder_dropped_frames: Option<f64>,
    pub tracks: Vec<AudioTrack>,
    pub metadata: std::collections::BTreeMap<String, serde_json::Value>,
    /// Operation or fatal playback failure, not a decoder log message.
    pub error: Option<String>,
    /// Latest decoder/backend diagnostic for display in an information panel.
    pub diagnostic: Option<String>,
    /// Oldest first, at most 16 entries. Consecutive duplicates are coalesced.
    /// Prefixes are limited to 128 characters and messages to 1024 characters.
    pub diagnostics: Vec<Diagnostic>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            path: None,
            playback_started: false,
            duration: 0.0,
            position: 0.0,
            paused: true,
            ended: false,
            volume: 100.0,
            muted: false,
            speed: 1.0,
            effective_muted: true,
            #[cfg(test)]
            audio_output: AudioOutputMode::default(),
            video_output: None,
            hardware_decoder: None,
            dropped_frames: None,
            decoder_dropped_frames: None,
            tracks: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
            error: None,
            diagnostic: None,
            diagnostics: Vec::new(),
        }
    }
}

impl Snapshot {
    fn record_diagnostic(&mut self, level: &str, prefix: &str, message: &str) {
        let message: String = message.trim().chars().take(MAX_DIAGNOSTIC_CHARS).collect();
        if message.is_empty() {
            return;
        }
        let diagnostic = Diagnostic {
            severity: DiagnosticSeverity::from_level(level),
            prefix: prefix.trim().chars().take(128).collect(),
            message,
        };
        self.diagnostic = Some(if diagnostic.prefix.is_empty() {
            diagnostic.message.clone()
        } else {
            format!("{}: {}", diagnostic.prefix, diagnostic.message)
        });
        if self.diagnostics.last() == Some(&diagnostic) {
            return;
        }
        if self.diagnostics.len() == MAX_DIAGNOSTICS {
            self.diagnostics.remove(0);
        }
        self.diagnostics.push(diagnostic);
    }

    fn clear_diagnostics(&mut self) {
        self.diagnostic = None;
        self.diagnostics.clear();
    }
}

fn with_diagnostic(error: String, diagnostic: Option<&str>) -> String {
    match diagnostic {
        Some(message) => format!("{error}\nDecoder diagnostic: {message}"),
        None => error,
    }
}

fn end_file_error(
    reason: c_int,
    error: Option<String>,
    diagnostic: Option<&str>,
) -> Option<String> {
    // MPV_END_FILE_REASON_ERROR is authoritative even when no error code is supplied.
    (reason == 4).then(|| {
        with_diagnostic(
            error.unwrap_or_else(|| "Media playback failed".to_owned()),
            diagnostic,
        )
    })
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed, top-down BGRA8 with opaque alpha, ready for GPUI.
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct LoadState {
    pub volume: f64,
    pub muted: bool,
    pub gains: std::collections::BTreeMap<i64, f64>,
    pub position: f64,
    pub paused: bool,
}

#[derive(Clone, Debug)]
pub enum Command {
    Load(PathBuf),
    LoadSaved(PathBuf, LoadState),
    Toggle,
    Play,
    Pause,
    /// Rewind the current file to its beginning and pause; retain tracks and duration.
    Stop,
    SeekAbsolute(f64),
    SeekRelative(f64),
    FrameStep,
    FrameBackStep,
    SetVolume(f64),
    SetMute(bool),
    SetSpeed(f64),
    #[cfg(test)]
    SetAudioOutput(AudioOutputMode),
    SetTrackGain {
        id: i64,
        gain: f64,
    },
    Shutdown,
}

struct Mailbox {
    snapshot: Mutex<Snapshot>,
    frame: Mutex<Option<VideoFrame>>,
    render_error: Mutex<Option<String>>,
    size: AtomicU64,
    viewport: AtomicU64,
    generation: AtomicU64,
}

impl Mailbox {
    fn new() -> Self {
        Self {
            snapshot: Mutex::new(Snapshot::default()),
            frame: Mutex::new(None),
            render_error: Mutex::new(None),
            size: AtomicU64::new(0),
            viewport: AtomicU64::new(0),
            generation: AtomicU64::new(0),
        }
    }

    fn invalidate_frames(&self) {
        let mut frame = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.size.store(0, Ordering::Release);
        *frame = None;
    }

    fn publish_frame(&self, generation: u64, size: u64, frame: VideoFrame) {
        let mut slot = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::Acquire) == generation
            && self.size.load(Ordering::Acquire) == size
        {
            *slot = Some(frame);
        }
    }
}

pub struct Playback {
    commands: mpsc::SyncSender<Command>,
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
    native: bool,
}

impl Playback {
    #[cfg(test)]
    pub fn is_stopped(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }
    /// Loads libmpv and waits for both workers to initialize. No native window is created.
    #[cfg(test)]
    pub fn new() -> Result<Self, String> {
        Self::create(None)
    }

    /// The UI owns this HWND and must keep it alive until Playback is dropped.
    pub fn native(hwnd: usize) -> Result<Self, String> {
        Self::create(Some(hwnd))
    }

    fn create(hwnd: Option<usize>) -> Result<Self, String> {
        let mailbox = Arc::new(Mailbox::new());
        let (commands, receiver) = mpsc::sync_channel(128);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let shared = mailbox.clone();
        let worker = thread::Builder::new()
            .name("mpv-control".into())
            .spawn(move || match Player::new_output(shared.clone(), hwnd) {
                Ok(mut player) => {
                    if ready_tx.send(Ok(())).is_ok() {
                        player.run(receiver);
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                mailbox,
                worker: Some(worker),
                native: hwnd.is_some(),
            }),
            result => {
                let _ = worker.join();
                Err(match result {
                    Ok(Err(e)) => e,
                    _ => "Playback worker exited during initialization".into(),
                })
            }
        }
    }

    /// Nonblocking enqueue. Execution errors appear in `Snapshot::error`.
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands.try_send(command).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => "Playback command queue is full".into(),
            mpsc::TrySendError::Disconnected(_) => "Playback worker has stopped".into(),
        })
    }

    /// Display dimensions in physical pixels. Zero restores source-size rendering,
    /// subject to the 3840x2160 cap. The render worker redraws paused video too.
    #[cfg(test)]
    pub fn set_viewport(&self, width: u32, height: u32) {
        self.mailbox.viewport.store(
            if width == 0 || height == 0 {
                0
            } else {
                (u64::from(width) << 32) | u64::from(height)
            },
            Ordering::Release,
        );
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self
            .mailbox
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(error) = self
            .mailbox
            .render_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            snapshot.error = Some(with_diagnostic(error, snapshot.diagnostic.as_deref()));
        }
        snapshot
    }

    /// Takes the newest frame. Older unconsumed frames are replaced, not queued.
    #[cfg(test)]
    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.mailbox
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // Never lose shutdown to a full queue. Keep servicing native sent
        // messages if the worker is waiting on its UI-thread parent window.
        loop {
            match self.commands.try_send(Command::Shutdown) {
                Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => break,
                Err(mpsc::TrySendError::Full(_)) => {
                    service_native_messages(self.native);
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        if let Some(worker) = self.worker.take() {
            // Native VO teardown can synchronously message its UI-thread parent.
            // Service sent messages while joining, without dispatching queued input
            // or reentering GPUI controls during PlayerShell destruction.
            if self.native {
                while !worker.is_finished() {
                    service_native_messages(true);
                    thread::sleep(Duration::from_millis(1));
                }
            }
            let _ = worker.join();
        }
    }
}

fn service_native_messages(native: bool) {
    #[cfg(windows)]
    if native {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let mut message = std::mem::zeroed();
            PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
    }
    #[cfg(not(windows))]
    let _ = native;
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}
impl RenderParam {
    fn new(kind: c_int, data: *mut c_void) -> Self {
        Self { kind, data }
    }
    fn end() -> Self {
        Self::new(0, ptr::null_mut())
    }
}
#[repr(C)]
struct Event {
    id: c_int,
    error: c_int,
    userdata: u64,
    data: *mut c_void,
}
// Values and parameter types from the public libmpv render API ABI.
const RENDER_API_TYPE: c_int = 1;
const RENDER_SW_SIZE: c_int = 17; // int[2]
const RENDER_SW_FORMAT: c_int = 18; // NUL-terminated char*
const RENDER_SW_STRIDE: c_int = 19; // size_t*
const RENDER_SW_POINTER: c_int = 20; // pixel data itself, not void**

#[repr(C)]
struct Hook {
    name: *const c_char,
    id: u64,
}
#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}
#[repr(C)]
struct LogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}
#[repr(C)]
union NodeData {
    string: *mut c_char,
    flag: c_int,
    integer: i64,
    number: f64,
    list: *mut NodeList,
}
#[repr(C)]
struct Node {
    data: NodeData,
    format: c_int,
}
#[repr(C)]
struct NodeList {
    count: c_int,
    values: *mut Node,
    keys: *mut *mut c_char,
}

type Handle = *mut c_void;
type Callback = unsafe extern "C" fn(*mut c_void);

struct Api {
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    destroy: unsafe extern "C" fn(Handle),
    option: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    set: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    get: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    command: unsafe extern "C" fn(Handle, *const *const c_char) -> c_int,
    wait: unsafe extern "C" fn(Handle, f64) -> *mut Event,
    observe: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    hook_add: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    hook_continue: unsafe extern "C" fn(Handle, u64) -> c_int,
    logs: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
    free_node: unsafe extern "C" fn(*mut Node),
    error: unsafe extern "C" fn(c_int) -> *const c_char,
    render_create: unsafe extern "C" fn(*mut Handle, Handle, *mut RenderParam) -> c_int,
    render_callback: unsafe extern "C" fn(Handle, Option<Callback>, *mut c_void),
    render_update: unsafe extern "C" fn(Handle) -> u64,
    render: unsafe extern "C" fn(Handle, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(Handle),
    // The library must outlive every copied function pointer and both mpv contexts.
    _library: Library,
}

impl Api {
    fn load() -> Result<Self, String> {
        let library = crate::runtime::load_library()?;
        // Only the app-owned, bundled library supplies these documented C exports.
        unsafe { Self::from_library(library) }
    }

    unsafe fn from_library(library: Library) -> Result<Self, String> {
        macro_rules! symbol {
            ($name:literal) => {
                unsafe {
                    *library
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|e| e.to_string())?
                }
            };
        }
        Ok(Self {
            create: symbol!("mpv_create"),
            initialize: symbol!("mpv_initialize"),
            destroy: symbol!("mpv_terminate_destroy"),
            option: symbol!("mpv_set_option_string"),
            set: symbol!("mpv_set_property_string"),
            get: symbol!("mpv_get_property"),
            command: symbol!("mpv_command"),
            wait: symbol!("mpv_wait_event"),
            observe: symbol!("mpv_observe_property"),
            logs: symbol!("mpv_request_log_messages"),
            hook_add: symbol!("mpv_hook_add"),
            hook_continue: symbol!("mpv_hook_continue"),
            free_node: symbol!("mpv_free_node_contents"),
            error: symbol!("mpv_error_string"),
            render_create: symbol!("mpv_render_context_create"),
            render_callback: symbol!("mpv_render_context_set_update_callback"),
            render_update: symbol!("mpv_render_context_update"),
            render: symbol!("mpv_render_context_render"),
            render_free: symbol!("mpv_render_context_free"),
            _library: library,
        })
    }

    fn check(&self, result: c_int, action: &str) -> Result<(), String> {
        if result >= 0 {
            Ok(())
        } else {
            Err(format!("{action}: {}", unsafe {
                text((self.error)(result))
            }))
        }
    }
}

unsafe fn text(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    }
}

struct Player {
    api: Arc<Api>,
    handle: Handle,
    mailbox: Arc<Mailbox>,
    render_stop: Arc<AtomicBool>,
    renderer: Option<JoinHandle<()>>,
    state: Snapshot,
    media_ready: bool,
    frame_ready: bool,
    preview: bool,
    waiting_for_seek: bool,
    intended_paused: bool,
    fatal_error: Option<String>,
    next_load: Option<LoadState>,
    saved_gains: std::collections::BTreeMap<i64, f64>,
    resume_position: Option<f64>,
    pending_steps: std::collections::VecDeque<bool>,
}

impl Player {
    fn new_output(mailbox: Arc<Mailbox>, hwnd: Option<usize>) -> Result<Self, String> {
        if let Some(hwnd) = hwnd {
            let wid = hwnd.to_string();
            Self::new_configured(
                mailbox,
                &[
                    ("vo", "gpu"),
                    ("gpu-context", "d3d11"),
                    ("hwdec", "auto"),
                    ("audio-device", "openal"),
                    ("wid", &wid),
                    ("input-vo-keyboard", "no"),
                    ("input-cursor", "no"),
                    ("input-builtin-bindings", "no"),
                ],
                false,
            )
        } else {
            Self::new_configured(mailbox, &[("audio-device", "openal")], false)
        }
    }

    #[cfg(test)]
    fn new_with_options(mailbox: Arc<Mailbox>, options: &[(&str, &str)]) -> Result<Self, String> {
        Self::new_configured(mailbox, options, false)
    }

    fn new_configured(
        mailbox: Arc<Mailbox>,
        options: &[(&str, &str)],
        preview: bool,
    ) -> Result<Self, String> {
        let api = Arc::new(Api::load()?);
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("mpv_create failed".into());
        }
        let mut player = Self {
            api,
            handle,
            mailbox,
            render_stop: Arc::new(AtomicBool::new(false)),
            renderer: None,
            state: Snapshot::default(),
            media_ready: false,
            frame_ready: false,
            preview,
            waiting_for_seek: false,
            intended_paused: true,
            fatal_error: None,
            next_load: None,
            saved_gains: Default::default(),
            resume_position: None,
            pending_steps: Default::default(),
        };
        for (name, value) in [
            ("vo", "libmpv"),
            ("hwdec", "no"),
            ("terminal", "no"),
            ("config", "no"),
            ("load-scripts", "no"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("pause", "yes"),
            ("mute", "yes"),
            ("volume", "100"),
            ("volume-max", "100"),
            // Headsets can advertise virtual 7.1 even when the listener expects
            // stereo. Downmix in mpv instead of relying on that device routing.
            ("audio-channels", "stereo"),
            ("audio-normalize-downmix", "yes"),
            ("ad-lavc-downmix", "no"),
            ("input-default-bindings", "no"),
            ("osd-level", "0"),
        ] {
            let name = CString::new(name).unwrap();
            let value = CString::new(value).unwrap();
            player.api.check(
                unsafe { (player.api.option)(handle, name.as_ptr(), value.as_ptr()) },
                "Configure mpv",
            )?;
        }
        for &(name, value) in options {
            let name = CString::new(name).map_err(|e| e.to_string())?;
            let value = CString::new(value).map_err(|e| e.to_string())?;
            player.api.check(
                unsafe { (player.api.option)(handle, name.as_ptr(), value.as_ptr()) },
                "Configure mpv",
            )?;
        }
        player
            .api
            .check(unsafe { (player.api.initialize)(handle) }, "Initialize mpv")?;
        player.api.check(
            unsafe {
                (player.api.logs)(
                    handle,
                    if crate::logging::enabled() {
                        c"info".as_ptr()
                    } else {
                        c"warn".as_ptr()
                    },
                )
            },
            "Subscribe to mpv diagnostics",
        )?;
        if !preview {
            player.api.check(
                unsafe { (player.api.observe)(handle, 0, c"track-list".as_ptr(), 0) },
                "Observe audio tracks",
            )?;
            player.api.check(
                unsafe { (player.api.hook_add)(handle, 0, c"on_preloaded".as_ptr(), 0) },
                "Register audio mix hook",
            )?;
        }
        let native = options.iter().any(|(key, _)| *key == "wid");
        Print(
            Level::Info,
            if native {
                "Initialized native GPU video output, D3D11, hwdec=auto with software fallback"
            } else {
                "Initialized software preview/test renderer"
            },
        );
        if native {
            return Ok(player);
        }
        let api = player.api.clone();
        let shared = player.mailbox.clone();
        let stop = player.render_stop.clone();
        // Pass the address only to create the renderer. Control owns destruction and
        // always joins rendering before destroying this handle. No synchronous client
        // calls are made on the render thread after context creation.
        let address = handle as usize;
        let (tx, rx) = mpsc::sync_channel(1);
        player.renderer = Some(
            thread::Builder::new()
                .name("mpv-software-render".into())
                .spawn(move || render_worker(api, address, shared, stop, tx))
                .map_err(|e| e.to_string())?,
        );
        rx.recv()
            .map_err(|_| "Render worker exited during initialization".to_owned())??;
        Ok(player)
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let key = CString::new(name).map_err(|e| e.to_string())?;
        let value = CString::new(value).map_err(|e| e.to_string())?;
        self.api.check(
            unsafe { (self.api.set)(self.handle, key.as_ptr(), value.as_ptr()) },
            name,
        )
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let strings: Vec<_> = args
            .iter()
            .map(|s| CString::new(*s))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let mut pointers: Vec<_> = strings.iter().map(|s| s.as_ptr()).collect();
        pointers.push(ptr::null());
        self.api.check(
            unsafe { (self.api.command)(self.handle, pointers.as_ptr()) },
            args[0],
        )
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), String> {
        if !paused {
            self.pending_steps.clear();
        }
        Print(
            Level::Debug,
            format!(
                "Audio {}: intended_pause={paused}, user_mute={}",
                if paused { "suppression" } else { "restoration" },
                self.state.muted
            ),
        );
        // Suppress audio before any native resume, including frame stepping.
        // Snapshot::muted remains the user's choice, not this effective mute.
        self.set(
            "mute",
            if paused || self.state.muted {
                "yes"
            } else {
                "no"
            },
        )?;
        self.set("pause", if paused { "yes" } else { "no" })?;
        self.intended_paused = paused;
        self.state.paused = paused;
        Ok(())
    }

    fn seek_command(&mut self, args: &[&str]) -> Result<(), String> {
        if !self.poll_events() {
            return Err("Playback decoder shut down".into());
        }
        // Commands return before SEEK arrives. Invalidate now, and require that
        // event before a restart can make the previous frame look ready again.
        self.frame_ready = false;
        self.waiting_for_seek = true;
        self.mailbox.invalidate_frames();
        let result = self.command(args);
        if result.is_err() {
            self.waiting_for_seek = false;
        }
        result
    }

    fn step_frame(&mut self, forward: bool) -> Result<(), String> {
        self.set("mute", "yes")?;
        self.set("pause", "yes")?;
        self.frame_ready = false;
        self.waiting_for_seek = true;
        self.mailbox.invalidate_frames();
        // Seek-based stepping never resumes the audio device, which may still
        // contain samples queued before pause that mute cannot silence.
        let result = if forward {
            self.command(&["frame-step", "1", "seek"])
        } else {
            self.command(&["frame-back-step"])
        };
        if result.is_err() {
            self.waiting_for_seek = false;
        }
        result
    }

    fn number(&self, name: &str) -> Option<f64> {
        let key = CString::new(name).ok()?;
        let mut value = 0.0_f64;
        let result = unsafe {
            (self.api.get)(
                self.handle,
                key.as_ptr(),
                5,
                (&mut value as *mut f64).cast(),
            )
        };
        (result >= 0 && value.is_finite()).then_some(value)
    }

    fn flag(&self, name: &str) -> Option<bool> {
        let key = CString::new(name).ok()?;
        let mut value: c_int = 0;
        let result = unsafe {
            (self.api.get)(
                self.handle,
                key.as_ptr(),
                3,
                (&mut value as *mut c_int).cast(),
            )
        };
        (result >= 0).then_some(value != 0)
    }

    fn publish(&self) {
        *self
            .mailbox
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.state.clone();
    }

    fn refresh(&mut self) {
        if self.media_ready && self.frame_ready && !self.intended_paused {
            self.state.playback_started = true;
        }
        self.state.duration = self.number("duration").unwrap_or(0.0).max(0.0);
        self.state.position = self.number("time-pos").unwrap_or(0.0).max(0.0);
        self.state.ended = self.flag("eof-reached") == Some(true);
        // A native frame-step temporarily unpauses. Only explicit play commands
        // can clear the user's pause intent. Native EOF pauses also latch it.
        self.intended_paused |= self.flag("pause") == Some(true);
        self.state.paused = self.intended_paused;
        if self.intended_paused && self.flag("mute") != Some(true) {
            if let Err(error) = self.set("mute", "yes") {
                self.state.error = Some(error);
            }
        }
        self.state.volume = self.number("volume").unwrap_or(self.state.volume);

        self.state.speed = self.number("speed").unwrap_or(self.state.speed);
        self.state.effective_muted = self.flag("mute").unwrap_or(true);
        self.state.video_output = self
            .property_node("current-vo")
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned));
        self.state.hardware_decoder = self
            .property_node("hwdec-current")
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned));
        self.state.dropped_frames = self.number("frame-drop-count");
        self.state.decoder_dropped_frames = self.number("decoder-frame-drop-count");
        let width = if self.media_ready && self.frame_ready {
            self.number("dwidth").unwrap_or(0.0) as u32
        } else {
            0
        };
        let height = if self.media_ready && self.frame_ready {
            self.number("dheight").unwrap_or(0.0) as u32
        } else {
            0
        };
        let (width, height) = if self.preview {
            preview_size(width, height)
        } else {
            (width, height)
        };
        self.mailbox.size.store(
            (u64::from(width) << 32) | u64::from(height),
            Ordering::Release,
        );
        self.publish();
    }

    fn property_node(&self, name: &str) -> Result<serde_json::Value, String> {
        let name = CString::new(name).map_err(|e| e.to_string())?;
        let mut node = Node {
            data: NodeData { integer: 0 },
            format: 0,
        };
        let result = unsafe {
            (self.api.get)(
                self.handle,
                name.as_ptr(),
                6,
                (&mut node as *mut Node).cast(),
            )
        };
        self.api.check(result, "Read mpv property")?;
        let value = unsafe { node_json(&node) };
        unsafe { (self.api.free_node)(&mut node) };
        Ok(value)
    }

    fn refresh_metadata(&mut self) {
        if self.preview {
            return;
        }
        for property in [
            "file-format",
            "video-format",
            "video-params",
            "container-fps",
            "video-bitrate",
            "track-list",
            "chapter-list",
            "metadata",
        ] {
            if let Ok(value) = self.property_node(property) {
                if !value.is_null() {
                    self.state.metadata.insert(property.into(), value);
                }
            }
        }
    }

    fn tracks(&mut self) -> Result<(), String> {
        if self.preview {
            return Ok(());
        }
        let value = self.property_node("track-list")?;
        let mut tracks = audio_tracks(&value, &self.state.tracks);
        for track in &mut tracks {
            if !self
                .state
                .tracks
                .iter()
                .any(|previous| previous.id == track.id)
            {
                if let Some(gain) = self.saved_gains.get(&track.id) {
                    track.gain = *gain;
                }
            }
        }
        let changed = tracks
            .iter()
            .map(|t| t.id)
            .ne(self.state.tracks.iter().map(|t| t.id));
        if changed {
            // Commit gains only after mpv accepts the new graph.
            self.set("af", &gain_filter(&tracks)?)?;
            self.set("lavfi-complex", &mix_graph(&tracks)?)?;
        }
        self.state.tracks = tracks;
        Ok(())
    }

    fn execute(&mut self, command: Command) -> Result<(), String> {
        Print(Level::Debug, format!("Playback command {command:?}"));
        match command {
            Command::LoadSaved(path, state) => {
                finite_range(state.volume, 0.0, 100.0, "Master volume")?;
                finite_range(state.position, 0.0, f64::MAX, "Resume position")?;
                crate::clip_store::validate_gains(&state.gains)?;
                self.next_load = Some(state);
                self.execute(Command::Load(path))
            }
            Command::Load(path) => {
                self.pending_steps.clear();
                let saved = self.next_load.take();
                let requested_path = path.clone();
                let path = std::fs::canonicalize(&path)
                    .map_err(|e| format!("Open {}: {e}", path.display()))?;
                let path = path.to_str().ok_or("Media path is not valid UTF-8")?;
                self.media_ready = false;
                self.frame_ready = false;
                self.mailbox.invalidate_frames();
                // Unload before clearing the old graph, so its track IDs cannot
                // leak into initialization of the replacement file.
                self.command(&["stop"])?;
                self.poll_events();
                self.media_ready = false;
                self.frame_ready = false;
                self.set("lavfi-complex", "")?;
                self.set("af", "")?;
                self.state.tracks.clear();
                self.state.metadata.clear();
                self.saved_gains = saved
                    .as_ref()
                    .map(|state| state.gains.clone())
                    .unwrap_or_default();
                self.resume_position = saved
                    .as_ref()
                    .map(|state| state.position)
                    .filter(|p| *p > 0.0);
                if let Some(state) = &saved {
                    self.execute(Command::SetVolume(state.volume))?;
                    self.execute(Command::SetMute(state.muted))?;
                }
                self.set_paused(saved.as_ref().is_some_and(|state| state.paused))?;
                self.state.error = None;
                self.fatal_error = None;
                self.state.clear_diagnostics();
                self.mailbox
                    .render_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                self.state.position = 0.0;
                self.state.duration = 0.0;
                self.state.path = Some(requested_path);
                self.state.playback_started = false;
                self.command(&["loadfile", path, "replace"])?;
                Ok(())
            }
            Command::Toggle => self.set_paused(!self.intended_paused),
            Command::Play => self.set_paused(false),
            Command::Pause => self.set_paused(true),
            Command::Stop => {
                self.pending_steps.clear();
                self.set_paused(true)?;
                self.frame_ready = false;
                self.mailbox.invalidate_frames();
                if self.media_ready {
                    self.seek_command(&["seek", "0", "absolute+exact"])?;
                }
                self.state.position = 0.0;
                self.state.paused = true;
                Ok(())
            }
            Command::SeekAbsolute(value) => {
                self.pending_steps.clear();
                finite_range(value, 0.0, f64::MAX, "Seek position")?;
                self.seek_command(&["seek", &value.to_string(), "absolute+exact"])
            }
            Command::SeekRelative(value) => {
                self.pending_steps.clear();
                if !value.is_finite() {
                    return Err("Seek offset must be finite".into());
                }
                self.seek_command(&["seek", &value.to_string(), "relative+exact"])
            }
            Command::FrameStep | Command::FrameBackStep => {
                if !self.intended_paused {
                    return Err("Frame stepping is only allowed while paused".into());
                }
                let forward = matches!(command, Command::FrameStep);
                // Relative frame seeks issued before the previous seek finishes
                // can collapse onto the same frame. Keep their original order.
                if self.waiting_for_seek || !self.frame_ready {
                    if self.pending_steps.len() >= 128 {
                        return Err("Frame step queue is full".into());
                    }
                    self.pending_steps.push_back(forward);
                    Ok(())
                } else {
                    self.step_frame(forward)
                }
            }
            Command::SetVolume(value) => {
                finite_range(value, 0.0, 100.0, "Volume")?;
                self.set("volume", &value.to_string())
            }
            Command::SetMute(value) => {
                self.set(
                    "mute",
                    if value || self.intended_paused {
                        "yes"
                    } else {
                        "no"
                    },
                )?;
                self.state.muted = value;
                Ok(())
            }
            Command::SetSpeed(value) => {
                finite_range(value, 0.01, 100.0, "Speed")?;
                self.set("speed", &value.to_string())
            }
            #[cfg(test)]
            Command::SetAudioOutput(mode) => {
                if mode == AudioOutputMode::HeadsetCompatible {
                    let devices = self.property_node("audio-device-list")?;
                    if !devices
                        .as_array()
                        .is_some_and(|devices| devices.iter().any(|d| d["name"] == "openal"))
                    {
                        return Err(
                            "Headset compatibility output is not available in this decoder build"
                                .into(),
                        );
                    }
                }
                let previous = self.state.audio_output;
                self.set("audio-channels", mode.channels())?;
                if let Err(error) = self.set("audio-device", mode.device()) {
                    let _ = self.set("audio-channels", previous.channels());
                    return Err(error);
                }
                self.state.audio_output = mode;
                Print(
                    Level::Info,
                    format!(
                        "Audio routing {mode:?}: channels={}, source={:?}, output={:?}",
                        mode.channels(),
                        self.property_node("audio-params").ok(),
                        self.property_node("audio-out-params").ok()
                    ),
                );
                Ok(())
            }
            Command::SetTrackGain { id, gain } => {
                finite_range(gain, 0.0, 10.0, "Track gain")?;
                let index = self
                    .state
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or_else(|| format!("Unknown audio track {id}"))?;
                if self.state.tracks[index].gain == gain {
                    return Ok(());
                }
                self.command(&[
                    "af-command",
                    "track-gains",
                    "volume",
                    &gain.to_string(),
                    &format!("volume@track{id}"),
                ])?;
                self.state.tracks[index].gain = gain;
                Ok(())
            }
            Command::Shutdown => Ok(()),
        }
    }

    fn run(&mut self, receiver: mpsc::Receiver<Command>) {
        let mut refresh_at = Instant::now();
        let mut stats_at = Instant::now();
        loop {
            match receiver.recv_timeout(Duration::from_millis(8)) {
                Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(command) => {
                    if let Err(error) = self.execute(command) {
                        Print(Level::Error, &error);
                        self.fatal_error = None;
                        self.state.error =
                            Some(with_diagnostic(error, self.state.diagnostic.as_deref()));
                    }
                    self.refresh();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !self.poll_events() {
                return;
            }
            if stats_at.elapsed() >= Duration::from_secs(5) {
                Print(
                    Level::Info,
                    format!(
                        "Playback pos={:.3} paused={} hwdec={:?} vo={:?} dropped={:?} decoder_dropped={:?}",
                        self.state.position,
                        self.intended_paused,
                        self.property_node("hwdec-current").ok(),
                        self.property_node("current-vo").ok(),
                        self.number("frame-drop-count"),
                        self.number("decoder-frame-drop-count")
                    ),
                );
                Print(
                    Level::Info,
                    format!(
                        "Audio device={:?} backend={:?} channels={:?} source={:?} output={:?}",
                        self.property_node("audio-device").ok(),
                        self.property_node("current-ao").ok(),
                        self.property_node("audio-channels").ok(),
                        self.property_node("audio-params").ok(),
                        self.property_node("audio-out-params").ok()
                    ),
                );
                stats_at = Instant::now();
            }
            if refresh_at.elapsed() >= Duration::from_millis(40) {
                self.refresh();
                refresh_at = Instant::now();
            } else {
                self.publish();
            }
        }
    }

    fn poll_events(&mut self) -> bool {
        loop {
            // Event data is consumed before the next mpv_wait_event call.
            let event = unsafe { &*(self.api.wait)(self.handle, 0.0) };
            match event.id {
                0 => break,
                1 => return false,
                2 if !event.data.is_null() => {
                    let log = unsafe { &*event.data.cast::<LogMessage>() };
                    Print(
                        match unsafe { text(log.level) }.as_str() {
                            "fatal" | "error" => Level::Error,
                            "warn" => Level::Warn,
                            _ => Level::Info,
                        },
                        format!(
                            "mpv/{}: {}",
                            unsafe { text(log.prefix) },
                            unsafe { text(log.text) }.trim()
                        ),
                    );
                    self.state.record_diagnostic(
                        &unsafe { text(log.level) },
                        &unsafe { text(log.prefix) },
                        &unsafe { text(log.text) },
                    );
                    // Logs can arrive after END_FILE. Rebuild context from the
                    // original failure rather than repeatedly appending messages.
                    if let Some(error) = self.fatal_error.as_ref() {
                        self.state.error = Some(with_diagnostic(
                            error.clone(),
                            self.state.diagnostic.as_deref(),
                        ));
                    }
                }
                6 => {
                    self.waiting_for_seek = false;
                    self.media_ready = false;
                    self.frame_ready = false;
                    self.state.tracks.clear();
                    self.state.metadata.clear();
                    self.state.duration = 0.0;
                    self.state.position = 0.0;
                    self.mailbox.invalidate_frames();
                }
                7 if !event.data.is_null() => {
                    let end = unsafe { &*event.data.cast::<EndFile>() };
                    if let Some(error) = end_file_error(
                        end.reason,
                        self.api.check(end.error, "Load/play media").err(),
                        None,
                    ) {
                        self.fatal_error = Some(error.clone());
                        self.state.error =
                            Some(with_diagnostic(error, self.state.diagnostic.as_deref()));
                    }
                    self.media_ready = false;
                    self.frame_ready = false;
                    self.mailbox.invalidate_frames();
                }
                8 => {
                    self.media_ready = true;
                    if let Err(error) = self.tracks() {
                        self.state.error = Some(error);
                    }
                    if let Some(position) = self.resume_position.take() {
                        if let Err(error) =
                            self.seek_command(&["seek", &position.to_string(), "absolute+exact"])
                        {
                            self.state.error = Some(error);
                        }
                    }
                    self.refresh_metadata();
                }
                20 => {
                    self.waiting_for_seek = false;
                    self.frame_ready = false;
                    self.mailbox.invalidate_frames();
                }
                21 => {
                    // Publish only after mpv completes startup/seek, not a redraw
                    // of the previous file or the pre-seek frame.
                    self.frame_ready = self.media_ready && !self.waiting_for_seek;
                    if self.media_ready {
                        self.refresh_metadata();
                    }
                }
                22 if self.media_ready => {
                    if let Err(error) = self.tracks() {
                        self.state.error = Some(error);
                    }
                    self.refresh_metadata();
                }
                25 if !event.data.is_null() => {
                    let hook = unsafe { &*event.data.cast::<Hook>() };
                    let id = hook.id;
                    // This hook runs after demuxing, before selection/decoders.
                    // Always continue it, including graph failures, to avoid
                    // blocking load and shutdown indefinitely.
                    if let Err(error) = self.tracks() {
                        self.state.error = Some(error);
                    }
                    if let Err(error) = self.api.check(
                        unsafe { (self.api.hook_continue)(self.handle, id) },
                        "Continue audio mix hook",
                    ) {
                        self.state.error = Some(error);
                    }
                }
                24 => {
                    self.state.error = Some("mpv event queue overflow".into());
                }
                _ => {}
            }
        }
        if self.intended_paused
            && self.frame_ready
            && !self.waiting_for_seek
            && let Some(forward) = self.pending_steps.pop_front()
            && let Err(error) = self.step_frame(forward)
        {
            self.state.error = Some(error);
        }
        true
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.render_stop.store(true, Ordering::Release);
        if let Some(renderer) = self.renderer.take() {
            let _ = renderer.join();
        }
        unsafe { (self.api.destroy)(self.handle) };
    }
}

/// Owns a separate silent mpv core and its software renderer on the preview worker.
/// It deliberately has no audio-mix hook and never changes the playback instance.
pub(crate) struct PreviewDecoder {
    player: Player,
    path: Option<PathBuf>,
}

fn render_size(width: u32, height: u32, viewport: u64) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let (vw, vh) = if viewport == 0 {
        (3840, 2160)
    } else {
        (
            ((viewport >> 32) as u32).min(3840),
            (viewport as u32).min(2160),
        )
    };
    let scale = (f64::from(vw) / f64::from(width))
        .min(f64::from(vh) / f64::from(height))
        .min(1.0);
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
}

fn preview_size(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let scale = (240.0 / f64::from(width))
        .min(135.0 / f64::from(height))
        .min(1.0);
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
}

impl PreviewDecoder {
    pub(crate) fn duration(&self) -> Option<f64> {
        self.path.as_ref()?;
        self.player
            .number("duration")
            .filter(|seconds| *seconds > 0.0)
    }

    pub(crate) fn new() -> Result<Self, String> {
        let player = Player::new_configured(
            Arc::new(Mailbox::new()),
            &[
                ("aid", "no"),
                ("sid", "no"),
                ("ao", "null"),
                ("audio", "no"),
                ("sub", "no"),
                ("video-timing-offset", "0"),
                ("network-timeout", "5"),
                ("hr-seek", "yes"),
                ("ytdl", "no"),
                ("access-references", "no"),
                ("autoload-files", "no"),
            ],
            true,
        )?;
        Ok(Self { player, path: None })
    }

    /// The caller must discard this decoder after cancellation or failure. That
    /// prevents events from an unfinished request satisfying the next request.
    pub(crate) fn decode(
        &mut self,
        path: &std::path::Path,
        seconds: f64,
        deadline: Instant,
        canceled: impl Fn() -> bool,
    ) -> Result<image::RgbaImage, String> {
        finite_range(seconds, 0.0, f64::MAX, "Preview position")?;
        let check = || {
            if canceled() {
                Err("Preview canceled".to_owned())
            } else if Instant::now() >= deadline {
                Err("Preview decoding timed out after 5 seconds".to_owned())
            } else {
                Ok(())
            }
        };
        check()?;
        let path = std::fs::canonicalize(path).map_err(|e| format!("Open preview: {e}"))?;
        let player = &mut self.player;
        if !player.poll_events() {
            return Err("Preview decoder shut down".into());
        }
        player.state.error = None;
        player.fatal_error = None;
        player.state.clear_diagnostics();
        player.mailbox.render_error.lock().unwrap().take();
        player.set("pause", "yes")?;
        player.frame_ready = false;
        player.mailbox.invalidate_frames();
        if self.path.as_ref() == Some(&path) && player.media_ready {
            // Require a fresh SEEK before accepting PLAYBACK_RESTART. A queued
            // redraw or restart of the old frame cannot satisfy this request.
            player.seek_command(&["seek", &seconds.to_string(), "absolute+exact"])?;
        } else {
            self.path = None;
            player.command(&["stop"])?;
            player.poll_events();
            player.media_ready = false;
            player.frame_ready = false;
            player.waiting_for_seek = false;
            player.mailbox.invalidate_frames();
            let name = path.to_str().ok_or("Media path is not valid UTF-8")?;
            player.command(&[
                "loadfile",
                name,
                "replace",
                "-1",
                &format!("start={seconds}"),
            ])?;
        }
        loop {
            check()?;
            if !player.poll_events() {
                return Err("Preview decoder shut down".into());
            }
            player.refresh();
            if let Some(error) = player.state.error.as_ref() {
                return Err(error.clone());
            }
            if let Some(error) = player.mailbox.render_error.lock().unwrap().as_ref() {
                return Err(with_diagnostic(
                    error.clone(),
                    player.state.diagnostic.as_deref(),
                ));
            }
            if player.media_ready && player.frame_ready {
                if let Some(mut frame) = player.mailbox.frame.lock().unwrap().take() {
                    check()?;
                    self.path = Some(path);
                    for pixel in frame.pixels.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                    return image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels)
                        .ok_or_else(|| "Invalid preview image dimensions".to_owned());
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

unsafe extern "C" fn render_updated(context: *mut c_void) {
    // The boxed flag remains alive until render_free returns; this callback never
    // calls mpv, takes a lock, allocates, or unwinds.
    unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Release);
}

fn render_worker(
    api: Arc<Api>,
    address: usize,
    mailbox: Arc<Mailbox>,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let mut context = ptr::null_mut();
    let mut params = [
        RenderParam::new(RENDER_API_TYPE, c"sw".as_ptr().cast_mut().cast()),
        RenderParam::end(),
    ];
    let result =
        unsafe { (api.render_create)(&mut context, address as Handle, params.as_mut_ptr()) };
    if let Err(error) = api.check(result, "Create software renderer") {
        let _ = ready.send(Err(error));
        return;
    }
    let updated = Box::new(AtomicBool::new(true));
    unsafe {
        (api.render_callback)(
            context,
            Some(render_updated),
            (&*updated as *const AtomicBool).cast_mut().cast(),
        )
    };
    if ready.send(Ok(())).is_ok() {
        let mut last_size = 0;
        let mut last_generation = 0;
        let mut last_viewport = u64::MAX;
        let mut storage = Vec::<u32>::new();
        while !stop.load(Ordering::Acquire) {
            let generation = mailbox.generation.load(Ordering::Acquire);
            let packed = mailbox.size.load(Ordering::Acquire);
            let viewport = mailbox.viewport.load(Ordering::Acquire);
            let new_frame = updated.swap(false, Ordering::AcqRel)
                && unsafe { (api.render_update)(context) } & 1 != 0;
            // A paused first frame may arrive before control publishes dimensions.
            // Redraw it once the target size becomes available, without a new callback.
            if new_frame
                || (packed != 0
                    && (packed != last_size
                        || generation != last_generation
                        || viewport != last_viewport))
            {
                if viewport != last_viewport {
                    mailbox.frame.lock().unwrap().take();
                }
                last_size = packed;
                last_generation = generation;
                last_viewport = viewport;
                let (width, height) = render_size((packed >> 32) as u32, packed as u32, viewport);
                // Limit allocation to 256 MiB. Render a tiny surface before dimensions
                // become available so the VO can finish startup without waiting on us.
                let valid = width > 0
                    && height > 0
                    && width <= 16384
                    && height <= 16384
                    && u64::from(width) * u64::from(height) <= 64 * 1024 * 1024;
                let (w, h) = if valid { (width, height) } else { (1, 1) };
                let stride = ((w as usize * 4) + 63) & !63;
                // u32 storage guarantees pixel alignment. Align the starting address
                // to 64 bytes and reserve writable padding after the last scanline.
                storage.resize(stride * h as usize / 4 + 16, 0);
                let base = storage.as_mut_ptr() as usize;
                let offset = (64 - (base & 63)) & 63;
                let pixels = unsafe { storage.as_mut_ptr().cast::<u8>().add(offset) };
                let mut size = [w as c_int, h as c_int];
                let mut stride = stride;
                let mut params = [
                    RenderParam::new(RENDER_SW_SIZE, size.as_mut_ptr().cast()),
                    RenderParam::new(RENDER_SW_FORMAT, c"bgr0".as_ptr().cast_mut().cast()),
                    RenderParam::new(RENDER_SW_STRIDE, (&mut stride as *mut usize).cast()),
                    RenderParam::new(RENDER_SW_POINTER, pixels.cast()),
                    RenderParam::end(),
                ];
                let result = unsafe { (api.render)(context, params.as_mut_ptr()) };
                if let Err(error) = api.check(result, "Render frame") {
                    *mailbox
                        .render_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(error);
                } else if valid
                    && mailbox.size.load(Ordering::Acquire) == packed
                    && mailbox.viewport.load(Ordering::Acquire) == viewport
                {
                    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
                    for y in 0..h as usize {
                        let row = unsafe {
                            std::slice::from_raw_parts(pixels.add(y * stride), w as usize * 4)
                        };
                        bgra.extend_from_slice(row);
                    }
                    for pixel in bgra.chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                    mailbox.publish_frame(
                        generation,
                        packed,
                        VideoFrame {
                            width: w,
                            height: h,
                            pixels: bgra,
                        },
                    );
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    unsafe {
        (api.render_callback)(context, None, ptr::null_mut());
        (api.render_free)(context);
    }
}

unsafe fn node_json(node: &Node) -> serde_json::Value {
    use serde_json::Value;
    match node.format {
        1 => Value::String(unsafe { text(node.data.string) }),
        3 => Value::Bool(unsafe { node.data.flag } != 0),
        4 => Value::from(unsafe { node.data.integer }),
        5 => Value::from(unsafe { node.data.number }),
        7 | 8 => {
            let list = unsafe { node.data.list };
            if list.is_null() {
                return Value::Null;
            }
            let list = unsafe { &*list };
            if list.count <= 0 || list.values.is_null() {
                return if node.format == 7 {
                    Value::Array(Vec::new())
                } else {
                    Value::Object(Default::default())
                };
            }
            let values = unsafe { std::slice::from_raw_parts(list.values, list.count as usize) };
            if node.format == 7 {
                Value::Array(values.iter().map(|n| unsafe { node_json(n) }).collect())
            } else {
                if list.keys.is_null() {
                    return Value::Null;
                }
                let keys = unsafe { std::slice::from_raw_parts(list.keys, list.count as usize) };
                Value::Object(
                    keys.iter()
                        .zip(values)
                        .map(|(key, value)| (unsafe { text(*key) }, unsafe { node_json(value) }))
                        .collect(),
                )
            }
        }
        _ => Value::Null,
    }
}

fn finite_range(value: f64, min: f64, max: f64, name: &str) -> Result<(), String> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be finite and between {min} and {max}"))
    }
}

fn audio_tracks(value: &serde_json::Value, previous: &[AudioTrack]) -> Vec<AudioTrack> {
    let mut tracks = Vec::new();
    if let Some(entries) = value.as_array() {
        for entry in entries {
            if entry["type"].as_str() != Some("audio") {
                continue;
            }
            let Some(id) = entry["id"].as_i64().filter(|id| *id > 0) else {
                continue;
            };
            let gain = previous
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.gain)
                .unwrap_or(1.0);
            let title = entry["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| entry["lang"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Audio {id}"));
            tracks.push(AudioTrack { id, title, gain });
        }
    }
    tracks
}

fn mix_graph(tracks: &[AudioTrack]) -> Result<String, String> {
    use std::fmt::Write;
    let mut graph = String::new();
    let mut ids = std::collections::HashSet::new();
    for (index, track) in tracks.iter().enumerate() {
        if track.id <= 0 || !ids.insert(track.id) {
            return Err("Audio track IDs must be positive and unique".into());
        }
        finite_range(track.gain, 0.0, 10.0, "Track gain")?;
        if tracks.len() == 1 {
            let _ = write!(graph, "[aid{}]anull[ao]", track.id);
        } else {
            let _ = write!(
                graph,
                "[aid{}]aformat=channel_layouts=stereo,pan={}c|c{}=c0|c{}=c1[mix{index}];",
                track.id,
                tracks.len() * 2,
                index * 2,
                index * 2 + 1
            );
        }
    }
    if tracks.len() > 1 {
        for index in 0..tracks.len() {
            let _ = write!(graph, "[mix{index}]");
        }
        let _ = write!(
            graph,
            "amix=inputs={}:duration=longest:dropout_transition=0:normalize=0[ao]",
            tracks.len()
        );
    }
    Ok(graph)
}

// Keep each stereo input separate until the commandable output filter applies
// its gain. Changing lavfi-complex would restart playback, even for gain alone.
fn gain_filter(tracks: &[AudioTrack]) -> Result<String, String> {
    use std::fmt::Write;
    mix_graph(tracks)?;
    if tracks.is_empty() {
        return Ok(String::new());
    }
    let mut graph = String::new();
    if tracks.len() == 1 {
        let track = &tracks[0];
        let _ = write!(graph, "volume@track{}={}", track.id, track.gain);
    } else {
        let _ = write!(graph, "asplit={}", tracks.len());
        for index in 0..tracks.len() {
            let _ = write!(graph, "[input{index}]");
        }
        graph.push(';');
        for (index, track) in tracks.iter().enumerate() {
            let _ = write!(
                graph,
                "[input{index}]pan=stereo|c0=c{}|c1=c{},volume@track{}={}[mix{index}];",
                index * 2,
                index * 2 + 1,
                track.id,
                track.gain
            );
        }
        for index in 0..tracks.len() {
            let _ = write!(graph, "[mix{index}]");
        }
        let _ = write!(
            graph,
            "amix=inputs={}:duration=longest:dropout_transition=0:normalize=0",
            tracks.len()
        );
    }
    Ok(format!("@track-gains:lavfi=[{graph}]"))
}

/// Small indexed AVI fixtures, generated without tools or checked-in binaries.
#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
pub(crate) mod test_fixtures {
    fn chunk(id: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::from(*id);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        if bytes.len() % 2 != 0 {
            out.push(0);
        }
        out
    }

    fn list(kind: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        let mut data = Vec::from(*kind);
        data.extend_from_slice(bytes);
        chunk(b"LIST", &data)
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn stream_header(audio: bool, width: u32, height: u32, fps: u32, frames: u32) -> Vec<u8> {
        let mut out = Vec::from(if audio { *b"auds" } else { *b"vids" });
        out.extend_from_slice(if audio { &[0; 4] } else { b"DIB " });
        // Flags, priority/language, initial frames, scale, rate, start, length,
        // suggested buffer size, quality and sample size, followed by rcFrame.
        out.extend(words(&[
            0,
            0,
            0,
            1,
            if audio { 48000 } else { fps },
            0,
            if audio { 48000 / fps * frames } else { frames },
            if audio {
                96000 / fps
            } else {
                width * height * 3
            },
            u32::MAX,
            if audio { 2 } else { 0 },
        ]));
        for value in [
            0_u16,
            0,
            if audio { 0 } else { width as u16 },
            if audio { 0 } else { height as u16 },
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(out.len(), 56);
        out
    }

    pub(crate) fn avi(path: &std::path::Path, audio_tracks: usize, blue: bool) {
        avi_size(path, audio_tracks, blue, 64, 48);
    }

    pub(crate) fn avi_size(
        path: &std::path::Path,
        audio_tracks: usize,
        blue: bool,
        width: u32,
        height: u32,
    ) {
        avi_timed(path, audio_tracks, blue, width, height, 10, 60);
    }

    pub(crate) fn avi_timed(
        path: &std::path::Path,
        audio_tracks: usize,
        blue: bool,
        width: u32,
        height: u32,
        fps: u32,
        frames: u32,
    ) {
        assert!(fps > 0 && 48000 % fps == 0);
        assert!(audio_tracks <= 2);
        assert!(width > 0 && width <= 1024 && height > 0 && height <= 1024);
        assert_eq!(width % 4, 0, "DIB rows must be padded to four bytes");
        let mut headers = chunk(
            b"avih",
            &words(&[
                1000000 / fps,
                width * height * 3 * fps + audio_tracks as u32 * 96000,
                0,
                0x110,
                frames,
                0,
                1 + audio_tracks as u32,
                width * height * 3,
                width,
                height,
                0,
                0,
                0,
                0,
            ]),
        );
        let mut video = chunk(b"strh", &stream_header(false, width, height, fps, frames));
        let mut bitmap = words(&[40, width, height]);
        bitmap.extend_from_slice(&1_u16.to_le_bytes());
        bitmap.extend_from_slice(&24_u16.to_le_bytes());
        bitmap.extend(words(&[0, width * height * 3, 0, 0, 0, 0]));
        video.extend(chunk(b"strf", &bitmap));
        headers.extend(list(b"strl", &video));
        for _ in 0..audio_tracks {
            let mut audio = chunk(b"strh", &stream_header(true, width, height, fps, frames));
            let mut format = Vec::new();
            format.extend_from_slice(&1_u16.to_le_bytes()); // PCM
            format.extend_from_slice(&1_u16.to_le_bytes()); // mono
            format.extend(words(&[48000, 96000]));
            format.extend_from_slice(&2_u16.to_le_bytes());
            format.extend_from_slice(&16_u16.to_le_bytes());
            audio.extend(chunk(b"strf", &format));
            headers.extend(list(b"strl", &audio));
        }
        let mut movie = Vec::new();
        let mut index = Vec::new();
        let mut append = |id: &[u8; 4], data: &[u8]| {
            index.extend_from_slice(id);
            index.extend(words(&[0x10, movie.len() as u32 + 4, data.len() as u32]));
            movie.extend(chunk(id, data));
        };
        for frame in 0..frames {
            let color = if blue {
                [255, 0, 0]
            } else {
                [20, 70, 30 + (frame % 60) as u8 * 3]
            };
            let pixels: Vec<u8> = (0..width * height).flat_map(|_| color).collect();
            append(b"00db", &pixels);
            for track in 0..audio_tracks {
                let frequency = if track == 0 { 440.0 } else { 880.0 };
                let samples: Vec<u8> = (0..48000 / fps)
                    .flat_map(|sample| {
                        let phase = (f64::from(frame) * f64::from(48000 / fps) + f64::from(sample))
                            * frequency
                            * std::f64::consts::TAU
                            / 48000.0;
                        ((phase.sin() * 4000.0) as i16).to_le_bytes()
                    })
                    .collect();
                append(if track == 0 { b"01wb" } else { b"02wb" }, &samples);
            }
        }
        let mut contents = Vec::from(*b"AVI ");
        contents.extend(list(b"hdrl", &headers));
        contents.extend(list(b"movi", &movie));
        contents.extend(chunk(b"idx1", &index));
        std::fs::write(path, chunk(b"RIFF", &contents)).expect("Write AVI fixture");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Probe a user-supplied file with the bundled decoder, without physical
    /// playback or external tools. Both paths must be explicitly supplied.
    #[test]
    #[ignore = "requires STAR_PLAYER_AUDIO_PROBE and STAR_PLAYER_AUDIO_PROBE_OUTPUT"]
    fn diagnose_audio_file() {
        let path = PathBuf::from(
            std::env::var_os("STAR_PLAYER_AUDIO_PROBE").expect("Set STAR_PLAYER_AUDIO_PROBE"),
        );
        let output = PathBuf::from(
            std::env::var_os("STAR_PLAYER_AUDIO_PROBE_OUTPUT")
                .expect("Set STAR_PLAYER_AUDIO_PROBE_OUTPUT"),
        );
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null"), ("vid", "no")])
                .unwrap();
        player.execute(Command::Load(path)).unwrap();
        smoke_wait(&mut player, "decoded audio", |p| {
            p.media_ready && p.property_node("audio-out-params").is_ok()
        });
        let mut report = serde_json::Map::new();
        for key in [
            "track-list",
            "audio-params",
            "audio-out-params",
            "audio-codec-name",
            "audio-channels",
            "audio-device-list",
            "lavfi-complex",
            "af",
            "volume",
            "speed",
        ] {
            report.insert(
                key.into(),
                player.property_node(key).unwrap_or(serde_json::Value::Null),
            );
        }
        std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }

    #[test]
    #[ignore = "requires explicit media/report paths; opens muted physical output"]
    fn diagnose_headset_backends() {
        let path = PathBuf::from(
            std::env::var_os("STAR_PLAYER_AUDIO_PROBE").expect("Set STAR_PLAYER_AUDIO_PROBE"),
        );
        let output = PathBuf::from(
            std::env::var_os("STAR_PLAYER_AUDIO_PROBE_OUTPUT")
                .expect("Set STAR_PLAYER_AUDIO_PROBE_OUTPUT"),
        );
        let mut player = Player::new_with_options(
            Arc::new(Mailbox::new()),
            &[("vid", "no"), ("audio-device", "openal")],
        )
        .unwrap();
        player.execute(Command::SetMute(true)).unwrap();
        player.execute(Command::Load(path)).unwrap();
        smoke_wait(&mut player, "muted native audio", |p| {
            p.property_node("audio-out-params").is_ok()
        });
        assert_eq!(player.property_node("current-ao").unwrap(), "openal");
        let mut reports = Vec::new();
        for mode in [AudioOutputMode::Stereo, AudioOutputMode::HeadsetCompatible] {
            player.execute(Command::SetAudioOutput(mode)).unwrap();
            smoke_wait(&mut player, "audio backend switch", |p| {
                p.property_node("current-ao").ok().is_some_and(|v| {
                    v == if mode == AudioOutputMode::HeadsetCompatible {
                        "openal"
                    } else {
                        "wasapi"
                    }
                }) && p.property_node("audio-out-params").is_ok()
            });
            assert_eq!(player.flag("mute"), Some(true));
            reports.push(serde_json::json!({
                "mode": format!("{mode:?}"),
                "backend": player.property_node("current-ao").ok(),
                "source": player.property_node("audio-params").ok(),
                "output": player.property_node("audio-out-params").ok(),
                "device": player.property_node("audio-device").ok(),
                "muted": player.flag("mute")
            }));
        }
        std::fs::write(output, serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn surround_audio_downmixes_to_stereo_and_routing_preserves_controls() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("surround.wav");
        // WAVEFORMATEXTENSIBLE PCM, FL/FR/FC/LFE/BL/BR. No external encoder.
        let mut format = Vec::new();
        for n in [0xfffe_u16, 6] {
            format.extend(n.to_le_bytes());
        }
        for n in [48000_u32, 48000 * 12] {
            format.extend(n.to_le_bytes());
        }
        for n in [12_u16, 16, 22, 16] {
            format.extend(n.to_le_bytes());
        }
        format.extend(0x3f_u32.to_le_bytes());
        format.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        let mut samples = Vec::new();
        for frame in 0..48000 * 4 {
            for channel in 0..6 {
                let frequency = if channel == 3 {
                    80.0
                } else {
                    220.0 * (channel + 1) as f64
                };
                let value = (1000.0
                    * (frame as f64 * frequency * std::f64::consts::TAU / 48000.0).sin())
                    as i16;
                samples.extend(value.to_le_bytes());
            }
        }
        let mut data = b"WAVEfmt ".to_vec();
        data.extend((format.len() as u32).to_le_bytes());
        data.extend(format);
        data.extend(b"data");
        data.extend((samples.len() as u32).to_le_bytes());
        data.extend(samples);
        let mut file = b"RIFF".to_vec();
        file.extend((data.len() as u32).to_le_bytes());
        file.extend(data);
        std::fs::write(&path, file).unwrap();

        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path)).unwrap();
        smoke_wait(&mut player, "surround input and stereo output", |p| {
            p.property_node("audio-params")
                .ok()
                .is_some_and(|v| v["channel-count"] == 6)
                && p.property_node("audio-out-params")
                    .ok()
                    .is_some_and(|v| v["channel-count"] == 2)
        });
        assert_eq!(player.state.audio_output, AudioOutputMode::Stereo);
        player.execute(Command::Pause).unwrap();
        player.execute(Command::SetVolume(37.0)).unwrap();
        let graph = player.property_node("lavfi-complex").unwrap();
        for mode in [AudioOutputMode::System, AudioOutputMode::Stereo] {
            player.execute(Command::SetAudioOutput(mode)).unwrap();
            player.refresh();
            assert_eq!(
                player.property_node("audio-channels").unwrap().as_str(),
                Some(mode.channels())
            );
            assert_eq!(player.state.audio_output, mode);
            assert_eq!(player.property_node("lavfi-complex").unwrap(), graph);
            assert_eq!(player.state.volume, 37.0);
            assert!(player.intended_paused && player.state.effective_muted);
        }
    }
    #[test]
    fn preview_dimensions_preserve_aspect_and_bound_allocation() {
        assert_eq!(preview_size(1920, 1080), (240, 135));
        assert_eq!(preview_size(320, 240), (180, 135));
        assert_eq!(preview_size(1080, 1920), (76, 135));
        assert_eq!(preview_size(64, 48), (64, 48));
        assert_eq!(preview_size(0, 1080), (0, 0));
        assert_eq!(preview_size(u32::MAX, 1), (240, 1));
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn bundled_preview_is_paused_silent_bounded_and_cancelable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-two-tracks.avi");
        test_fixtures::avi_size(&path, 2, true, 320, 240);
        let mut decoder = PreviewDecoder::new().expect("Initialize bundled preview decoder");
        let image = decoder
            .decode(&path, 1.0, Instant::now() + Duration::from_secs(5), || {
                false
            })
            .unwrap();
        assert_eq!(image.dimensions(), (180, 135));
        assert!(
            image
                .pixels()
                .all(|p| p[0] < 10 && p[1] < 10 && p[2] > 240 && p[3] == 255)
        );
        assert_eq!(decoder.player.flag("pause"), Some(true));
        assert!((decoder.player.number("time-pos").unwrap() - 1.0).abs() < 0.11);
        assert!(decoder.player.property_node("audio-params").is_err());
        let tracks = decoder.player.property_node("track-list").unwrap();
        assert!(
            tracks
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["type"] == "audio")
                .all(|t| t["selected"] != true)
        );
        assert!(decoder.player.state.tracks.is_empty());
        assert_eq!(decoder.duration(), Some(6.0));
        assert!(
            decoder
                .decode(&path, 2.0, Instant::now() + Duration::from_secs(5), || true)
                .unwrap_err()
                .contains("canceled")
        );
        assert!(
            decoder
                .decode(&path, 2.0, Instant::now(), || false)
                .unwrap_err()
                .contains("timed out")
        );
    }

    #[test]
    fn viewport_fits_display_without_upscaling() {
        assert_eq!(render_size(3840, 2160, (960_u64 << 32) | 540), (960, 540));
        assert_eq!(render_size(320, 240, (64_u64 << 32) | 64), (64, 48));
        assert_eq!(render_size(64, 48, (1920_u64 << 32) | 1080), (64, 48));
        assert_eq!(render_size(7680, 4320, 0), (3840, 2160));
        assert_eq!(render_size(0, 0, 0), (0, 0));
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn saved_audio_is_applied_during_load_and_paused_position_is_restored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("saved-audio.avi");
        test_fixtures::avi(&path, 2, false);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path.clone())).unwrap();
        smoke_wait(&mut player, "initial tracks", |p| p.state.tracks.len() == 2);
        let first = player.state.tracks[0].id;
        let second = player.state.tracks[1].id;
        let gains = std::collections::BTreeMap::from([(first, 0.35), (second, 0.0), (999, 0.8)]);
        player
            .execute(Command::LoadSaved(
                path.clone(),
                LoadState {
                    volume: 28.0,
                    muted: true,
                    gains,
                    position: 1.0,
                    paused: true,
                },
            ))
            .unwrap();
        smoke_wait(&mut player, "saved paused load", |p| {
            p.frame_ready && (p.state.position - 1.0).abs() < 0.11 && p.state.tracks.len() == 2
        });
        assert_eq!(player.state.volume, 28.0);
        assert!(player.state.muted);
        assert!(player.intended_paused);
        assert_eq!(
            player
                .state
                .tracks
                .iter()
                .map(|t| t.gain)
                .collect::<Vec<_>>(),
            [0.35, 0.0]
        );
        let filter = player.property_node("af").unwrap().to_string();
        assert!(filter.contains("0.35"));
        player
            .execute(Command::LoadSaved(
                path,
                LoadState {
                    volume: 28.0,
                    muted: false,
                    gains: Default::default(),
                    position: 0.0,
                    paused: true,
                },
            ))
            .unwrap();
        smoke_wait(&mut player, "default gains on another load", |p| {
            p.frame_ready && p.state.tracks.len() == 2
        });
        assert!(player.state.tracks.iter().all(|t| t.gain == 1.0));
        assert!(!player.state.muted);
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn paused_steps_suppress_audio_without_changing_user_controls() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio-steps.avi");
        test_fixtures::avi(&path, 2, false);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path.clone())).unwrap();
        smoke_wait(&mut player, "two tracks", |p| {
            p.frame_ready && p.state.tracks.len() == 2
        });
        player.execute(Command::Pause).unwrap();
        player.execute(Command::SetVolume(37.0)).unwrap();
        let first = player.state.tracks[0].id;
        let second = player.state.tracks[1].id;
        player
            .execute(Command::SetTrackGain {
                id: first,
                gain: 0.4,
            })
            .unwrap();
        player
            .execute(Command::SetTrackGain {
                id: second,
                gain: 0.6,
            })
            .unwrap();
        let graph = player.property_node("lavfi-complex").unwrap();
        for user_muted in [false, true] {
            player.execute(Command::SetMute(user_muted)).unwrap();
            player.execute(Command::SeekAbsolute(1.0)).unwrap();
            smoke_wait(&mut player, "silent paused seek", |p| {
                p.frame_ready && (p.state.position - 1.0).abs() < 0.01
            });
            for (command, target) in [(Command::FrameStep, 1.1), (Command::FrameBackStep, 1.0)] {
                player.execute(command).unwrap();
                assert_eq!(player.flag("mute"), Some(true));
                player.refresh();
                assert!(player.state.paused);
                smoke_wait(&mut player, "silent step", |p| {
                    assert!(p.state.paused, "Native step cleared logical pause");
                    assert_eq!(p.flag("pause"), Some(true), "Step resumed the audio device");
                    assert_eq!(p.state.muted, user_muted);
                    assert_eq!(p.flag("mute"), Some(true), "Step released effective mute");
                    assert_eq!(p.state.volume, 37.0);
                    assert_eq!(p.property_node("lavfi-complex").unwrap(), graph);
                    assert_eq!(
                        p.state.tracks.iter().map(|t| t.gain).collect::<Vec<_>>(),
                        [0.4, 0.6]
                    );
                    p.flag("pause") == Some(true) && (p.state.position - target).abs() < 0.01
                });
            }
            // Admission is based on logical intent, not mpv's temporary resume.
            player.execute(Command::FrameStep).unwrap();
            player.execute(Command::FrameStep).unwrap();
            player.refresh();
            assert!(player.state.paused);
            assert_eq!(player.flag("mute"), Some(true));
            smoke_wait(&mut player, "repeated silent steps", |p| {
                assert!(p.state.paused);
                assert_eq!(p.flag("pause"), Some(true));
                assert_eq!(p.flag("mute"), Some(true));
                p.flag("pause") == Some(true) && p.state.position >= 1.19
            });
            player.execute(Command::FrameStep).unwrap();
            player.execute(Command::FrameBackStep).unwrap();
            player.execute(Command::FrameStep).unwrap();
            smoke_wait(&mut player, "ordered mixed steps", |p| {
                assert_eq!(p.flag("pause"), Some(true));
                assert_eq!(p.flag("mute"), Some(true));
                p.frame_ready && p.pending_steps.is_empty() && (p.state.position - 1.3).abs() < 0.01
            });
            player.execute(Command::Toggle).unwrap();
            assert!(!player.intended_paused);
            assert_eq!(player.flag("mute"), Some(user_muted));
            player.execute(Command::Pause).unwrap();
            assert_eq!(player.flag("mute"), Some(true));
            player.execute(Command::Play).unwrap();
            assert_eq!(player.flag("mute"), Some(user_muted));
            player.execute(Command::Pause).unwrap();
        }
        player.execute(Command::SetMute(false)).unwrap();
        player.execute(Command::Load(path)).unwrap();
        assert_eq!(player.flag("mute"), Some(false));
        assert!(!player.intended_paused);
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn viewport_redraws_paused_bgra_without_seeking() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("viewport.avi");
        test_fixtures::avi_size(&path, 0, true, 320, 240);
        let playback = Playback::new().unwrap();
        playback.set_viewport(32, 32);
        playback.send(Command::Load(path)).unwrap();
        let wait_frame = |width, height| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(playback.snapshot().error.is_none());
                if let Some(frame) = playback.take_frame() {
                    if (frame.width, frame.height) == (width, height) {
                        return frame;
                    }
                }
                assert!(Instant::now() < deadline, "Viewport redraw timed out");
                thread::sleep(Duration::from_millis(2));
            }
        };
        let first = wait_frame(32, 24);
        assert!(
            first
                .pixels
                .chunks_exact(4)
                .all(|p| p[0] > 240 && p[1] < 10 && p[2] < 10 && p[3] == 255)
        );
        playback.send(Command::Pause).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !playback.snapshot().paused {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let position = playback.snapshot().position;
        playback.set_viewport(64, 64);
        let resized = wait_frame(64, 48);
        assert!(
            resized
                .pixels
                .chunks_exact(4)
                .all(|p| p[0] > 240 && p[1] < 10 && p[2] < 10 && p[3] == 255)
        );
        assert!(playback.snapshot().paused);
        assert!((playback.snapshot().position - position).abs() < 0.01);
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn high_fps_fixture_decodes_and_steps_one_frame() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("60fps.avi");
        test_fixtures::avi_timed(&path, 0, false, 64, 48, 60, 120);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path)).unwrap();
        smoke_wait(&mut player, "60fps video", |p| {
            p.frame_ready && p.mailbox.frame.lock().unwrap().is_some()
        });
        assert_eq!(player.number("container-fps"), Some(60.0));
        player.execute(Command::Pause).unwrap();
        player.execute(Command::SeekAbsolute(0.5)).unwrap();
        smoke_wait(&mut player, "60fps seek", |p| {
            p.frame_ready && (p.state.position - 0.5).abs() < 0.001
        });
        player.execute(Command::FrameStep).unwrap();
        smoke_wait(&mut player, "60fps step", |p| {
            p.flag("pause") == Some(true) && p.state.position > 0.51
        });
        assert!((player.state.position - 0.5 - 1.0 / 60.0).abs() < 0.001);
    }

    /// Timing-sensitive measurement, separate from the deterministic decoding test.
    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    #[ignore = "opt-in native render throughput measurement"]
    fn software_60fps_render_throughput() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("throughput.avi");
        test_fixtures::avi_timed(&path, 0, false, 320, 240, 60, 120);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut delivered = 0;
        let mut last_color = None;
        loop {
            assert!(player.poll_events());
            player.refresh();
            assert!(player.state.error.is_none(), "{:?}", player.state.error);
            if let Some(frame) = player.mailbox.frame.lock().unwrap().take() {
                let color = frame.pixels[2];
                if last_color != Some(color) {
                    delivered += 1;
                    last_color = Some(color);
                }
            }
            if player.state.position >= 1.95 {
                break;
            }
            assert!(Instant::now() < deadline, "60fps playback stalled");
            thread::sleep(Duration::from_millis(2));
        }
        // Allow scheduler jitter, but detect a 30fps cap on this 120-frame clip.
        assert!(
            delivered >= 90,
            "Only {delivered} distinct frames delivered from 120 at 60fps"
        );
    }

    #[test]
    fn log_severity_is_diagnostic_not_playback_failure() {
        let mut state = Snapshot::default();
        for (level, severity) in [
            ("fatal", DiagnosticSeverity::Fatal),
            ("error", DiagnosticSeverity::Error),
            ("warn", DiagnosticSeverity::Warning),
            ("info", DiagnosticSeverity::Info),
            ("debug", DiagnosticSeverity::Debug),
            ("trace", DiagnosticSeverity::Trace),
            ("future-level", DiagnosticSeverity::Unknown),
        ] {
            state.record_diagnostic(level, "h264", "mmco: unref short failure\n");
            assert!(state.error.is_none());
            assert_eq!(state.diagnostics.last().unwrap().severity, severity);
            assert_eq!(
                state.diagnostic.as_deref(),
                Some("h264: mmco: unref short failure")
            );
        }
        state.error = Some("Seek failed".to_owned());
        state.record_diagnostic("error", "h264", "Recovered decoder diagnostic");
        assert_eq!(state.error.as_deref(), Some("Seek failed"));
        for reason in [0, 2, 3, 5] {
            assert!(
                end_file_error(
                    reason,
                    Some("Not a failure".into()),
                    state.diagnostic.as_deref()
                )
                .is_none()
            );
        }
        let fatal = end_file_error(
            4,
            Some("Unsupported media".into()),
            state.diagnostic.as_deref(),
        )
        .unwrap();
        assert!(fatal.contains("Unsupported media"));
        assert!(fatal.contains("Recovered decoder diagnostic"));
        assert_eq!(
            end_file_error(4, None, None).as_deref(),
            Some("Media playback failed")
        );
    }

    #[test]
    fn diagnostic_history_is_bounded_utf8_safe_and_resettable() {
        let mut state = Snapshot::default();
        state.record_diagnostic("warn", &"界".repeat(1000), &"é".repeat(4000));
        assert_eq!(state.diagnostics[0].prefix.chars().count(), 128);
        assert_eq!(
            state.diagnostics[0].message.chars().count(),
            MAX_DIAGNOSTIC_CHARS
        );
        state.clear_diagnostics();
        for index in 0..20 {
            state.record_diagnostic("error", "decoder", &format!("diagnostic {index}"));
        }
        assert_eq!(state.diagnostics.len(), MAX_DIAGNOSTICS);
        assert_eq!(state.diagnostics[0].message, "diagnostic 4");
        state.record_diagnostic("error", "decoder", "diagnostic 19");
        state.record_diagnostic("error", "decoder", " \n ");
        assert_eq!(state.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(state.error.is_none());
        state.clear_diagnostics();
        assert!(state.diagnostics.is_empty());
        assert!(state.diagnostic.is_none());
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn malformed_media_still_reports_fatal_error_and_valid_load_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let malformed = directory.path().join("malformed.mkv");
        std::fs::write(&malformed, [0_u8; 128]).unwrap();
        let valid = directory.path().join("valid.avi");
        test_fixtures::avi(&valid, 0, true);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(malformed)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.state.error.is_none() {
            assert!(player.poll_events());
            player.refresh();
            assert!(
                Instant::now() < deadline,
                "Malformed file did not report a fatal error"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            player
                .state
                .error
                .as_ref()
                .unwrap()
                .contains("Load/play media")
        );
        assert!(!player.media_ready && !player.frame_ready);
        assert!(!player.state.playback_started);
        assert!(
            !player.state.diagnostics.is_empty(),
            "Native diagnostic was lost"
        );
        assert!(
            player
                .state
                .error
                .as_ref()
                .unwrap()
                .contains("Decoder diagnostic:")
        );
        player.execute(Command::Load(valid)).unwrap();
        assert!(player.state.error.is_none());
        assert!(player.state.diagnostic.is_none());
        assert!(player.state.diagnostics.is_empty());
        smoke_wait(&mut player, "valid media after malformed load", |p| {
            p.frame_ready && p.mailbox.frame.lock().unwrap().is_some()
        });
    }

    fn track(id: i64, gain: f64) -> AudioTrack {
        AudioTrack {
            id,
            title: String::new(),
            gain,
        }
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn watched_signal_requires_playback_not_just_loading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.avi");
        test_fixtures::avi(&path, 0, true);
        let mut player =
            Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
        player.execute(Command::Load(path.clone())).unwrap();
        player.execute(Command::Pause).unwrap();
        smoke_wait(&mut player, "paused initial load", |p| {
            p.frame_ready && p.state.paused
        });
        assert_eq!(player.state.path, Some(path.clone()));
        assert!(!player.state.playback_started);
        player.execute(Command::Play).unwrap();
        smoke_wait(&mut player, "watched playback signal", |p| {
            p.state.playback_started
        });
        player.execute(Command::Pause).unwrap();
        player.refresh();
        assert!(
            player.state.playback_started,
            "Started signal must survive pausing"
        );
        player.execute(Command::Load(path)).unwrap();
        assert!(
            !player.state.playback_started,
            "New load must reset started signal"
        );
    }
    #[test]
    fn empty_graph_disables_complex_filter() {
        assert_eq!(mix_graph(&[]).unwrap(), "");
        assert_eq!(gain_filter(&[]).unwrap(), "");
    }
    #[test]
    fn input_graph_does_not_depend_on_track_gains() {
        assert_eq!(
            mix_graph(&[track(1, 1.0), track(7, 0.0)]).unwrap(),
            mix_graph(&[track(1, 0.2), track(7, 0.8)]).unwrap()
        );
    }
    #[test]
    fn single_track_still_applies_gain() {
        assert_eq!(mix_graph(&[track(3, 0.5)]).unwrap(), "[aid3]anull[ao]");
        assert_eq!(
            gain_filter(&[track(3, 0.5)]).unwrap(),
            "@track-gains:lavfi=[volume@track3=0.5]"
        );
    }
    #[test]
    fn mixes_all_tracks_without_normalizing_or_dropping_muted_inputs() {
        assert_eq!(
            gain_filter(&[track(1, 1.0), track(7, 0.0)]).unwrap(),
            "@track-gains:lavfi=[asplit=2[input0][input1];[input0]pan=stereo|c0=c0|c1=c1,volume@track1=1[mix0];[input1]pan=stereo|c0=c2|c1=c3,volume@track7=0[mix1];[mix0][mix1]amix=inputs=2:duration=longest:dropout_transition=0:normalize=0]"
        );
    }
    #[test]
    fn rejects_invalid_gains_and_ids() {
        for gain in [f64::NAN, f64::INFINITY, -1.0, 10.1] {
            assert!(mix_graph(&[track(1, gain)]).is_err());
        }
        assert!(mix_graph(&[track(0, 1.0)]).is_err());
        assert!(mix_graph(&[track(1, 1.0), track(1, 0.0)]).is_err());
    }

    #[test]
    fn defaults_all_audio_on_and_preserves_individual_gains() {
        let value = serde_json::json!([
            {"type":"video", "id":1},
            {"type":"audio", "id":3, "title":"Commentary"},
            {"type":"audio", "id":8, "lang":"eng"},
            {"type":"audio", "id":0}
        ]);
        let tracks = audio_tracks(&value, &[]);
        assert_eq!(
            tracks.iter().map(|t| (t.id, t.gain)).collect::<Vec<_>>(),
            vec![(3, 1.0), (8, 1.0)]
        );
        assert_eq!(tracks[0].title, "Commentary");
        assert_eq!(tracks[1].title, "eng");
        let tracks = audio_tracks(&value, &[track(3, 0.0)]);
        assert_eq!(
            tracks.iter().map(|t| t.gain).collect::<Vec<_>>(),
            vec![0.0, 1.0]
        );
        let tracks = audio_tracks(&value, &[track(3, 0.25), track(8, 0.75)]);
        assert_eq!(
            tracks.iter().map(|t| t.gain).collect::<Vec<_>>(),
            vec![0.25, 0.75]
        );
    }

    fn test_frame() -> VideoFrame {
        VideoFrame {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn invalidation_rejects_late_frames_even_at_identical_dimensions() {
        let mailbox = Mailbox::new();
        let size = (1_u64 << 32) | 1;
        mailbox.size.store(size, Ordering::Release);
        mailbox.publish_frame(0, size, test_frame());
        assert!(mailbox.frame.lock().unwrap().is_some());
        mailbox.invalidate_frames();
        assert!(mailbox.frame.lock().unwrap().is_none());
        mailbox.size.store(size, Ordering::Release);
        mailbox.publish_frame(0, size, test_frame());
        assert!(mailbox.frame.lock().unwrap().is_none());
        mailbox.publish_frame(1, size, test_frame());
        assert!(mailbox.frame.lock().unwrap().is_some());
        mailbox.size.store(0, Ordering::Release);
        mailbox.publish_frame(1, size, test_frame());
        assert!(mailbox.frame.lock().unwrap().take().is_some());
        assert!(mailbox.frame.lock().unwrap().is_none());
    }

    #[test]
    fn software_render_parameter_abi_is_explicit() {
        assert_eq!(
            [
                RENDER_API_TYPE,
                RENDER_SW_SIZE,
                RENDER_SW_FORMAT,
                RENDER_SW_STRIDE,
                RENDER_SW_POINTER
            ],
            [1, 17, 18, 19, 20]
        );
        let mut size: [c_int; 2] = [64, 48];
        let mut stride: usize = 256;
        let mut pixels = [0_u32; 64];
        let params = [
            RenderParam::new(RENDER_SW_SIZE, size.as_mut_ptr().cast()),
            RenderParam::new(RENDER_SW_STRIDE, (&mut stride as *mut usize).cast()),
            RenderParam::new(RENDER_SW_POINTER, pixels.as_mut_ptr().cast()),
            RenderParam::end(),
        ];
        assert_eq!(unsafe { *params[0].data.cast::<c_int>().add(1) }, 48);
        assert_eq!(unsafe { *params[1].data.cast::<usize>() }, 256);
        assert_eq!(params[2].data, pixels.as_mut_ptr().cast());
        assert_eq!(params[3].kind, 0);
        assert!(params[3].data.is_null());
    }

    #[test]
    fn rejects_nonfinite_control_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(finite_range(value, 0.0, 100.0, "Volume").is_err());
        }
        assert!(finite_range(0.0, 0.0, 100.0, "Volume").is_ok());
        assert!(finite_range(100.0, 0.0, 100.0, "Volume").is_ok());
    }

    fn smoke_wait(
        player: &mut Player,
        label: &str,
        mut predicate: impl FnMut(&mut Player) -> bool,
    ) {
        let start = Instant::now();
        loop {
            assert!(
                player.poll_events(),
                "mpv shut down while waiting for {label}"
            );
            player.refresh();
            assert!(
                player.state.error.is_none(),
                "{label}: {:?}",
                player.state.error
            );
            assert!(
                player.mailbox.render_error.lock().unwrap().is_none(),
                "Software render failed"
            );
            if predicate(player) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Timed out waiting for {label}: {:?}; seeking={:?}; frame={:?}; audio_pts={:?}",
                player.state,
                player.flag("seeking"),
                player.property_node("video-frame-info"),
                player.number("audio-pts")
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn track_gain_changes_do_not_restart_playback() {
        let directory = tempfile::tempdir().unwrap();
        for count in [1, 2] {
            let path = directory.path().join(format!("{count}-tracks.avi"));
            test_fixtures::avi(&path, count, false);
            let mut player =
                Player::new_with_options(Arc::new(Mailbox::new()), &[("ao", "null")]).unwrap();
            player.execute(Command::Load(path)).unwrap();
            smoke_wait(&mut player, "playing audio and video", |p| {
                p.frame_ready && p.state.tracks.len() == count && p.state.position > 0.3
            });
            let graph = player.property_node("lavfi-complex").unwrap();
            let filters = player.property_node("af").unwrap();
            let before = player.state.position;
            let ids = player.state.tracks.iter().map(|t| t.id).collect::<Vec<_>>();
            for step in 0..20 {
                for &id in &ids {
                    player
                        .execute(Command::SetTrackGain {
                            id,
                            gain: step as f64 / 20.0,
                        })
                        .unwrap();
                }
                thread::sleep(Duration::from_millis(20));
                loop {
                    let event = unsafe { &*(player.api.wait)(player.handle, 0.0) };
                    if event.id == 0 {
                        break;
                    }
                    assert!(
                        !matches!(event.id, 20 | 21),
                        "Gain adjustment caused seek/restart event {}",
                        event.id
                    );
                }
                assert_eq!(player.flag("pause"), Some(false));
                assert_eq!(player.flag("seeking"), Some(false));
            }
            player.refresh();
            assert!(player.state.position > before + 0.2);
            assert_eq!(player.property_node("lavfi-complex").unwrap(), graph);
            assert_eq!(player.property_node("af").unwrap(), filters);
        }
    }

    /// Uses bundled libmpv and a null audio device. Asserts graph readback,
    /// both selected audio inputs and decoding, not physical acoustic output.
    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn real_libmpv_smoke() {
        let directory = tempfile::tempdir().expect("Create fixture directory");
        let multiple = directory.path().join("two-tracks.avi");
        let single = directory.path().join("one-track.avi");
        let blue = directory.path().join("blue.avi");
        test_fixtures::avi(&multiple, 2, false);
        test_fixtures::avi(&single, 1, false);
        test_fixtures::avi(&blue, 0, true);

        let mailbox = Arc::new(Mailbox::new());
        // The control path is driven on the test thread; the actual software render
        // thread and libmpv core run normally. This permits property readback without
        // adding debug/probe commands to the public API.
        let mut player = Player::new_with_options(mailbox.clone(), &[("ao", "null")])
            .expect("Initialize bundled libmpv");
        player.execute(Command::Load(multiple)).unwrap();
        smoke_wait(&mut player, "duration, video and two audio tracks", |p| {
            (p.state.duration - 6.0).abs() < 0.2
                && p.state.tracks.len() == 2
                && p.mailbox.frame.lock().unwrap().is_some()
        });
        assert!(
            player
                .state
                .metadata
                .get("file-format")
                .is_some_and(|value| value.is_string())
        );
        assert_eq!(player.state.metadata["video-params"]["w"], 64);
        assert_eq!(player.state.metadata["video-params"]["h"], 48);
        assert!(
            player.state.metadata["track-list"]
                .as_array()
                .unwrap()
                .len()
                >= 3
        );
        assert_eq!(
            player
                .state
                .tracks
                .iter()
                .map(|t| t.gain)
                .collect::<Vec<_>>(),
            vec![1.0, 1.0]
        );
        let frame = mailbox.frame.lock().unwrap().take().unwrap();
        assert_eq!((frame.width, frame.height), (64, 48));
        assert_eq!(frame.pixels.len(), 64 * 48 * 4);
        assert!(frame.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        assert!(
            frame
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [0, 0, 0])
        );

        let first = player.state.tracks[0].id;
        let second = player.state.tracks[1].id;
        player.execute(Command::Pause).unwrap();
        player.execute(Command::SeekAbsolute(1.0)).unwrap();
        assert!(
            !player.frame_ready,
            "Seek kept readiness from the previous frame"
        );
        assert!(mailbox.frame.lock().unwrap().is_none());
        smoke_wait(&mut player, "paused seek", |p| {
            p.frame_ready
                && p.state.paused
                && (p.state.position - 1.0).abs() < 0.06
                && p.mailbox
                    .frame
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|f| f.pixels[2] == 60)
        });
        let sought_frame = mailbox.frame.lock().unwrap().take().unwrap();
        assert_eq!(&sought_frame.pixels[..4], &[20, 70, 60, 255]);
        let before = player.state.position;
        player.execute(Command::FrameStep).unwrap();
        smoke_wait(&mut player, "forward frame step", |p| {
            p.state.paused
                && p.state.position > before + 0.04
                && p.mailbox
                    .frame
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|f| f.pixels[2] == 63)
        });
        let stepped_frame = mailbox.frame.lock().unwrap().take().unwrap();
        assert_eq!(&stepped_frame.pixels[..4], &[20, 70, 63, 255]);
        assert!((player.state.position - before - 0.1).abs() < 0.01);
        let forward = player.state.position;
        player.execute(Command::FrameBackStep).unwrap();
        smoke_wait(&mut player, "backward frame step", |p| {
            p.frame_ready
                && p.state.paused
                && p.state.position < forward - 0.04
                && p.mailbox
                    .frame
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|f| f.pixels[2] == 60)
        });
        let backstepped_frame = mailbox.frame.lock().unwrap().take().unwrap();
        assert_eq!(backstepped_frame.pixels, sought_frame.pixels);
        assert!((player.state.position - before).abs() < 0.01);

        player
            .execute(Command::SetTrackGain {
                id: first,
                gain: 0.35,
            })
            .unwrap();
        player
            .execute(Command::SetTrackGain {
                id: second,
                gain: 0.6,
            })
            .unwrap();
        assert_eq!(
            player.property_node("lavfi-complex").unwrap().as_str(),
            Some(mix_graph(&player.state.tracks).unwrap().as_str())
        );
        player.execute(Command::Play).unwrap();
        smoke_wait(&mut player, "simultaneous audio decoding", |p| {
            let tracks = p.property_node("track-list").unwrap();
            let selected = tracks
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["type"] == "audio" && t["selected"] == true)
                .count();
            selected == 2 && p.property_node("audio-params").is_ok() && p.state.position > 1.3
        });
        assert!(player.execute(Command::FrameStep).is_err());
        assert!(player.execute(Command::FrameBackStep).is_err());
        player.execute(Command::Stop).unwrap();
        smoke_wait(&mut player, "stop at paused beginning", |p| {
            p.frame_ready
                && p.state.paused
                && p.state.position < 0.06
                && p.mailbox.frame.lock().unwrap().is_some()
        });
        assert!((player.state.duration - 6.0).abs() < 0.2);
        assert_eq!(
            player
                .state
                .tracks
                .iter()
                .map(|t| t.gain)
                .collect::<Vec<_>>(),
            vec![0.35, 0.6]
        );

        player.execute(Command::Load(blue)).unwrap();
        assert!(
            player.state.metadata.is_empty(),
            "Metadata from the previous file leaked into loading"
        );
        assert!(mailbox.frame.lock().unwrap().is_none());
        smoke_wait(&mut player, "same-size replacement video", |p| {
            p.mailbox.frame.lock().unwrap().is_some()
        });
        let frame = mailbox.frame.lock().unwrap().take().unwrap();
        assert!(
            frame
                .pixels
                .chunks_exact(4)
                .all(|pixel| pixel[2] < 10 && pixel[1] < 10 && pixel[0] > 240),
            "Replacement published an old frame"
        );

        player.execute(Command::Load(single)).unwrap();
        smoke_wait(&mut player, "single audio track", |p| {
            p.state.tracks.len() == 1 && p.media_ready
        });
        assert_eq!(player.state.tracks[0].gain, 1.0);
        let id = player.state.tracks[0].id;
        player
            .execute(Command::SetTrackGain { id, gain: 0.25 })
            .unwrap();
        assert_eq!(
            player.property_node("lavfi-complex").unwrap().as_str(),
            Some(format!("[aid{id}]anull[ao]").as_str())
        );
        // Exercise reload after a gain change, then keep the single-track slider.
        player.execute(Command::Stop).unwrap();
        smoke_wait(&mut player, "single-track stop", |p| {
            p.state.paused && p.state.position < 0.06
        });
        assert_eq!(player.state.tracks[0].gain, 0.25);
        drop(player);
    }
}
