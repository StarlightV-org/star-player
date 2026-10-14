//! Filesystem and decoder work stays off the UI thread.
use crate::{
    clip_store::{self, Config, Discovery, History},
    playback::PreviewDecoder,
};
use notify::{RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashSet, VecDeque},
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
    AddFolder(PathBuf),
    Sort(String),
    Refresh,
    Thumbnail(PathBuf, Stamp),
    Watched(Vec<PathBuf>, bool, String),
    Delete(Vec<PathBuf>, Arc<AtomicBool>),
}
pub enum Event {
    Config(Config),
    Catalog(Catalog),
    History(History),
    Thumbnail(PathBuf, Stamp, Option<image::RgbaImage>),
    WatchedComplete(Vec<PathBuf>),
    Error(String),
}

pub struct LibraryWorker {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
}
impl LibraryWorker {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        thread::spawn(move || {
            let result = clip_store::directory()
                .and_then(|dir| run(&dir, requests, events.clone(), stopping));
            if let Err(error) = result {
                let _ = events.send(Event::Error(error));
            }
        });
        Self { tx, rx, stop }
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
    while !stop.load(Ordering::Acquire) {
        for request in requests.try_iter() {
            match request {
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
                Request::AddFolder(path) => {
                    let mut next = config.clone();
                    let result = if config_valid {
                        clip_store::add_directory(&mut next, path)
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
            dirty.get_or_insert(Instant::now() - Duration::from_secs(1));
        }
        if dirty.is_some_and(|at| at.elapsed() >= Duration::from_millis(400)) {
            watched_roots.retain(|p: &PathBuf| {
                if p.is_dir() {
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
                            "Repair watched.csv before saving watched status".into(),
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
                                    next.insert(hash.clone(), timestamp.clone());
                                } else {
                                    next.remove(&hash);
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
                        let _ = events.send(Event::Thumbnail(path, expected, None));
                        continue;
                    }
                    let key = format!(
                        "{:x}",
                        Sha256::digest(format!("{}:{expected:?}", path.display()).as_bytes())
                    );
                    let cache = dir.join("thumbnails").join(format!("{key}.jpg"));
                    let cached = image::open(&cache).ok().map(|i| i.into_rgba8());
                    let image = cached.or_else(|| {
                        if decoder.is_none() {
                            decoder = PreviewDecoder::new().ok();
                        }
                        let result = decoder.as_mut()?.decode(
                            &path,
                            1.0,
                            Instant::now() + Duration::from_secs(5),
                            || stop.load(Ordering::Acquire),
                        );
                        match result {
                            Ok(image) => {
                                if stamp(&path).ok().as_ref() != Some(&expected) {
                                    return None;
                                }
                                let _ = fs::create_dir_all(cache.parent().unwrap());
                                let _ = image::DynamicImage::ImageRgba8(image.clone())
                                    .into_rgb8()
                                    .save(&cache);
                                Some(image)
                            }
                            Err(_) => {
                                decoder = None;
                                None
                            }
                        }
                    });
                    let _ = events.send(Event::Thumbnail(path, expected, image));
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(30));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let store = temp.path().join(".spacer");
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
                assert_eq!(history.len(), 1);
                assert_eq!(history.values().next().unwrap(), &timestamp);
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
                && history.is_empty()
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
        assert!(clip_store::read_history(&store).unwrap().is_empty());
    }
}
