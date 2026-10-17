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

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum RememberPlayback {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "5min")]
    FiveMinutes,
    #[default]
    #[serde(rename = "10min")]
    TenMinutes,
    #[serde(rename = "15min")]
    FifteenMinutes,
    #[serde(rename = "30min")]
    ThirtyMinutes,
    #[serde(rename = "60min")]
    SixtyMinutes,
}

impl RememberPlayback {
    pub const OPTIONS: [Self; 6] = [
        Self::All,
        Self::FiveMinutes,
        Self::TenMinutes,
        Self::FifteenMinutes,
        Self::ThirtyMinutes,
        Self::SixtyMinutes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::FiveMinutes => "5min",
            Self::TenMinutes => "10min",
            Self::FifteenMinutes => "15min",
            Self::ThirtyMinutes => "30min",
            Self::SixtyMinutes => "60min",
        }
    }

    pub fn includes(self, duration: f64) -> bool {
        let minutes = match self {
            Self::All => 0,
            Self::FiveMinutes => 5,
            Self::TenMinutes => 10,
            Self::FifteenMinutes => 15,
            Self::ThirtyMinutes => 30,
            Self::SixtyMinutes => 60,
        };
        duration.is_finite() && duration > f64::from(minutes * 60)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    pub remember_playback: RememberPlayback,
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
    pub player: PlayerConfig,
    pub updates: UpdateConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    pub automatic_check: bool,
    pub automatic_download: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            automatic_check: true,
            automatic_download: true,
        }
    }
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
            player: PlayerConfig::default(),
            updates: UpdateConfig::default(),
        }
    }
}

pub fn directory() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Cannot locate your home directory")?;
    directory_in(&home)
}

