//! Small, atomic preference and watched-history files. Never replace invalid data.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Discovery {
    #[default]
    NotAsked,
    Enabled,
    Declined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub clip_discovery: Discovery,
    pub steam_discovery: Discovery,
    pub clip_directories: Vec<PathBuf>,
    pub game_folder_sort: String,
    pub volume: f64,
    pub muted: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            clip_discovery: Discovery::NotAsked,
            steam_discovery: Discovery::NotAsked,
            clip_directories: vec![],
            game_folder_sort: "newest_clip".into(),
            volume: 100.0,
            muted: false,
        }
    }
}

pub fn directory() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|p| p.join(".spacer"))
        .ok_or_else(|| "Cannot locate your home directory".into())
}

/// Hide Windows' extended-path syntax in UI only. Never use this for file I/O.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        // Only strip drive paths; other extended namespaces must stay intact.
        if rest.as_bytes().get(1) == Some(&b':') {
            rest.to_owned()
        } else {
            text.into_owned()
        }
    } else {
        text.into_owned()
    }
}

pub fn read_config(dir: &Path) -> Result<Config, String> {
    let path = dir.join("config.json");
    match fs::read(&path) {
        Ok(data) => {
            let config: Config =
                serde_json::from_slice(&data).map_err(|e| format!("{}: {e}", path.display()))?;
            if config.version != 1 {
                return Err("Unsupported config.json version; file left unchanged".into());
            }
            if !config.volume.is_finite() || !(0.0..=100.0).contains(&config.volume) {
                return Err("Invalid master volume; config.json left unchanged".into());
            }
            Ok(config)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist(path)
        .map_err(|e| format!("Save {}: {e}", path.display()))?;
    Ok(())
}

pub fn save_config(dir: &Path, config: &Config) -> Result<(), String> {
    atomic_write(
        &dir.join("config.json"),
        &serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?,
    )
}

pub fn add_directory(config: &mut Config, path: PathBuf) -> Result<(), String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!("Clip directory is unavailable: {}", path.display()));
    }
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !config
        .clip_directories
        .iter()
        .any(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == path)
    {
        config.clip_directories.push(path);
    }
    Ok(())
}

pub fn nvidia_directory(data: &[u8]) -> Result<PathBuf, String> {
    let settings: serde_json::Value =
        serde_json::from_slice(data).map_err(|e| format!("Invalid NVIDIA settings: {e}"))?;
    let path = settings
        .pointer("/settings/currentDirectoryV2")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or("NVIDIA settings do not contain settings.currentDirectoryV2")?;
    Ok(PathBuf::from(path))
}

pub fn detect_nvidia(config: &mut Config) -> Result<(), String> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
    let settings =
        PathBuf::from(local).join("NVIDIA Corporation/NVIDIA Overlay/GallerySettings.json");
    let data = fs::read(&settings).map_err(|e| {
        format!(
            "Cannot read {}: {e}. You can add a clip folder manually.",
            settings.display()
        )
    })?;
    add_directory(config, nvidia_directory(&data)?)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaState {
    pub identity_version: u32,
    pub watched_at: String,
    pub track_gains: BTreeMap<i64, f64>,
}

fn legacy_version() -> u32 {
    1
}

#[derive(Serialize, Deserialize)]
struct WatchedRow {
    sha256: String,
    #[serde(default = "legacy_version")]
    identity_version: u32,
    watched_at: String,
    #[serde(default)]
    track_gains: String,
}

pub type History = BTreeMap<String, MediaState>;

