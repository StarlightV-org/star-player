//! Filesystem and decoder work stays off the UI thread.
use crate::{
    clip_store::{self, Config, Discovery, History},
    playback::PreviewDecoder,
};
use notify::{RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stamp {
    pub size: u64,
    pub modified: SystemTime,
}
pub fn stamp(path: &Path) -> Result<Stamp, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    Ok(Stamp {
        size: meta.len(),
        modified: meta.modified().map_err(|e| e.to_string())?,
    })
}
#[derive(Clone, Debug)]
pub struct Clip {
    pub path: PathBuf,
    pub created: SystemTime,
    pub stamp: Stamp,
}
#[derive(Clone, Default)]
pub struct Catalog {
    pub folders: Vec<PathBuf>,
    pub clips: Vec<Clip>,
}

pub fn is_video(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "mp4"
                | "m4v"
                | "mkv"
                | "mov"
                | "avi"
                | "webm"
                | "ts"
                | "mts"
                | "m2ts"
                | "wmv"
                | "flv"
                | "mpeg"
                | "mpg"
        )
    })
}

pub fn scan(roots: &[PathBuf]) -> (Catalog, Vec<String>) {
    let mut catalog = Catalog::default();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = roots.to_vec();
    while let Some(folder) = pending.pop() {
        let canonical = match fs::canonicalize(&folder) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("{}: {e}", folder.display()));
                continue;
            }
        };
        if !seen.insert(canonical) {
            continue;
        }
        catalog.folders.push(folder.clone());
        let entries = match fs::read_dir(&folder) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{}: {e}", folder.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    errors.push(e.to_string());
                    continue;
                }
            };
            let path = entry.path();
            let kind = match entry.file_type() {
                Ok(k) => k,
                Err(e) => {
                    errors.push(e.to_string());
                    continue;
                }
            };
            // Do not follow links/junctions into unrelated roots or directory cycles.
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && is_video(&path)
                && let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
            {
                catalog.clips.push(Clip {
                    path,
                    created: meta.created().unwrap_or(modified),
                    stamp: Stamp {
                        size: meta.len(),
                        modified,
                    },
                });
            }
        }
    }
    catalog.folders.sort();
    catalog
        .clips
        .sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.path.cmp(&b.path)));
    (catalog, errors)
}

pub enum Request {
    Consent(bool),
    SteamConsent(bool),
    AddFolder(PathBuf),
    RemoveFolder(PathBuf),
    Detect,
    Sort(String),
    RememberPlayback(clip_store::RememberPlayback),
    Updates(clip_store::UpdateConfig),
    Refresh,
    Thumbnail(PathBuf, Stamp),
    Watched(Vec<PathBuf>, bool, String),
    Delete(Vec<PathBuf>, Arc<AtomicBool>),
    Audio(PathBuf, BTreeMap<i64, f64>),
    FileSettings(PathBuf, crate::chapters::FileSettings),
    Resume(PathBuf, Option<f64>),
    Master(f64, bool),
    Rename(PathBuf, String, Arc<AtomicBool>),
    Shutdown,
}
pub enum Event {
    Config(Config),
    SteamConsent(Result<bool, String>),
    Catalog(Catalog),
    History(History),
    Thumbnail(PathBuf, Stamp, Option<image::RgbaImage>, Option<f64>),
    WatchedComplete(Vec<PathBuf>),
    Error(String),
    Renamed(PathBuf, PathBuf),
    RenameFailed(PathBuf, String),
    FileSettingsFailed(PathBuf, crate::chapters::FileSettings, String),
}

pub struct LibraryWorker {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl LibraryWorker {
    pub fn new() -> Self {
        Self::with_directory(None)
    }
    #[cfg(test)]
    pub fn for_test(directory: PathBuf) -> Self {
        Self::with_directory(Some(directory))
    }
    fn with_directory(directory: Option<PathBuf>) -> Self {
        let (tx, requests) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            let result = directory
                .map(Ok)
                .unwrap_or_else(clip_store::directory)
                .and_then(|dir| run(&dir, requests, events.clone(), stopping));
            if let Err(error) = result {
                let _ = events.send(Event::Error(error));
            }
        });
        Self {
            tx,
            rx,
            stop,
            worker: Some(worker),
        }
    }
    pub fn send(&self, request: Request) {
        let _ = self.tx.send(request);
    }
    pub fn mark_watched(&self, paths: Vec<PathBuf>, watched: bool) {
        self.send(Request::Watched(
            paths,
            watched,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ));
    }
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.rx.try_iter()
    }
}
impl Drop for LibraryWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.stop.store(true, Ordering::Release);
    }
}

