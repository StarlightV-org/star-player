//! Originals are read-only inputs. Only our staging file can ever be written.
use serde_json::Value;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct AudioStream {
    pub index: u32,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct Media {
    pub duration: f64,
    pub video: Option<u32>,
    pub video_codec: Option<String>,
    pub audio: Vec<AudioStream>,
    pub hdr: bool,
    pub width: u64,
    pub height: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mp4,
    Mkv,
    Webm,
    Mp3,
}
impl Format {
    pub const OPTIONS: [Self; 4] = [Self::Mp4, Self::Mkv, Self::Webm, Self::Mp3];

    pub fn audio_only(self) -> bool {
        self == Self::Mp3
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4 · H.264 / AAC",
            Self::Mkv => "MKV · H.264 / AAC",
            Self::Webm => "WebM · VP9 / Opus",
            Self::Mp3 => "Audio only · MP3",
        }
    }
    fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    High,
    Balanced,
    Small,
}
impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "High quality",
            Self::Balanced => "Balanced",
            Self::Small => "Smaller file",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    Separate,
    Merge,
    None,
}

#[derive(Clone, Debug)]
pub struct AudioSelection {
    pub index: u32,
    pub gain: f64,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub start: f64,
    pub end: f64,
    pub format: Format,
    pub quality: Quality,
    pub audio_mode: AudioMode,
    pub audio: Vec<AudioSelection>,
}

pub enum Event {
    Probed(Result<Media, String>),
    Tools(Result<Option<PathBuf>, String>),
    DownloadProgress(u64, u64),
    Progress(f64),
    Finished(Result<PathBuf, String>),
}

pub struct Job {
    cancel: Arc<AtomicBool>,
    events: mpsc::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
    join_on_drop: bool,
}
impl Job {
    pub fn find_tools() -> Self {
        Self::spawn(|stop, tx| {
            let _ = tx.send(Event::Tools(crate::export_tools::find(stop)));
        })
    }
    pub fn download_tools() -> Self {
        Self::spawn(|stop, tx| {
            let result = crate::export_tools::download(stop, |done, total| {
                let _ = tx.send(Event::DownloadProgress(done, total));
            });
            let _ = tx.send(Event::Tools(result.map(Some)));
        })
    }
    pub fn select_tools(path: PathBuf) -> Self {
        Self::spawn(move |stop, tx| {
            let _ = tx.send(Event::Tools(
                crate::export_tools::select(&path, stop).map(Some),
            ));
        })
    }
    pub fn probe(source: PathBuf) -> Self {
        Self::spawn(move |stop, tx| {
            let result = (|| {
                let _read_only = source_guard(&source)?;
                let source = fs::canonicalize(source).map_err(|e| e.to_string())?;
                probe(&source, stop)
            })();
            let _ = tx.send(Event::Probed(result));
        })
    }
    pub fn export(spec: Export) -> Self {
        let mut job = Self::spawn(move |stop, tx| {
            let result = export(&spec, stop, tx);
            let _ = tx.send(Event::Finished(result));
        });
        // Finish killing the child and cleaning staging before app shutdown.
        job.join_on_drop = true;
        job
    }
    fn spawn(work: impl FnOnce(&AtomicBool, &mpsc::Sender<Event>) + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (tx, events) = mpsc::channel();
        let thread = thread::spawn(move || work(&stop, &tx));
        Self {
            cancel,
            events,
            thread: Some(thread),
            join_on_drop: false,
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            // Do not wait on network preparation. An encoder job must finish
            // terminating its child before the application's process exits.
            if self.join_on_drop || thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

fn check_cancel(stop: &AtomicBool) -> Result<(), String> {
    if stop.load(Ordering::Acquire) {
        Err("Export cancelled".into())
    } else {
        Ok(())
    }
}

fn source_guard(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Deny writes, renames, and deletion while the job holds this handle.
        // FFmpeg and the libmpv inspector can open additional read-only handles.
        options.share_mode(1); // FILE_SHARE_READ
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot open original read-only: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("The source must be a local regular file".into());
    }
    Ok(file)
}

fn destination(path: &Path) -> Result<PathBuf, String> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(
            "Choose a new filename. Existing files, including the original, are never overwritten"
                .into(),
        );
    }
    let name = path.file_name().ok_or("Choose an output filename")?;
    let text = name.to_string_lossy();
    if text.contains(':') || text.ends_with(['.', ' ']) {
        return Err(
            "Choose a normal filename without alternate data streams or trailing dots/spaces"
                .into(),
        );
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|e| format!("Cannot access output folder: {e}"))?;
    Ok(parent.join(name))
}

fn probe(source: &Path, stop: &AtomicBool) -> Result<Media, String> {
    let value = crate::playback::inspect_media(source, stop)?;
    parse_media(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)
}