pub fn read_history(dir: &Path) -> Result<History, String> {
    let path = dir.join("watched.csv");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(History::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut reader = csv::Reader::from_reader(file);
    let headers = reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if headers != ["sha256", "watched_at"]
        && headers != ["sha256", "identity_version", "watched_at", "track_gains"]
    {
        return Err("Invalid watched.csv headers; file left unchanged".into());
    }
    let mut history = History::new();
    for row in reader.deserialize::<WatchedRow>() {
        let row = row.map_err(|e| format!("Invalid watched.csv: {e}"))?;
        if row.sha256.len() != 64
            || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(row.identity_version, 1 | 2)
            || (row.watched_at.is_empty() && row.identity_version == 1)
            || (!row.watched_at.is_empty()
                && chrono::DateTime::parse_from_rfc3339(&row.watched_at).is_err())
        {
            return Err("Invalid watched.csv record; file left unchanged".into());
        }
        let track_gains: BTreeMap<i64, f64> = if row.track_gains.is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str(&row.track_gains)
                .map_err(|e| format!("Invalid track gains: {e}; watched.csv left unchanged"))?
        };
        validate_gains(&track_gains)?;
        if history
            .insert(
                row.sha256.to_lowercase(),
                MediaState {
                    identity_version: row.identity_version,
                    watched_at: row.watched_at,
                    track_gains,
                },
            )
            .is_some()
        {
            return Err("Duplicate watched.csv identity; file left unchanged".into());
        }
    }
    Ok(history)
}

pub fn save_history(dir: &Path, history: &History) -> Result<(), String> {
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(Vec::new());
    writer
        .write_record(["sha256", "identity_version", "watched_at", "track_gains"])
        .map_err(|e| e.to_string())?;
    for (sha256, state) in history {
        validate_gains(&state.track_gains)?;
        writer
            .serialize(WatchedRow {
                sha256: sha256.clone(),
                identity_version: state.identity_version,
                watched_at: state.watched_at.clone(),
                track_gains: serde_json::to_string(&state.track_gains)
                    .map_err(|e| e.to_string())?,
            })
            .map_err(|e| e.to_string())?;
    }
    atomic_write(
        &dir.join("watched.csv"),
        &writer.into_inner().map_err(|e| e.to_string())?,
    )
}

pub fn hash_filename(path: &Path) -> Result<String, String> {
    let name = path.file_name().ok_or("Clip path has no filename")?;
    let folder = path.parent().and_then(Path::file_name).unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(folder.as_encoded_bytes());
    hash.update([0]);
    hash.update(name.as_encoded_bytes());
    Ok(format!("{:x}", hash.finalize()))
}

fn legacy_hash(path: &Path) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            path.file_name()
                .ok_or("Missing filename")?
                .as_encoded_bytes()
        )
    ))
}

pub fn media_state(history: &History, path: &Path) -> MediaState {
    hash_filename(path)
        .ok()
        .and_then(|key| history.get(&key))
        .cloned()
        .or_else(|| {
            legacy_hash(path)
                .ok()
                .and_then(|key| history.get(&key))
                .filter(|s| s.identity_version == 1)
                .cloned()
        })
        .map(|mut state| {
            state.identity_version = 2;
            state
        })
        .unwrap_or(MediaState {
            identity_version: 2,
            ..MediaState::default()
        })
}