fn run(
    dir: &Path,
    requests: mpsc::Receiver<Request>,
    events: mpsc::Sender<Event>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let config_result = clip_store::read_config(dir);
    let config_valid = config_result.is_ok();
    let mut config = config_result.unwrap_or_else(|e| {
        let _ = events.send(Event::Error(e));
        Config::default()
    });
    let history_result = clip_store::read_history(dir);
    let history_valid = history_result.is_ok();
    let mut history = history_result.unwrap_or_else(|e| {
        let _ = events.send(Event::Error(e));
        History::new()
    });
    if config_valid && config.clip_discovery == clip_store::Discovery::Enabled {
        let mut next = config.clone();
        if clip_store::detect_nvidia(&mut next).is_ok()
            && next.clip_directories != config.clip_directories
        {
            match clip_store::save_config(dir, &next) {
                Ok(()) => config = next,
                Err(error) => {
                    let _ = events.send(Event::Error(error));
                }
            }
        }
    }
    let _ = events.send(Event::Config(config.clone()));
    let _ = events.send(Event::History(history.clone()));
    let (watch_tx, watch_rx) = mpsc::channel();
    let watcher_result =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if !matches!(&event, Ok(e) if matches!(e.kind, notify::EventKind::Access(_))) {
                let _ = watch_tx.send(event);
            }
        });
    let mut watcher = match watcher_result {
        Ok(watcher) => Some(watcher),
        Err(e) => {
            let _ = events.send(Event::Error(format!(
                "Folder watcher unavailable: {e}. Using periodic scans."
            )));
            None
        }
    };
    let mut watched_roots = HashSet::new();
    let mut dirty = Some(Instant::now() - Duration::from_secs(1));
    let mut last_scan = Instant::now();
    let mut tasks = VecDeque::new();
    let mut watched_tasks = VecDeque::new();
    let mut decoder = None::<PreviewDecoder>;
    let mut catalog = Catalog::default();
    let mut closing = false;
    if let Err(e) = crate::thumbnail_cache::prune(&dir.join("thumbnails")) {
        let _ = events.send(Event::Error(e));
    }
    while !stop.load(Ordering::Acquire) {
        for request in requests.try_iter() {
            match request {
                Request::Shutdown => {
                    closing = true;
                    tasks.clear();
                }
                Request::Resume(path, position) => {
                    let result = (|| {
                        if !history_valid {
                            return Err(
                                "Repair media history before saving playback positions".into()
                            );
                        }
                        clip_store::validate_resume_position(position)?;
                        let mut next = history.clone();
                        let mut state = clip_store::media_state(&next, &path);
                        state.resume_position = position;
                        next.insert(clip_store::hash_filename(&path)?, state);
                        clip_store::save_history(dir, &next)?;
                        history = next;
                        Ok::<_, String>(())
                    })();
                    match result {
                        Ok(()) => {
                            let _ = events.send(Event::History(history.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::FileSettings(path, settings) => {
                    let attempted = settings.clone();
                    let result = (|| {
                        if !history_valid {
                            return Err("Repair media history before saving file settings".into());
                        }
                        settings.validate()?;
                        let mut next = history.clone();
                        let mut state = clip_store::media_state(&next, &path);
                        state.file_settings = settings;
                        next.insert(clip_store::hash_filename(&path)?, state);
                        clip_store::save_history(dir, &next)?;
                        history = next;
                        Ok::<_, String>(())
                    })();
                    match result {
                        Ok(()) => {
                            let _ = events.send(Event::History(history.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::FileSettingsFailed(path, attempted, e));
                        }
                    }
                }
                Request::Audio(path, gains) => {
                    let result = (|| {
                        if !history_valid {
                            return Err("Repair media history before saving audio settings".into());
                        }
                        clip_store::validate_gains(&gains)?;
                        let mut next = history.clone();
                        let mut state = clip_store::media_state(&next, &path);
                        state.track_gains = gains;
                        next.insert(clip_store::hash_filename(&path)?, state);
                        clip_store::save_history(dir, &next)?;
                        history = next;
                        Ok::<_, String>(())
                    })();
                    match result {
                        Ok(()) => {
                            let _ = events.send(Event::History(history.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::Master(volume, muted) => {
                    let mut next = config.clone();
                    next.volume = volume;
                    next.muted = muted;
                    let result =
                        if config_valid && volume.is_finite() && (0.0..=100.0).contains(&volume) {
                            clip_store::save_config(dir, &next)
                        } else {
                            Err("Cannot save invalid master volume or config.json".into())
                        };
                    match result {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::Rename(path, stem, released) => {
                    decoder = None;
                    tasks.retain(|task| !matches!(task, Request::Thumbnail(p, _) if p == &path));
                    let deadline = Instant::now() + Duration::from_secs(6);
                    while !released.load(Ordering::Acquire) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(10));
                    }
                    let result = if !released.load(Ordering::Acquire) {
                        Err(RenameFailure::from(
                            "Preview decoder is still releasing the file. Retry rename.",
                        ))
                    } else if !history_valid {
                        Err(RenameFailure::from(
                            "Repair media history before renaming files",
                        ))
                    } else {
                        rename_clip(dir, &path, &stem, &catalog, &mut history)
                    };
                    match result {
                        Ok(new) => {
                            for task in &mut watched_tasks {
                                if let Request::Watched(paths, _, _) = task {
                                    for p in paths {
                                        if *p == path {
                                            *p = new.clone();
                                        }
                                    }
                                }
                            }
                            let _ = events.send(Event::History(history.clone()));
                            let _ = events.send(Event::Renamed(path, new));
                            dirty = Some(Instant::now() - Duration::from_secs(1));
                        }
                        Err(e) => {
                            if let Some(new) = e.current_path {
                                for task in &mut watched_tasks {
                                    if let Request::Watched(paths, _, _) = task {
                                        for p in paths {
                                            if *p == path {
                                                *p = new.clone();
                                            }
                                        }
                                    }
                                }
                                let _ = events.send(Event::History(history.clone()));
                                let _ = events.send(Event::Renamed(path, new));
                                let _ = events.send(Event::Error(e.message));
                                dirty = Some(Instant::now() - Duration::from_secs(1));
                            } else {
                                let _ = events.send(Event::RenameFailed(path, e.message));
                            }
                        }
                    }
                }
                Request::SteamConsent(enabled) => {
                    if !config_valid {
                        let _ = events.send(Event::SteamConsent(Err(
                            "Repair config.json before saving Steam permissions".into(),
                        )));
                        continue;
                    }
                    let mut next = config.clone();
                    next.steam_discovery = if enabled {
                        Discovery::Enabled
                    } else {
                        Discovery::Declined
                    };
                    match clip_store::save_config(dir, &next) {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                            let _ = events.send(Event::SteamConsent(Ok(enabled)));
                        }
                        Err(e) => {
                            let _ = events.send(Event::SteamConsent(Err(e)));
                        }
                    }
                }
                Request::Consent(enabled) => {
                    if !config_valid {
                        let _ = events.send(Event::Error(
                            "Repair config.json before saving preferences".into(),
                        ));
                        continue;
                    }
                    let mut next = config.clone();
                    next.clip_discovery = if enabled {
                        Discovery::Enabled
                    } else {
                        Discovery::Declined
                    };
                    let detection = if enabled {
                        clip_store::detect_nvidia(&mut next)
                    } else {
                        Ok(())
                    };
                    match clip_store::save_config(dir, &next) {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                            dirty = Some(Instant::now() - Duration::from_secs(1));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                    if let Err(e) = detection {
                        let _ = events.send(Event::Error(e));
                    }
                }
                Request::AddFolder(_) | Request::RemoveFolder(_) | Request::Detect => {
                    let mut next = config.clone();
                    let result = if config_valid {
                        match request {
                            Request::AddFolder(path) => clip_store::add_directory(&mut next, path),
                            Request::RemoveFolder(path) => {
                                next.clip_directories.retain(|root| root != &path);
                                Ok(())
                            }
                            Request::Detect => clip_store::detect_nvidia(&mut next),
                            _ => unreachable!(),
                        }
                        .and_then(|_| clip_store::save_config(dir, &next))
                    } else {
                        Err("Repair config.json before saving preferences".into())
                    };
                    match result {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                            dirty = Some(Instant::now() - Duration::from_secs(1));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::RememberPlayback(value) => {
                    let mut next = config.clone();
                    next.player.remember_playback = value;
                    let result = if config_valid {
                        clip_store::save_config(dir, &next)
                    } else {
                        Err("Repair config.json before saving preferences".into())
                    };
                    match result {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::Updates(value) => {
                    let mut next = config.clone();
                    next.updates = value;
                    let result = if config_valid {
                        clip_store::save_config(dir, &next)
                    } else {
                        Err("Repair config.json before saving preferences".into())
                    };
                    match result {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::Sort(sort) => {
                    let mut next = config.clone();
                    next.game_folder_sort = sort;
                    let result = if config_valid {
                        clip_store::save_config(dir, &next)
                    } else {
                        Err("Repair config.json before saving preferences".into())
                    };
                    match result {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
                Request::Refresh => dirty = Some(Instant::now() - Duration::from_secs(1)),
                Request::Thumbnail(path, stamp) => {
                    tasks.push_front(Request::Thumbnail(path, stamp))
                }
                Request::Watched(paths, value, timestamp) => {
                    watched_tasks.push_back(Request::Watched(paths, value, timestamp))
                }
                Request::Delete(paths, released) => {
                    // Playback is released by the UI before this request is sent.
                    decoder = None;
                    let deadline = Instant::now() + Duration::from_secs(6);
                    while !released.load(Ordering::Acquire)
                        && Instant::now() < deadline
                        && !stop.load(Ordering::Acquire)
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    if !released.load(Ordering::Acquire) {
                        let _ = events.send(Event::Error(
                            "Preview decoder is still releasing the file. Please retry deletion."
                                .into(),
                        ));
                        continue;
                    }
                    for path in paths {
                        let Some(clip) = catalog.clips.iter().find(|c| c.path == path) else {
                            continue;
                        };
                        if stamp(&path).ok().as_ref() != Some(&clip.stamp) {
                            let _ = events.send(Event::Error(format!(
                                "{} changed since selection; refresh before deleting",
                                path.display()
                            )));
                            continue;
                        }
                        if let Err(e) = trash::delete(&path) {
                            let _ = events.send(Event::Error(format!(
                                "Could not recycle {}: {e}",
                                path.display()
                            )));
                        }
                    }
                    dirty = Some(Instant::now() - Duration::from_secs(1));
                }
            }
        }
        for event in watch_rx.try_iter() {
            if let Err(e) = event {
                let _ = events.send(Event::Error(format!("Folder watcher: {e}")));
            }
            dirty.get_or_insert_with(Instant::now);
        }
        // Periodic reconciliation also recovers missing/reconnected roots and missed events.
        if last_scan.elapsed() >= Duration::from_secs(30) {
            if config_valid && config.clip_discovery == clip_store::Discovery::Enabled {
                let mut next = config.clone();
                if clip_store::detect_nvidia(&mut next).is_ok()
                    && next.clip_directories != config.clip_directories
                {
                    match clip_store::save_config(dir, &next) {
                        Ok(()) => {
                            config = next;
                            let _ = events.send(Event::Config(config.clone()));
                        }
                        Err(error) => {
                            let _ = events.send(Event::Error(error));
                        }
                    }
                }
            }
            dirty.get_or_insert(Instant::now() - Duration::from_secs(1));
        }
        if dirty.is_some_and(|at| at.elapsed() >= Duration::from_millis(400)) {
            watched_roots.retain(|p: &PathBuf| {
                if p.is_dir() && config.clip_directories.contains(p) {
                    true
                } else {
                    if let Some(watcher) = &mut watcher {
                        let _ = watcher.unwatch(p);
                    }
                    false
                }
            });
            for root in &config.clip_directories {
                if !watched_roots.contains(root) && root.is_dir() {
                    let Some(watcher) = &mut watcher else {
                        break;
                    };
                    match watcher.watch(root, RecursiveMode::Recursive) {
                        Ok(()) => {
                            watched_roots.insert(root.clone());
                        }
                        Err(e) => {
                            let _ =
                                events.send(Event::Error(format!("Watch {}: {e}", root.display())));
                        }
                    }
                }
            }
            let (next, errors) = scan(&config.clip_directories);
            catalog = next;
            if history_valid {
                let mut migrated = history.clone();
                for clip in &catalog.clips {
                    if let Ok(hash) = clip_store::hash_filename(&clip.path) {
                        let state = clip_store::media_state(&history, &clip.path);
                        if !migrated.contains_key(&hash) && !state.watched_at.is_empty() {
                            migrated.insert(hash, state);
                        }
                    }
                }
                if migrated != history {
                    match clip_store::save_history(dir, &migrated) {
                        Ok(()) => {
                            history = migrated;
                            let _ = events.send(Event::History(history.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
            }
            let _ = events.send(Event::Catalog(catalog.clone()));
            if !errors.is_empty() {
                let _ = events.send(Event::Error(errors.join("\n")));
            }
            dirty = None;
            last_scan = Instant::now();
        }
        if let Some(task) = watched_tasks.pop_front().or_else(|| tasks.pop_front()) {
            match task {
                Request::Watched(paths, value, timestamp) => {
                    if !history_valid {
                        let _ = events.send(Event::Error(
                            "Repair media history before saving watched status".into(),
                        ));
                        let _ = events.send(Event::WatchedComplete(paths));
                        continue;
                    }
                    let mut next = history.clone();
                    for path in &paths {
                        let result = clip_store::hash_filename(path);
                        match result {
                            Ok(hash) => {
                                if value {
                                    let mut state = clip_store::media_state(&next, path);
                                    state.watched_at = timestamp.clone();
                                    next.insert(hash.clone(), state);
                                } else {
                                    let mut state = clip_store::media_state(&next, path);
                                    state.watched_at.clear();
                                    next.insert(hash, state);
                                }
                            }
                            Err(e) => {
                                let _ = events.send(Event::Error(format!(
                                    "Watched status: {}: {e}",
                                    path.display()
                                )));
                            }
                        }
                    }
                    match clip_store::save_history(dir, &next) {
                        Ok(()) => {
                            history = next;
                            let _ = events.send(Event::History(history.clone()));
                        }
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                    let _ = events.send(Event::WatchedComplete(paths));
                }
                Request::Thumbnail(path, expected) => {
                    if stamp(&path).ok().as_ref() != Some(&expected) {
                        let _ = events.send(Event::Thumbnail(path, expected, None, None));
                        continue;
                    }
                    let key = format!(
                        "{:x}",
                        Sha256::digest(format!("{}:{expected:?}", path.display()).as_bytes())
                    );
                    let cache = dir.join("thumbnails").join(format!("{key}.jpg"));
                    let duration_cache = cache.with_extension("duration");
                    let mut duration = fs::read(&duration_cache)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<f64>(&bytes).ok())
                        .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
                    let mut image = image::open(&cache).ok().map(|i| i.into_rgba8());
                    if image.is_none() || duration.is_none() {
                        if decoder.is_none() {
                            decoder = PreviewDecoder::new().ok();
                        }
                        let result = decoder
                            .as_mut()
                            .ok_or_else(|| "Preview decoder unavailable".to_owned())
                            .and_then(|decoder| {
                                decoder.decode(
                                    &path,
                                    1.0,
                                    Instant::now() + Duration::from_secs(5),
                                    || stop.load(Ordering::Acquire),
                                )
                            });
                        match result {
                            Ok(decoded) => {
                                if stamp(&path).ok().as_ref() != Some(&expected) {
                                    let _ =
                                        events.send(Event::Thumbnail(path, expected, None, None));
                                    continue;
                                }
                                duration = decoder.as_ref().and_then(PreviewDecoder::duration);
                                if image.is_none() {
                                    if let Err(e) = crate::thumbnail_cache::save(&cache, &decoded) {
                                        let _ = events
                                            .send(Event::Error(format!("Thumbnail cache: {e}")));
                                    }
                                    image = Some(decoded);
                                }
                                if let Some(seconds) = duration
                                    && cache.is_file()
                                    && let Err(e) = clip_store::atomic_write(
                                        &duration_cache,
                                        seconds.to_string().as_bytes(),
                                    )
                                {
                                    let _ = events
                                        .send(Event::Error(format!("Clip duration cache: {e}")));
                                }
                            }
                            Err(_) => {
                                decoder = None;
                            }
                        }
                    }
                    let _ = events.send(Event::Thumbnail(path, expected, image, duration));
                }
                _ => {}
            }
        } else if closing {
            break;
        } else {
            thread::sleep(Duration::from_millis(30));
        }
    }
    Ok(())
}

pub fn rename_destination(path: &Path, stem: &str) -> Result<PathBuf, String> {
    if stem.is_empty()
        || stem.trim() != stem
        || stem.ends_with('.')
        || stem
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err("Enter a valid Windows filename without an extension".into());
    }
    let base = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        return Err("That filename is reserved by Windows".into());
    }
    let mut filename = std::ffi::OsString::from(stem);
    if let Some(extension) = path.extension() {
        filename.push(".");
        filename.push(extension);
    }
    Ok(path.with_file_name(filename))
}

/// MoveFileW refuses an existing destination, including a race after validation.
fn rename_no_replace(old: &Path, new: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileW(old: *const u16, new: *const u16) -> i32;
        }
        let old: Vec<u16> = old.as_os_str().encode_wide().chain(Some(0)).collect();
        let new: Vec<u16> = new.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileW(old.as_ptr(), new.as_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(old, new).map_err(|e| e.to_string())?;
        if let Err(e) = fs::remove_file(old) {
            let _ = fs::remove_file(new);
            return Err(e.to_string());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RenameFailure {
    message: String,
    current_path: Option<PathBuf>,
}
impl From<String> for RenameFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            current_path: None,
        }
    }
}
impl From<&str> for RenameFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn rename_clip(
    dir: &Path,
    old: &Path,
    stem: &str,
    catalog: &Catalog,
    history: &mut History,
) -> Result<PathBuf, RenameFailure> {
    let new = rename_destination(old, stem)?;
    if old == new {
        return Ok(new);
    }
    if let Some(clip) = catalog.clips.iter().find(|c| c.path == old) {
        if stamp(old)? != clip.stamp {
            return Err("File changed since selection; refresh before renaming".into());
        }
    }
    let old_hash = clip_store::hash_filename(old)?;
    let new_hash = clip_store::hash_filename(&new)?;
    let state = clip_store::media_state(history, old);
    let mut next = history.clone();
    // Do not silently overwrite another file's settings under a shared identity.
    if next
        .get(&new_hash)
        .is_some_and(|existing| existing != &state)
    {
        return Err("The destination identity already has different saved settings".into());
    }
    next.insert(new_hash, state);
    if !catalog.clips.iter().any(|c| {
        c.path != old && clip_store::hash_filename(&c.path).ok().as_ref() == Some(&old_hash)
    }) {
        next.remove(&old_hash);
    }
    rename_no_replace(old, &new)?;
    if let Err(error) = clip_store::save_history(dir, &next) {
        return match rename_no_replace(&new, old) {
            Ok(()) => Err(format!("Rename rolled back: {error}").into()),
            Err(rollback) => {
                *history = next;
                Err(RenameFailure {
                    message: format!(
                        "File is now {}; settings could not be saved: {error}. Rollback failed: {rollback}",
                        new.display()
                    ),
                    current_path: Some(new),
                })
            }
        };
    }
    *history = next;
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_settings_worker_preserves_audio_and_resume_and_rejects_broken_database() {
        let store = tempfile::tempdir().unwrap();
        let path = store.path().join("clip.mp4");
        let mut state = clip_store::MediaState {
            identity_version: 2,
            resume_position: Some(42.0),
            track_gains: BTreeMap::from([(1, 0.4)]),
            ..Default::default()
        };
        let key = clip_store::hash_filename(&path).unwrap();
        clip_store::save_history(store.path(), &History::from([(key.clone(), state.clone())]))
            .unwrap();
        let settings = crate::chapters::FileSettings {
            chapters: crate::chapters::import("0:10 Chapter", 60.0, &[]).points,
            embedded_disabled: true,
            ..Default::default()
        };
        let worker = LibraryWorker::for_test(store.path().to_owned());
        worker.send(Request::FileSettings(path.clone(), settings.clone()));
        loop {
            match worker.rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::History(history)
                    if history
                        .get(&key)
                        .is_some_and(|s| s.file_settings == settings) =>
                {
                    break;
                }
                Event::FileSettingsFailed(_, _, e) | Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        drop(worker);
        state.file_settings = settings.clone();
        assert_eq!(clip_store::read_history(store.path()).unwrap()[&key], state);
        fs::write(store.path().join("library.db"), b"broken").unwrap();
        let worker = LibraryWorker::for_test(store.path().to_owned());
        worker.send(Request::FileSettings(path.clone(), settings.clone()));
        loop {
            match worker.rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::FileSettingsFailed(p, attempted, _) => {
                    assert_eq!(p, path);
                    assert_eq!(attempted, settings);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(
            fs::read(store.path().join("library.db")).unwrap(),
            b"broken"
        );
    }
    #[test]
    fn steam_permission_ack_follows_saved_config_and_rejects_invalid_config() {
        let store = tempfile::tempdir().unwrap();
        let worker = LibraryWorker::for_test(store.path().to_owned());
        worker.send(Request::SteamConsent(true));
        let mut published = false;
        loop {
            match worker.rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Config(config) if config.steam_discovery == Discovery::Enabled => {
                    published = true
                }
                Event::SteamConsent(Ok(true)) => {
                    assert!(published);
                    assert_eq!(
                        clip_store::read_config(store.path())
                            .unwrap()
                            .steam_discovery,
                        Discovery::Enabled
                    );
                    break;
                }
                Event::Error(e) | Event::SteamConsent(Err(e)) => panic!("{e}"),
                _ => {}
            }
        }
        drop(worker);
        fs::write(store.path().join("config.json"), b"broken").unwrap();
        let worker = LibraryWorker::for_test(store.path().to_owned());
        worker.send(Request::SteamConsent(true));
        loop {
            match worker.rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::SteamConsent(Err(_)) => break,
                Event::SteamConsent(Ok(_)) => {
                    panic!("Invalid configuration must not grant permission")
                }
                _ => {}
            }
        }
        assert_eq!(
            fs::read(store.path().join("config.json")).unwrap(),
            b"broken"
        );
    }
    #[test]
    fn update_preferences_persist_without_resetting_other_settings() {
        let store = tempfile::tempdir().unwrap();
        let config = Config {
            volume: 42.,
            ..Config::default()
        };
        clip_store::save_config(store.path(), &config).unwrap();
        let worker = LibraryWorker::for_test(store.path().to_owned());
        worker.send(Request::Updates(clip_store::UpdateConfig {
            automatic_check: false,
            automatic_download: false,
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match worker
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                Event::Config(config)
                    if !config.updates.automatic_check && !config.updates.automatic_download =>
                {
                    break;
                }
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        drop(worker);
        let saved = clip_store::read_config(store.path()).unwrap();
        assert!(!saved.updates.automatic_check && !saved.updates.automatic_download);
        assert_eq!(saved.volume, 42.);
    }

    #[test]
    fn settings_disable_discovery_and_remove_roots_without_deleting_files() {
        let store = tempfile::tempdir().unwrap();
        let clips = tempfile::tempdir().unwrap();
        let video = clips.path().join("clip.mp4");
        fs::write(&video, b"keep this file").unwrap();
        let mut config = Config::default();
        clip_store::add_directory(&mut config, clips.path().to_owned()).unwrap();
        clip_store::save_config(store.path(), &config).unwrap();
        let worker = LibraryWorker::for_test(store.path().to_owned());
        let wait_config = |predicate: &dyn Fn(&Config) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                for event in worker.events() {
                    match event {
                        Event::Config(config) if predicate(&config) => return config,
                        Event::Error(error) => panic!("{error}"),
                        _ => {}
                    }
                }
                assert!(Instant::now() < deadline, "Settings update timed out");
                thread::sleep(Duration::from_millis(10));
            }
        };
        worker.send(Request::Consent(false));
        let disabled =
            wait_config(&|config| config.clip_discovery == clip_store::Discovery::Declined);
        assert_eq!(disabled.clip_directories, config.clip_directories);
        worker.send(Request::RemoveFolder(config.clip_directories[0].clone()));
        wait_config(&|config| config.clip_directories.is_empty());
        assert_eq!(fs::read(video).unwrap(), b"keep this file");
        let saved = clip_store::read_config(store.path()).unwrap();
        assert_eq!(saved.clip_discovery, clip_store::Discovery::Declined);
        assert!(saved.clip_directories.is_empty());
        worker.send(Request::SteamConsent(true));
        let allowed = wait_config(&|config| config.steam_discovery == Discovery::Enabled);
        assert_eq!(allowed.clip_discovery, Discovery::Declined);
        assert!(allowed.clip_directories.is_empty());
        assert!(!store.path().join("game-icons").exists());
        worker.send(Request::SteamConsent(false));
        wait_config(&|config| config.steam_discovery == Discovery::Declined);
        assert_eq!(
            clip_store::read_config(store.path())
                .unwrap()
                .steam_discovery,
            Discovery::Declined
        );
    }

    #[test]
    fn rename_preserves_settings_and_refuses_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.mp4");
        fs::write(&old, b"video").unwrap();
        let state = clip_store::MediaState {
            identity_version: 2,
            watched_at: "2026-10-14T18:42:00Z".into(),
            track_gains: BTreeMap::from([(1, 0.25), (2, 0.0)]),
            resume_position: Some(345.75),
            file_settings: crate::chapters::FileSettings {
                chapters: crate::chapters::import("0:10 Intro", 100.0, &[]).points,
                embedded_disabled: true,
                ..Default::default()
            },
        };
        let old_hash = clip_store::hash_filename(&old).unwrap();
        let mut history = History::from([(old_hash.clone(), state.clone())]);
        let (catalog, _) = scan(&[dir.path().to_owned()]);
        let new = rename_clip(dir.path(), &old, "new", &catalog, &mut history).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read(&new).unwrap(), b"video");
        assert_eq!(clip_store::media_state(&history, &new), state);
        assert!(!history.contains_key(&old_hash));
        assert_eq!(clip_store::read_history(dir.path()).unwrap(), history);
        let existing = dir.path().join("existing.mp4");
        fs::write(&existing, b"do not replace").unwrap();
        assert!(
            rename_clip(
                dir.path(),
                &new,
                "existing",
                &Catalog::default(),
                &mut history
            )
            .is_err()
        );
        assert_eq!(fs::read(&existing).unwrap(), b"do not replace");
        assert!(new.exists());
    }
    #[test]
    fn rename_rolls_back_if_history_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("watched.csv")).unwrap();
        let old = dir.path().join("old.mp4");
        fs::write(&old, b"video").unwrap();
        let mut history = History::new();
        assert!(rename_clip(dir.path(), &old, "new", &Catalog::default(), &mut history).is_err());
        assert!(old.exists());
        assert!(!dir.path().join("new.mp4").exists());
        assert!(history.is_empty());
    }
    #[test]
    fn rename_retains_a_shared_old_identity() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("a/Game/clip.mp4");
        let shared = root.path().join("b/Game/clip.mp4");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(&old, b"a").unwrap();
        fs::write(&shared, b"b").unwrap();
        let state = clip_store::MediaState {
            identity_version: 2,
            watched_at: "2026-10-14T18:42:00Z".into(),
            ..Default::default()
        };
        let mut history =
            History::from([(clip_store::hash_filename(&old).unwrap(), state.clone())]);
        let (catalog, _) = scan(&[root.path().to_owned()]);
        let new = rename_clip(root.path(), &old, "renamed", &catalog, &mut history).unwrap();
        assert_eq!(clip_store::media_state(&history, &shared), state);
        assert_eq!(clip_store::media_state(&history, &new), state);
    }
    #[test]
    fn rename_validates_windows_names_and_keeps_extension() {
        for name in [
            "", "..", "CON", "nul.txt", "COM1", "LPT9", "a/b", "a\\b", "x:", " x", "x ", "x.",
        ] {
            assert!(
                rename_destination(Path::new("clip.MP4"), name).is_err(),
                "{name}"
            );
        }
        assert_eq!(
            rename_destination(Path::new("Game/clip.MP4"), "new name").unwrap(),
            PathBuf::from("Game/new name.MP4")
        );
    }
    #[test]
    fn worker_shutdown_drains_queued_settings_and_watched_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        fs::write(&path, b"video").unwrap();
        let (tx, requests) = mpsc::channel();
        let (events, _rx) = mpsc::channel();
        tx.send(Request::Audio(path.clone(), BTreeMap::from([(1, 0.4)])))
            .unwrap();
        tx.send(Request::Master(32.0, true)).unwrap();
        tx.send(Request::RememberPlayback(clip_store::RememberPlayback::All))
            .unwrap();
        tx.send(Request::Resume(path.clone(), Some(87.5))).unwrap();
        tx.send(Request::Watched(
            vec![path.clone()],
            true,
            "2026-10-14T18:42:00Z".into(),
        ))
        .unwrap();
        tx.send(Request::Watched(
            vec![path.clone()],
            false,
            "2026-10-14T18:42:00Z".into(),
        ))
        .unwrap();
        tx.send(Request::Shutdown).unwrap();
        run(
            dir.path(),
            requests,
            events,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let config = clip_store::read_config(dir.path()).unwrap();
        assert_eq!(config.volume, 32.0);
        assert_eq!(
            config.player.remember_playback,
            clip_store::RememberPlayback::All
        );
        assert!(config.muted);
        let state = clip_store::media_state(&clip_store::read_history(dir.path()).unwrap(), &path);
        assert!(state.watched_at.is_empty());
        assert_eq!(state.track_gains.get(&1), Some(&0.4));
        assert_eq!(state.resume_position, Some(87.5));
    }
    #[test]
    fn preserves_nested_folders_and_deduplicates_roots() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("Game");
        let nested = game.join("Highlights");
        fs::create_dir_all(&nested).unwrap();
        fs::write(game.join("one.mp4"), b"one").unwrap();
        fs::write(nested.join("two.MP4"), b"two").unwrap();
        fs::write(game.join("image.png"), b"image").unwrap();
        let (catalog, errors) = scan(&[root.path().to_owned(), game.clone()]);
        assert!(errors.is_empty());
        assert_eq!(catalog.clips.len(), 2);
        assert!(catalog.folders.contains(&nested));
        assert_eq!(
            catalog
                .clips
                .iter()
                .filter(|c| c.path.parent() == Some(game.as_path()))
                .count(),
            1
        );
        assert!(
            catalog
                .clips
                .windows(2)
                .all(|pair| pair[0].created >= pair[1].created)
        );
    }
    #[test]
    fn changed_file_invalidates_thumbnail_stamp() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("clip.mp4");
        fs::write(&path, b"old").unwrap();
        let old = stamp(&path).unwrap();
        fs::write(&path, b"new content").unwrap();
        let new = stamp(&path).unwrap();
        assert_ne!(new, old);
    }

    #[test]
    fn worker_persists_consent_history_and_observes_changes() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join(".star-player");
        let root = temp.path().join("Clips");
        fs::create_dir_all(root.join("Game")).unwrap();
        let first = root.join("Game/first.mp4");
        fs::write(&first, b"identical clip").unwrap();
        let (tx, requests) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker_dir = store.clone();
        let worker = thread::spawn(move || run(&worker_dir, requests, events, stopping));
        tx.send(Request::Consent(false)).unwrap();
        tx.send(Request::AddFolder(root.clone())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                Event::Catalog(catalog) if catalog.clips.len() == 1 => break,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        fs::create_dir_all(root.join("Game/Nested")).unwrap();
        let second = root.join("Game/Nested/first.mp4");
        fs::copy(&first, &second).unwrap();
        // No Refresh request: this catalog must be produced by the filesystem watcher.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                Event::Catalog(catalog) if catalog.clips.len() == 2 => break,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        let timestamp = "2026-10-14T18:42:00Z".to_owned();
        tx.send(Request::Watched(
            vec![first.clone(), second.clone()],
            true,
            timestamp.clone(),
        ))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Event::History(history) = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
                && !history.is_empty()
            {
                assert_eq!(history.len(), 2);
                assert!(history.values().all(|state| state.watched_at == timestamp));
                break;
            }
        }
        tx.send(Request::Watched(vec![second], false, timestamp))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Event::History(history) = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
                && history.values().any(|state| state.watched_at.is_empty())
            {
                break;
            }
        }
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        assert_eq!(
            clip_store::read_config(&store).unwrap().clip_discovery,
            Discovery::Declined
        );
        assert_eq!(
            clip_store::read_history(&store)
                .unwrap()
                .values()
                .filter(|s| !s.watched_at.is_empty())
                .count(),
            1
        );
    }
}