fn parse_media(output: &[u8]) -> Result<Media, String> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|e| format!("Cannot read media information: {e}"))?;
    let streams = value["track-list"].as_array().ok_or("No streams found")?;
    let video = streams
        .iter()
        .find(|s| s["type"] == "video" && s["albumart"] != true && s["external"] != true);
    let first_audio = streams
        .iter()
        .find(|s| s["type"] == "audio" && s["external"] != true);
    if video.is_none() && first_audio.is_none() {
        return Err("This file has no video or audio streams".into());
    }
    let duration = value["duration"]
        .as_f64()
        .filter(|d| d.is_finite() && *d > 0.)
        .ok_or("A finite media duration is required for export")?;
    let video_index = video
        .map(|s| {
            s["ff-index"]
                .as_u64()
                .and_then(|i| u32::try_from(i).ok())
                .ok_or("Invalid video stream index")
        })
        .transpose()?;
    let audio = streams
        .iter()
        .filter(|s| s["type"] == "audio" && s["external"] != true)
        .enumerate()
        .map(|(i, s)| {
            let index = s["ff-index"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("Missing FFmpeg audio stream index")?;
            let title = s["title"]
                .as_str()
                .or_else(|| s["lang"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Audio {}", i + 1));
            Ok(AudioStream { index, title })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let params = &value["video-dec-params"];
    if video.is_some() && !params["gamma"].is_string() {
        return Err(
            "Cannot determine source color transfer; refusing an unclassified video export".into(),
        );
    }
    Ok(Media {
        duration,
        video: video_index,
        video_codec: video.and_then(|s| s["codec"].as_str()).map(str::to_owned),
        audio,
        hdr: matches!(
            params["gamma"].as_str(),
            Some("pq" | "hlg" | "smpte2084" | "arib-std-b67")
        ) || video.is_some_and(|v| v["dolby-vision-profile"].as_i64().is_some_and(|p| p > 0)),
        width: video
            .and_then(|s| s["demux-w"].as_u64().or_else(|| params["w"].as_u64()))
            .unwrap_or(0),
        height: video
            .and_then(|s| s["demux-h"].as_u64().or_else(|| params["h"].as_u64()))
            .unwrap_or(0),
    })
}

fn validate(spec: &Export, media: &Media) -> Result<(), String> {
    if !spec.start.is_finite()
        || !spec.end.is_finite()
        || spec.start < 0.
        || spec.start >= spec.end
        || spec.end > media.duration + 0.001
    {
        return Err("Select a non-empty range within the source duration".into());
    }
    if !spec.format.audio_only() && media.video.is_none() {
        return Err("Choose an audio-only format for a source without video".into());
    }
    if spec.format.audio_only() && (spec.audio_mode == AudioMode::None || spec.audio.is_empty()) {
        return Err("Select at least one audio track for audio-only export".into());
    }
    if spec.format.audio_only() && spec.audio_mode != AudioMode::Merge {
        return Err("MP3 exports use a single mixed audio track".into());
    }
    if media.hdr && !spec.format.audio_only() {
        return Err(
            "HDR export is not supported by these SDR presets. The original has not been changed"
                .into(),
        );
    }
    let mut seen = std::collections::HashSet::new();
    for track in &spec.audio {
        if !track.gain.is_finite()
            || !(0.0..=10.0).contains(&track.gain)
            || !seen.insert(track.index)
            || !media.audio.iter().any(|a| a.index == track.index)
        {
            return Err("Invalid audio stream selection or gain".into());
        }
    }
    Ok(())
}

fn arguments(spec: &Export, media: &Media, staging: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-xerror",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.25",
        "-protocol_whitelist",
        "file,pipe",
        "-accurate_seek",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if spec.format.audio_only() {
        // An MP3 job must not decode unused AV1 video while reading its input.
        args.push("-vn".into());
    } else if media.video_codec.as_deref() == Some("av1") {
        // NVENC/Overlay recordings can declare AV1 levels rejected by libaom.
        // The full pinned bundle includes dav1d. Scope this INPUT decoder to
        // the selected source stream, not to other video streams or the output.
        args.extend([
            format!("-c:{}", media.video.expect("validated video stream")).into(),
            "libdav1d".into(),
        ]);
    }
    args.push("-ss".into());
    args.push(format!("{:.9}", spec.start).into());
    args.push("-i".into());
    args.push(spec.source.as_os_str().to_owned());
    args.extend(["-t".into(), format!("{:.9}", spec.end - spec.start).into()]);
    args.extend(
        ["-map_metadata", "-1", "-map_chapters", "-1", "-sn", "-dn"]
            .into_iter()
            .map(Into::into),
    );
    if spec.format.audio_only() {
        args.push("-vn".into());
    } else {
        args.extend([
            "-map".into(),
            format!("0:{}", media.video.expect("validated video stream")).into(),
        ]);
        args.extend(
            ["-fps_mode", "passthrough", "-enc_time_base:v", "demux"]
                .into_iter()
                .map(Into::into),
        );
        // Prefer compatible 4:2:0 without silently scaling odd-sized sources.
        args.extend(
            [
                "-pix_fmt",
                if media.width % 2 == 0 && media.height % 2 == 0 {
                    "yuv420p"
                } else {
                    "yuv444p"
                },
            ]
            .into_iter()
            .map(Into::into),
        );
        if spec.format == Format::Webm {
            let crf = match spec.quality {
                Quality::High => "24",
                Quality::Balanced => "31",
                Quality::Small => "38",
            };
            args.extend(
                [
                    "-c:v",
                    "libvpx-vp9",
                    "-crf",
                    crf,
                    "-b:v",
                    "0",
                    "-cpu-used",
                    "4",
                ]
                .into_iter()
                .map(Into::into),
            );
        } else {
            let crf = match spec.quality {
                Quality::High => "18",
                Quality::Balanced => "23",
                Quality::Small => "28",
            };
            args.extend(
                ["-c:v", "libx264", "-crf", crf, "-preset", "medium"]
                    .into_iter()
                    .map(Into::into),
            );
        }
    }
    if spec.audio_mode == AudioMode::None || spec.audio.is_empty() {
        args.push("-an".into());
    } else {
        let mut graph = String::new();
        for (i, track) in spec.audio.iter().enumerate() {
            let layout = if spec.audio_mode == AudioMode::Merge {
                ",aformat=channel_layouts=stereo"
            } else {
                ""
            };
            graph.push_str(&format!(
                "[0:{}]aresample=48000:async=1:first_pts=0{layout},volume={:.6}[a{i}];",
                track.index, track.gain
            ));
        }
        if spec.audio_mode == AudioMode::Merge {
            for i in 0..spec.audio.len() {
                graph.push_str(&format!("[a{i}]"));
            }
            graph.push_str(&format!("amix=inputs={}:duration=longest:dropout_transition=0:normalize=0,alimiter=limit=0.95:level=0:latency=1[mixed]", spec.audio.len()));
            args.extend([
                "-filter_complex".into(),
                graph.into(),
                "-map".into(),
                "[mixed]".into(),
            ]);
        } else {
            // Protect boosted separate tracks against clipping too.
            for i in 0..spec.audio.len() {
                graph.push_str(&format!(
                    "[a{i}]alimiter=limit=0.95:level=0:latency=1[out{i}];"
                ));
            }
            graph.pop();
            args.extend(["-filter_complex".into(), graph.into()]);
            for i in 0..spec.audio.len() {
                args.extend(["-map".into(), format!("[out{i}]").into()]);
            }
        }
        args.extend(
            [
                "-c:a",
                if spec.format == Format::Webm {
                    "libopus"
                } else if spec.format == Format::Mp3 {
                    "libmp3lame"
                } else {
                    "aac"
                },
                "-b:a",
                if spec.format.audio_only() {
                    match spec.quality {
                        Quality::High => "256k",
                        Quality::Balanced => "192k",
                        Quality::Small => "128k",
                    }
                } else {
                    "192k"
                },
            ]
            .into_iter()
            .map(Into::into),
        );
    }
    if spec.format == Format::Mp4 {
        args.extend(["-movflags", "+faststart"].into_iter().map(Into::into));
    }
    args.extend([
        "-f".into(),
        spec.format.muxer().into(),
        staging.as_os_str().to_owned(),
    ]);
    args
}

fn export(spec: &Export, stop: &AtomicBool, tx: &mpsc::Sender<Event>) -> Result<PathBuf, String> {
    check_cancel(stop)?;
    // This handle is never granted write access and stays alive until publication.
    let _read_only = source_guard(&spec.source)?;
    let source = fs::canonicalize(&spec.source).map_err(|e| e.to_string())?;
    let target = destination(&spec.destination)?;
    if target
        .extension()
        .and_then(|e| e.to_str())
        .is_none_or(|e| !e.eq_ignore_ascii_case(spec.format.extension()))
    {
        return Err(format!(
            "The output filename must end in .{}",
            spec.format.extension()
        ));
    }
    let ffmpeg = crate::export_tools::find(stop)?
        .ok_or("FFmpeg is not installed. Download it from the clip editor before exporting")?;
    check_cancel(stop)?;
    let media = probe(&source, stop)?;
    validate(spec, &media)?;
    let directory = tempfile::Builder::new()
        .prefix(".star-player-export-")
        .tempdir_in(target.parent().ok_or("Missing output folder")?)
        .map_err(|e| format!("Cannot create export staging folder: {e}"))?;
    let staging = directory.path().join("media.partial");
    let mut frozen = spec.clone();
    frozen.source = source;
    run(
        &ffmpeg,
        &arguments(&frozen, &media, &staging),
        stop,
        Some((tx, spec.end - spec.start)),
    )?;
    let output = probe(&staging, stop)?;
    if spec.format.audio_only() != output.video.is_none() {
        return Err("The exported video streams did not match the selected format".into());
    }
    let expected = spec.end - spec.start;
    if (output.duration - expected).abs() > 0.25_f64.max(expected * 0.01) {
        return Err("The export duration did not match the selected range".into());
    }
    let expected_audio = if spec.audio_mode == AudioMode::None || spec.audio.is_empty() {
        0
    } else if spec.audio_mode == AudioMode::Merge {
        1
    } else {
        spec.audio.len()
    };
    if output.audio.len() != expected_audio {
        return Err("The exported audio streams did not match the selection".into());
    }
    check_cancel(stop)?;
    OpenOptions::new()
        .write(true)
        .open(&staging)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    publish(&staging, &target)
        .map_err(|e| format!("Cannot publish export without overwriting files: {e}"))?;
    Ok(target)
}

fn publish(staging: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let source: Vec<u16> = staging.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // No MOVEFILE_REPLACE_EXISTING and no cross-volume copy fallback.
        // Also works on FAT/exFAT, where hard-link publication is unavailable.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                0,
            )
        } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(staging, target)
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain(mut input: impl Read, limit: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    while let Ok(count) = input.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let keep = count.min(limit.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
    }
    result
}

pub(crate) fn run(
    tool: &Path,
    args: &[OsString],
    stop: &AtomicBool,
    progress: Option<(&mpsc::Sender<Event>, f64)>,
) -> Result<Vec<u8>, String> {
    check_cancel(stop)?;
    let mut command = Command::new(tool);
    command
        .args(args)
        .env_remove("FFREPORT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(parent) = tool.parent() {
        command.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = Process(
        command
            .spawn()
            .map_err(|e| format!("Cannot start FFmpeg: {e}"))?,
    );
    let stdout = child.0.stdout.take().ok_or("Missing tool stdout")?;
    let stderr = child.0.stderr.take().ok_or("Missing tool stderr")?;
    let inspecting = progress.is_none();
    let progress = progress.map(|(tx, duration)| (tx.clone(), duration));
    let output = thread::spawn(move || {
        if let Some((tx, duration)) = progress {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(seconds) = line
                    .strip_prefix("out_time_us=")
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    let _ = tx.send(Event::Progress(
                        (seconds / 1_000_000. / duration).clamp(0., 0.99),
                    ));
                }
            }
            vec![]
        } else {
            drain(stdout, 4 * 1024 * 1024)
        }
    });
    let errors = thread::spawn(move || drain(stderr, 64 * 1024));
    let started = std::time::Instant::now();
    let status = loop {
        if inspecting && started.elapsed() >= Duration::from_secs(15) {
            let _ = child.0.kill();
            let _ = child.0.wait();
            break Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "FFmpeg capability check timed out",
            ));
        }
        if stop.load(Ordering::Acquire) {
            let _ = child.0.kill();
            break child.0.wait();
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.0.kill();
                let _ = child.0.wait();
                break Err(error);
            }
        }
    };
    let output = output.join().map_err(|_| "Export output reader failed")?;
    let errors = errors.join().map_err(|_| "Export error reader failed")?;
    check_cancel(stop)?;
    if !status.map_err(|e| e.to_string())?.success() {
        return Err(format!(
            "Export tool failed: {}",
            String::from_utf8_lossy(&errors)
        ));
    }
    Ok(output)
}