pub fn validate_gains(gains: &BTreeMap<i64, f64>) -> Result<(), String> {
    if gains
        .iter()
        .any(|(id, gain)| *id <= 0 || !gain.is_finite() || !(0.0..=10.0).contains(gain))
    {
        return Err("Invalid audio track gains; watched.csv left unchanged".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_paths_hide_extended_drive_and_unc_prefixes_only() {
        assert_eq!(
            display_path(Path::new(r"\\?\F:\Clips\Gothic 1")),
            r"F:\Clips\Gothic 1"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\Clips")),
            r"\\server\share\Clips"
        );
        assert_eq!(display_path(Path::new(r"F:\Clips")), r"F:\Clips");
        assert_eq!(
            display_path(Path::new(r"\\?\Volume{abc}\Clips")),
            r"\\?\Volume{abc}\Clips"
        );
    }

    #[test]
    fn legacy_records_remain_readable_and_use_folder_identity_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new("Desktop/clip.mp4");
        let old_hash = legacy_hash(path).unwrap();
        fs::write(
            dir.path().join("watched.csv"),
            format!("sha256,watched_at\n{old_hash},2026-10-14T18:42:00Z\n"),
        )
        .unwrap();
        let mut history = read_history(dir.path()).unwrap();
        let mut state = media_state(&history, path);
        assert_eq!(state.identity_version, 2);
        assert!(!state.watched_at.is_empty());
        state.watched_at.clear();
        state.track_gains.insert(1, 0.0);
        history.insert(hash_filename(path).unwrap(), state.clone());
        save_history(dir.path(), &history).unwrap();
        let loaded = read_history(dir.path()).unwrap();
        assert_eq!(media_state(&loaded, path), state);
        assert!(
            !media_state(&loaded, Path::new("Other/clip.mp4"))
                .watched_at
                .is_empty()
        );
        assert!(loaded.contains_key(&old_hash));
    }
    #[test]
    fn folder_and_filename_are_separate_fields() {
        assert_ne!(
            hash_filename(Path::new("a-b/c.mp4")).unwrap(),
            hash_filename(Path::new("a/b-c.mp4")).unwrap()
        );
        assert_eq!(
            hash_filename(Path::new("root/Desktop/clip.mp4")).unwrap(),
            hash_filename(Path::new("other/Desktop/clip.mp4")).unwrap()
        );
        assert_ne!(
            hash_filename(Path::new("Desktop/clip.mp4")).unwrap(),
            hash_filename(Path::new("Game/clip.mp4")).unwrap()
        );
    }
    #[test]
    fn old_configs_get_audio_defaults_and_invalid_gains_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), br#"{"version":1}"#).unwrap();
        let config = read_config(dir.path()).unwrap();
        assert_eq!(config.volume, 100.0);
        assert!(!config.muted);
        for gain in [f64::NAN, f64::INFINITY, -0.1, 10.1] {
            assert!(validate_gains(&BTreeMap::from([(1, gain)])).is_err());
        }
        fs::write(
            dir.path().join("config.json"),
            br#"{"version":1,"volume":101}"#,
        )
        .unwrap();
        assert!(read_config(dir.path()).is_err());
    }
    #[test]
    fn config_and_csv_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            clip_discovery: Discovery::Declined,
            ..Config::default()
        };
        add_directory(&mut config, dir.path().to_owned()).unwrap();
        add_directory(&mut config, dir.path().to_owned()).unwrap();
        save_config(dir.path(), &config).unwrap();
        let loaded = read_config(dir.path()).unwrap();
        assert_eq!(loaded.clip_discovery, Discovery::Declined);
        assert_eq!(loaded.clip_directories.len(), 1);
        let history = History::from([(
            "a".repeat(64),
            MediaState {
                identity_version: 2,
                watched_at: "2026-10-14T18:42:00Z".into(),
                track_gains: BTreeMap::from([(1, 0.4)]),
            },
        )]);
        save_history(dir.path(), &history).unwrap();
        assert_eq!(read_history(dir.path()).unwrap(), history);
        save_history(dir.path(), &History::new()).unwrap();
        assert!(read_history(dir.path()).unwrap().is_empty());
    }
    #[test]
    fn malformed_files_are_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), b"broken").unwrap();
        assert!(read_config(dir.path()).is_err());
        fs::write(dir.path().join("watched.csv"), b"wrong,headers\na,b\n").unwrap();
        assert!(read_history(dir.path()).is_err());
        assert_eq!(fs::read(dir.path().join("config.json")).unwrap(), b"broken");
    }
    #[test]
    fn nvidia_settings_and_filename_identity() {
        assert_eq!(
            nvidia_directory(br#"{"settings":{"currentDirectoryV2":"C:\\Clips"}}"#).unwrap(),
            PathBuf::from("C:\\Clips")
        );
        assert!(nvidia_directory(br#"{"settings":{}}"#).is_err());
        assert!(nvidia_directory(br#"{"settings":{"currentDirectoryV2":42}}"#).is_err());
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.mp4");
        let b = dir.path().join("b.mp4");
        fs::write(&a, b"clip").unwrap();
        fs::copy(&a, &b).unwrap();
        assert_ne!(hash_filename(&a).unwrap(), hash_filename(&b).unwrap());
        let original = hash_filename(&b).unwrap();
        fs::write(&b, b"changed").unwrap();
        assert_eq!(hash_filename(&b).unwrap(), original);
        assert_ne!(
            hash_filename(&a).unwrap(),
            hash_filename(&dir.path().join("another-folder/a.mp4")).unwrap()
        );
        // No file needs to exist: this operation never reads video bytes.
        assert!(hash_filename(Path::new("missing-video.mp4")).is_ok());
    }
}