fn directory_in(home: &Path) -> Result<PathBuf, String> {
    // Serialize migration within the process, including decoder initialization.
    static MIGRATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = MIGRATION.lock().map_err(|_| "Data migration lock failed")?;
    let old = home.join(".spacer");
    let new = home.join(".star-player");
    let prepare = || -> std::io::Result<()> {
        if !new.try_exists()? && old.try_exists()? {
            if !old.is_dir() {
                return Err(std::io::Error::other(
                    "The legacy data path is not a directory",
                ));
            }
            fs::rename(&old, &new)?;
        }
        fs::create_dir_all(&new)
    };
    prepare().map_err(|error| {
        format!(
            "Cannot prepare StarPlayer data at {}: {error}. Existing data was not deleted; close any running Spacer instances and check folder permissions before retrying",
            new.display()
        )
    })?;
    Ok(new)
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
    pub resume_position: Option<f64>,
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
    #[serde(default)]
    resume_position: Option<f64>,
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
        && headers
            != [
                "sha256",
                "identity_version",
                "watched_at",
                "track_gains",
                "resume_position",
            ]
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
        validate_resume_position(row.resume_position)?;
        if history
            .insert(
                row.sha256.to_lowercase(),
                MediaState {
                    identity_version: row.identity_version,
                    watched_at: row.watched_at,
                    track_gains,
                    resume_position: row.resume_position,
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
        .write_record([
            "sha256",
            "identity_version",
            "watched_at",
            "track_gains",
            "resume_position",
        ])
        .map_err(|e| e.to_string())?;
    for (sha256, state) in history {
        validate_gains(&state.track_gains)?;
        validate_resume_position(state.resume_position)?;
        writer
            .serialize(WatchedRow {
                sha256: sha256.clone(),
                identity_version: state.identity_version,
                watched_at: state.watched_at.clone(),
                track_gains: serde_json::to_string(&state.track_gains)
                    .map_err(|e| e.to_string())?,
                resume_position: state.resume_position,
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

pub fn validate_resume_position(position: Option<f64>) -> Result<(), String> {
    if position.is_some_and(|p| !p.is_finite() || p <= 0.0) {
        return Err("Invalid resume position; watched.csv left unchanged".into());
    }
    Ok(())
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
    fn migrates_all_legacy_data_and_reuses_destination() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(".spacer");
        for relative in [
            "config.json",
            "watched.csv",
            "keybinds.json",
            "appearance.json",
            "themes/custom.json",
            "runtime/bundle/decoder.dll",
            "thumbnails/clip.png",
            "game-icons/images/game.png",
        ] {
            let path = old.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, relative).unwrap();
        }
        let new = directory_in(home.path()).unwrap();
        assert_eq!(new, home.path().join(".star-player"));
        assert!(!old.exists());
        for relative in [
            "config.json",
            "watched.csv",
            "keybinds.json",
            "appearance.json",
            "themes/custom.json",
            "runtime/bundle/decoder.dll",
            "thumbnails/clip.png",
            "game-icons/images/game.png",
        ] {
            assert_eq!(fs::read_to_string(new.join(relative)).unwrap(), relative);
        }
        assert_eq!(directory_in(home.path()).unwrap(), new);
    }

    #[test]
    fn existing_destination_wins_without_merging() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(".spacer");
        let new = home.path().join(".star-player");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("config.json"), "old").unwrap();
        fs::write(new.join("config.json"), "new").unwrap();
        assert_eq!(directory_in(home.path()).unwrap(), new);
        assert_eq!(fs::read_to_string(old.join("config.json")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new.join("config.json")).unwrap(), "new");
    }

    #[test]
    fn creates_a_fresh_data_directory() {
        let home = tempfile::tempdir().unwrap();
        assert!(directory_in(home.path()).unwrap().is_dir());
        assert!(!home.path().join(".spacer").exists());
    }

    #[test]
    fn migration_failure_does_not_start_fresh() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(".spacer");
        fs::write(&old, "preserve me").unwrap();
        assert!(directory_in(home.path()).is_err());
        assert_eq!(fs::read_to_string(old).unwrap(), "preserve me");
        assert!(!home.path().join(".star-player").exists());
    }

    #[test]
    fn blocked_destination_preserves_legacy_data() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(".spacer");
        let new = home.path().join(".star-player");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("config.json"), "old").unwrap();
        fs::write(&new, "blocked").unwrap();
        assert!(directory_in(home.path()).is_err());
        assert_eq!(fs::read_to_string(old.join("config.json")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new).unwrap(), "blocked");
    }
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
        assert_eq!(
            config.player.remember_playback,
            RememberPlayback::TenMinutes
        );
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
    fn remember_playback_options_roundtrip_and_reject_unknown_values() {
        let dir = tempfile::tempdir().unwrap();
        for option in RememberPlayback::OPTIONS {
            let mut config = Config::default();
            config.player.remember_playback = option;
            save_config(dir.path(), &config).unwrap();
            assert_eq!(
                read_config(dir.path()).unwrap().player.remember_playback,
                option
            );
        }
        let bytes = br#"{"version":1,"player":{"remember_playback":"invalid"}}"#;
        fs::write(dir.path().join("config.json"), bytes).unwrap();
        assert!(read_config(dir.path()).is_err());
        assert_eq!(fs::read(dir.path().join("config.json")).unwrap(), bytes);
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
                resume_position: Some(123.5),
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
    fn audio_history_upgrades_without_losing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "b".repeat(64);
        fs::write(
            dir.path().join("watched.csv"),
            format!("sha256,identity_version,watched_at,track_gains\n{hash},2,,{{}}\n"),
        )
        .unwrap();
        let mut history = read_history(dir.path()).unwrap();
        assert_eq!(history[&hash].resume_position, None);
        history.get_mut(&hash).unwrap().resume_position = Some(63.25);
        save_history(dir.path(), &history).unwrap();
        assert_eq!(read_history(dir.path()).unwrap(), history);
    }

    #[test]
    fn invalid_resume_positions_leave_history_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        save_history(dir.path(), &History::new()).unwrap();
        let original = fs::read(dir.path().join("watched.csv")).unwrap();
        for position in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let history = History::from([(
                "a".repeat(64),
                MediaState {
                    identity_version: 2,
                    resume_position: Some(position),
                    ..Default::default()
                },
            )]);
            assert!(save_history(dir.path(), &history).is_err());
            assert_eq!(fs::read(dir.path().join("watched.csv")).unwrap(), original);
        }
        for position in ["NaN", "inf", "-1", "0"] {
            let bytes = format!(
                "sha256,identity_version,watched_at,track_gains,resume_position\n{},2,,{{}},{position}\n",
                "a".repeat(64)
            );
            fs::write(dir.path().join("watched.csv"), &bytes).unwrap();
            assert!(read_history(dir.path()).is_err());
            assert_eq!(
                fs::read_to_string(dir.path().join("watched.csv")).unwrap(),
                bytes
            );
        }
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