pub fn parse_timestamp(value: &str) -> Option<f64> {
    let parts = value.trim().split(':').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut result = 0.;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part
                .bytes()
                .all(|c| c.is_ascii_digit() || (i == parts.len() - 1 && c == b'.'))
        {
            return None;
        }
        let number: f64 = part.parse().ok()?;
        if !number.is_finite() || number < 0. || (i > 0 && number >= 60.) {
            return None;
        }
        result = result * 60. + number;
    }
    result.is_finite().then_some(result)
}

pub fn timestamp(seconds: f64) -> String {
    let millis = (seconds.max(0.) * 1000.).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_timestamp_validation() {
        assert_eq!(parse_timestamp("1:02.125"), Some(62.125));
        assert_eq!(parse_timestamp("0:01:02.125"), Some(62.125));
        assert_eq!(parse_timestamp("1.5"), Some(1.5));
        for bad in ["", "NaN", "inf", "-1", "1:60", "1.2:00", "1:2:3:4", "1..0"] {
            assert!(parse_timestamp(bad).is_none(), "{bad}");
        }
        assert_eq!(timestamp(62.125), "0:01:02.125");
    }

    #[test]
    fn preparation_job_drop_does_not_block_on_slow_transport() {
        let (release, wait) = mpsc::channel();
        let (finished, complete) = mpsc::channel();
        let job = Job::spawn(move |_, _| {
            let _ = wait.recv_timeout(Duration::from_secs(2));
            let _ = finished.send(());
        });
        let started = std::time::Instant::now();
        drop(job);
        assert!(started.elapsed() < Duration::from_millis(250));
        let _ = release.send(());
        complete.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn reject_existing_outputs_and_aliases_without_changing_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.mp4");
        fs::write(&source, b"original bytes").unwrap();
        let before = fs::metadata(&source).unwrap().modified().unwrap();
        let alias = dir.path().join("alias.mp4");
        fs::hard_link(&source, &alias).unwrap();
        assert!(destination(&source).is_err());
        assert!(destination(&alias).is_err());
        assert!(destination(&dir.path().join("SOURCE.MP4")).is_err());
        assert!(destination(&dir.path().join("new.mp4:stream")).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"original bytes");
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), before);
    }
    #[test]
    fn probe_indices_are_not_player_track_ids() {
        let media = parse_media(br#"{"duration":5.0,"video-dec-params":{"gamma":"bt.1886"},"track-list":[{"id":1,"ff-index":3,"type":"video","codec":"av1"},{"id":2,"ff-index":7,"type":"audio"},{"id":3,"ff-index":9,"type":"audio"}]}"#).unwrap();
        assert_eq!(media.video, Some(3));
        assert_eq!(media.video_codec.as_deref(), Some("av1"));
        assert_eq!(
            media.audio.iter().map(|a| a.index).collect::<Vec<_>>(),
            vec![7, 9]
        );
    }

    fn spec(source: &Path, target: &Path) -> Export {
        Export {
            source: source.to_owned(),
            destination: target.to_owned(),
            start: 0.35,
            end: 1.35,
            format: Format::Mp4,
            quality: Quality::Balanced,
            audio_mode: AudioMode::Merge,
            audio: vec![
                AudioSelection {
                    index: 1,
                    gain: 0.5,
                },
                AudioSelection { index: 2, gain: 1. },
            ],
        }
    }

    #[test]
    fn av1_input_decoder_precedes_input_and_does_not_replace_output_encoder() {
        let media = parse_media(br#"{"duration":5.0,"video-dec-params":{"gamma":"bt.1886"},"track-list":[{"ff-index":3,"type":"video","codec":"av1"},{"ff-index":7,"type":"audio"}]}"#).unwrap();
        let mut request = spec(Path::new("source.mp4"), Path::new("new.mp4"));
        let args = arguments(&request, &media, Path::new("media.partial"));
        let args = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let input = args.iter().position(|a| a == "-i").unwrap();
        let decoder = args.iter().position(|a| a == "-c:3").unwrap();
        assert!(decoder < input);
        assert_eq!(args[decoder + 1], "libdav1d");
        assert!(args.windows(2).any(|a| a == ["-ss", "0.350000000"]));
        assert!(args.windows(2).any(|a| a == ["-c:v", "libx264"]));
        assert!(args.contains(&"-xerror".to_owned()));
        request.format = Format::Mp3;
        let audio = arguments(&request, &media, Path::new("audio.partial"));
        assert!(!audio.iter().any(|a| a == "libdav1d"));
        let input = audio.iter().position(|a| a == "-i").unwrap();
        assert!(audio[..input].iter().any(|a| a == "-vn"));
        assert!(audio[input + 2..].iter().any(|a| a == "-vn"));
    }

    /// Change the declared level in a test-owned IVF. When the encoder computes
    /// a level below 4.0, insert the required tier bit using one padding bit.
    fn declare_av1_level_23(bytes: &mut [u8]) {
        assert_eq!(&bytes[..4], b"DKIF");
        let mut frame = 32;
        let mut changed = 0;
        while frame + 12 <= bytes.len() {
            let size = u32::from_le_bytes(bytes[frame..frame + 4].try_into().unwrap()) as usize;
            let end = frame + 12 + size;
            let mut offset = frame + 12;
            while offset < end {
                let header = bytes[offset];
                offset += 1;
                if header & 4 != 0 {
                    offset += 1;
                }
                assert_ne!(header & 2, 0, "IVF fixture uses sized AV1 OBUs");
                let mut length = 0;
                let mut shift = 0;
                loop {
                    let byte = bytes[offset];
                    offset += 1;
                    length |= usize::from(byte & 127) << shift;
                    if byte & 128 == 0 {
                        break;
                    }
                    shift += 7;
                    assert!(shift < 56);
                }
                if (header >> 3) & 15 == 1 {
                    let payload = &mut bytes[offset..offset + length];
                    assert_eq!(payload[0] & 0x1c, 0, "Regular sequence without timing info");
                    assert_eq!(
                        ((payload[0] & 1) << 4) | (payload[1] >> 4),
                        0,
                        "One operating point"
                    );
                    if payload[3] >> 3 <= 7 {
                        // Level > 7 introduces seq_tier immediately after its
                        // five level bits. Preserve all following syntax and
                        // the trailing one bit, dropping only zero padding.
                        assert_eq!(
                            payload.last().unwrap() & 1,
                            0,
                            "Fixture has spare OBU padding"
                        );
                        let mut carry = payload[3] & 1;
                        payload[3] = (23 << 3) | ((payload[3] & 7) >> 1);
                        for byte in &mut payload[4..] {
                            let next = *byte & 1;
                            *byte = (carry << 7) | (*byte >> 1);
                            carry = next;
                        }
                        assert_eq!(carry, 0);
                    } else {
                        payload[3] = (23 << 3) | (payload[3] & 7);
                    }
                    changed += 1;
                }
                offset += length;
                assert!(offset <= end);
            }
            frame = end;
        }
        assert!(changed > 0, "Fixture must contain a sequence header");
    }

    #[test]
    #[ignore = "requires optional FFmpeg; set STAR_PLAYER_TEST_FFMPEG and use --include-ignored"]
    fn av1_level_23_recording_exports_with_dav1d_and_preserves_original() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("fixture.avi");
        let source = dir.path().join("hardware-level.ivf");
        let target = dir.path().join("new.mp4");
        crate::playback::test_fixtures::avi_timed(&raw, 0, false, 64, 48, 10, 30);
        let stop = AtomicBool::new(false);
        let ffmpeg = crate::export_tools::find(&stop)
            .unwrap()
            .expect("Set STAR_PLAYER_TEST_FFMPEG to the tested ffmpeg.exe");
        let decoders = run(
            &ffmpeg,
            &["-hide_banner".into(), "-decoders".into()],
            &stop,
            None,
        )
        .unwrap();
        assert!(
            String::from_utf8_lossy(&decoders).contains("libdav1d"),
            "The export tools must include dav1d"
        );
        run(
            &ffmpeg,
            &[
                "-v".into(),
                "error".into(),
                "-nostdin".into(),
                "-n".into(),
                "-i".into(),
                raw.into_os_string(),
                "-c:v".into(),
                "libaom-av1".into(),
                "-cpu-used".into(),
                "8".into(),
                "-crf".into(),
                "20".into(),
                "-g".into(),
                "100".into(),
                "-aom-params".into(),
                "target-seq-level-idx=8".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                source.as_os_str().to_owned(),
            ],
            &stop,
            None,
        )
        .unwrap();
        let mut bytes = fs::read(&source).unwrap();
        declare_av1_level_23(&mut bytes);
        fs::write(&source, bytes).unwrap(); // Test-generated source, not a user file.
        let original_permissions = fs::metadata(&source).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let hash = Sha256::digest(fs::read(&source).unwrap());
        let old_decoder = run(
            &ffmpeg,
            &[
                "-v".into(),
                "error".into(),
                "-xerror".into(),
                "-c:v".into(),
                "libaom-av1".into(),
                "-i".into(),
                source.as_os_str().to_owned(),
                "-f".into(),
                "null".into(),
                "pipe:1".into(),
            ],
            &stop,
            None,
        );
        let old_error = old_decoder.expect_err("Reproduce libaom rejecting the declared level");
        assert!(
            old_error.contains("23") && old_error.contains("seq_level_idx"),
            "{old_error}"
        );
        let mut request = spec(&source, &target);
        request.start = 1.55;
        request.end = 2.55;
        request.audio.clear();
        let (tx, _) = mpsc::channel();
        export(&request, &stop, &tx).unwrap();
        let output = probe(&target, &stop).unwrap();
        assert!((output.duration - 1.).abs() <= 0.12);
        assert_eq!((output.width, output.height), (64, 48));
        assert_eq!(output.video_codec.as_deref(), Some("h264"));
        let frame = run(
            &ffmpeg,
            &[
                "-v".into(),
                "error".into(),
                "-xerror".into(),
                "-i".into(),
                target.into_os_string(),
                "-frames:v".into(),
                "1".into(),
                "-f".into(),
                "rawvideo".into(),
                "-pix_fmt".into(),
                "rgb24".into(),
                "pipe:1".into(),
            ],
            &stop,
            None,
        )
        .unwrap();
        assert!(
            (68..=88).contains(&frame[0]),
            "Trim starts at the selected AV1 frame: {:?}",
            &frame[..3]
        );
        assert_eq!(Sha256::digest(fs::read(&source).unwrap()), hash);
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
        assert!(fs::metadata(&source).unwrap().permissions().readonly());
        fs::set_permissions(&source, original_permissions).unwrap();
    }

    #[test]
    fn publication_never_replaces_a_destination_created_during_export() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("new.partial");
        let target = dir.path().join("existing.mp4");
        fs::write(&staging, b"new output").unwrap();
        fs::write(&target, b"keep me").unwrap();
        assert!(publish(&staging, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep me");
        assert_eq!(fs::read(&staging).unwrap(), b"new output");
    }

    #[test]
    #[ignore = "requires optional FFmpeg; set STAR_PLAYER_TEST_FFMPEG and use --include-ignored"]
    fn read_only_source_exports_precise_ranges_and_each_audio_mode() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Original ü & clip.avi");
        crate::playback::test_fixtures::avi_timed(&source, 2, true, 64, 48, 10, 30);
        let original_permissions = fs::metadata(&source).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let before = fs::metadata(&source).unwrap();
        let hash = Sha256::digest(fs::read(&source).unwrap());
        let stop = AtomicBool::new(false);
        let (tx, _) = mpsc::channel();
        for (i, (format, mode, audio_count)) in [
            (Format::Mp4, AudioMode::Merge, 1),
            (Format::Mkv, AudioMode::Separate, 2),
            (Format::Webm, AudioMode::None, 0),
            (Format::Mp3, AudioMode::Merge, 1),
        ]
        .into_iter()
        .enumerate()
        {
            let target = dir
                .path()
                .join(format!("New clip {i}.{}", format.extension()));
            let mut request = spec(&source, &target);
            request.format = format;
            request.audio_mode = mode;
            export(&request, &stop, &tx).unwrap();
            let media = probe(&target, &stop).unwrap();
            assert!((media.duration - 1.).abs() <= 0.12, "{}", media.duration);
            if format.audio_only() {
                assert_eq!(media.video, None);
                assert_eq!((media.width, media.height), (0, 0));
                let streams = crate::playback::inspect_media(&target, &stop).unwrap();
                assert_eq!(streams["track-list"].as_array().unwrap().len(), 1);
                assert_eq!(streams["track-list"][0]["codec"], "mp3");
                assert_eq!(streams["track-list"][0]["type"], "audio");
            } else {
                assert!(media.video.is_some());
                assert_eq!((media.width, media.height), (64, 48));
            }
            assert_eq!(media.audio.len(), audio_count);
            assert!(export(&request, &stop, &tx).is_err());
        }
        let mut invalid = spec(&source, &dir.path().join("invalid.mp4"));
        invalid.end = 50.;
        assert!(export(&invalid, &stop, &tx).is_err());
        assert!(!invalid.destination.exists());
        assert_eq!(Sha256::digest(fs::read(&source).unwrap()), hash);
        let after = fs::metadata(&source).unwrap();
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(
            after.permissions().readonly(),
            before.permissions().readonly()
        );
        assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".star-player-export-")
        }));
        // Only restore the test-owned fixture's permissions for TempDir cleanup.
        fs::set_permissions(&source, original_permissions).unwrap();
    }

    #[test]
    #[ignore = "requires optional FFmpeg; set STAR_PLAYER_TEST_FFMPEG and use --include-ignored"]
    fn precise_trim_does_not_include_frames_before_a_non_keyframe_start() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("fixture.avi");
        let source = dir.path().join("long-gop.mp4");
        crate::playback::test_fixtures::avi_timed(&raw, 0, false, 64, 48, 10, 30);
        let stop = AtomicBool::new(false);
        let ffmpeg = crate::export_tools::find(&stop)
            .unwrap()
            .expect("Set STAR_PLAYER_TEST_FFMPEG to the tested ffmpeg.exe");
        let args = vec![
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-n".into(),
            "-i".into(),
            raw.into_os_string(),
            "-c:v".into(),
            "libx264".into(),
            "-g".into(),
            "100".into(),
            "-sc_threshold".into(),
            "0".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            source.as_os_str().to_owned(),
        ];
        run(&ffmpeg, &args, &stop, None).unwrap();
        let target = dir.path().join("precise.mp4");
        let mut request = spec(&source, &target);
        request.start = 1.55;
        request.end = 2.55;
        request.audio.clear();
        let (tx, _) = mpsc::channel();
        export(&request, &stop, &tx).unwrap();
        let frame = run(
            &ffmpeg,
            &vec![
                "-v".into(),
                "error".into(),
                "-i".into(),
                target.as_os_str().to_owned(),
                "-frames:v".into(),
                "1".into(),
                "-f".into(),
                "rawvideo".into(),
                "-pix_fmt".into(),
                "rgb24".into(),
                "pipe:1".into(),
            ],
            &stop,
            None,
        )
        .unwrap();
        // Source red ramps from 30 at t=0 to 78 at t=1.6. A keyframe cut
        // would incorrectly begin near red=30 instead of the selected frame.
        assert!(
            frame[0] >= 70 && frame[0] <= 85,
            "first pixel: {:?}",
            &frame[..3]
        );
        let media = probe(&target, &stop).unwrap();
        assert!((media.duration - 1.).abs() <= 0.12);
    }

    #[test]
    #[ignore = "requires optional FFmpeg; set STAR_PLAYER_TEST_FFMPEG and use --include-ignored"]
    fn cancellation_kills_encoder_cleans_staging_and_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.avi");
        crate::playback::test_fixtures::avi_timed(&source, 0, true, 640, 480, 10, 30);
        let bytes = fs::read(&source).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let target = dir.path().join("cancelled.webm");
        let mut request = spec(&source, &target);
        request.format = Format::Webm;
        request.start = 0.;
        request.end = 3.;
        request.audio.clear();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (tx, rx) = mpsc::channel();
        let encoder = thread::spawn(move || export(&request, &stopping, &tx));
        // Wait for an actual encoder progress report, not just a queued job.
        let _ = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        stop.store(true, Ordering::Release);
        assert_eq!(encoder.join().unwrap().unwrap_err(), "Export cancelled");
        assert!(!target.exists());
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
        assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".star-player-export-")
        }));
    }

    #[test]
    #[ignore = "requires optional FFmpeg; set STAR_PLAYER_TEST_FFMPEG and use --include-ignored"]
    fn dropping_an_encoder_job_finishes_cleanup_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.avi");
        crate::playback::test_fixtures::avi_timed(&source, 0, true, 640, 480, 10, 30);
        let original = fs::read(&source).unwrap();
        let target = dir.path().join("cancelled.webm");
        let mut request = spec(&source, &target);
        request.format = Format::Webm;
        request.start = 0.;
        request.end = 3.;
        request.audio.clear();
        let job = Job::export(request);
        assert!(matches!(
            job.events.recv_timeout(Duration::from_secs(30)).unwrap(),
            Event::Progress(_)
        ));
        drop(job);
        assert!(!target.exists());
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(OpenOptions::new().write(true).open(&source).is_ok());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn source_handle_denies_writes_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.mp4");
        fs::write(&source, b"original").unwrap();
        let _guard = source_guard(&source).unwrap();
        assert!(OpenOptions::new().write(true).open(&source).is_err());
        assert!(fs::remove_file(&source).is_err());
        assert!(fs::rename(&source, dir.path().join("renamed.mp4")).is_err());
        assert!(File::open(&source).is_ok());
    }

    #[test]
    fn rejects_invalid_ranges_tracks_hdr_and_nonfinite_gains() {
        let media = Media {
            duration: 3.,
            video: Some(0),
            video_codec: None,
            audio: vec![AudioStream {
                index: 1,
                title: "Audio".into(),
            }],
            hdr: false,
            width: 64,
            height: 48,
        };
        let mut request = spec(Path::new("input.avi"), Path::new("output.mp4"));
        request.audio.truncate(1);
        assert!(validate(&request, &media).is_ok());
        for (start, end) in [
            (0., 0.),
            (-1., 1.),
            (2., 1.),
            (0., 4.),
            (f64::NAN, 1.),
            (0., f64::INFINITY),
        ] {
            let mut invalid = request.clone();
            invalid.start = start;
            invalid.end = end;
            assert!(validate(&invalid, &media).is_err());
        }
        for gain in [-1., 11., f64::NAN, f64::INFINITY] {
            let mut invalid = request.clone();
            invalid.audio[0].gain = gain;
            assert!(validate(&invalid, &media).is_err());
        }
        let mut invalid = request.clone();
        invalid.audio.push(invalid.audio[0].clone());
        assert!(validate(&invalid, &media).is_err());
        invalid.audio[0].index = 99;
        assert!(validate(&invalid, &media).is_err());
        let mut hdr = media;
        hdr.hdr = true;
        assert!(validate(&request, &hdr).is_err());
        request.format = Format::Mp3;
        assert!(
            validate(&request, &hdr).is_ok(),
            "Audio-only export does not convert HDR video"
        );
        request.audio_mode = AudioMode::Separate;
        assert!(
            validate(&request, &hdr).is_err(),
            "MP3 cannot retain separate audio streams"
        );
        request.audio_mode = AudioMode::Merge;
        request.audio.clear();
        assert!(validate(&request, &hdr).is_err());
    }

    #[test]
    fn audio_only_probe_accepts_audio_without_inventing_a_video_stream() {
        let media = parse_media(br#"{"duration":5.0,"track-list":[{"ff-index":0,"type":"audio"},{"ff-index":1,"type":"video","albumart":true}]}"#).unwrap();
        assert_eq!(media.video, None);
        assert_eq!(media.audio.len(), 1);
        assert_eq!((media.width, media.height), (0, 0));
    }

    #[test]
    fn inspection_uses_bundled_mpv_without_ffmpeg_or_ffprobe() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Read only ü, original.avi");
        crate::playback::test_fixtures::avi_timed(&source, 2, true, 64, 48, 10, 30);
        let original = fs::read(&source).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let guard = source_guard(&source).unwrap();
        let media = probe(&source, &AtomicBool::new(false)).unwrap();
        assert!((media.duration - 3.).abs() < 0.05);
        assert_eq!((media.width, media.height), (64, 48));
        assert_eq!(
            media.audio.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(media.video, Some(0));
        assert!(!media.hdr);
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
        drop(guard);
        assert!(probe(&source, &AtomicBool::new(true)).is_err());
    }

    #[test]
    fn missing_mapping_or_color_information_fails_closed() {
        assert!(
            parse_media(br#"{"duration":5.0,"track-list":[{"type":"audio","id":1}]}"#).is_err()
        );
        assert!(
            parse_media(br#"{"duration":5.0,"track-list":[{"type":"video","ff-index":0}]}"#)
                .is_err()
        );
        for gamma in ["pq", "hlg"] {
            let value = serde_json::json!({"duration":5.,"track-list":[{"type":"video","ff-index":0}],"video-dec-params":{"gamma":gamma}});
            assert!(
                parse_media(&serde_json::to_vec(&value).unwrap())
                    .unwrap()
                    .hdr
            );
        }
    }

    #[test]
    fn audio_only_inspection_needs_no_video_output_or_external_tools() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("audio.wav");
        let data_size = 48000u32 * 2;
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + data_size).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(48000u32.to_le_bytes());
        wav.extend(96000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_size.to_le_bytes());
        wav.resize(wav.len() + data_size as usize, 0);
        fs::write(&source, &wav).unwrap();
        let media = probe(&source, &AtomicBool::new(false)).unwrap();
        assert_eq!(media.video, None);
        assert_eq!(media.audio.len(), 1);
        assert_eq!(media.audio[0].index, 0);
        assert!((media.duration - 1.).abs() < 0.01);
        assert_eq!(fs::read(source).unwrap(), wav);
    }
}
