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
    pub clip_directories: Vec<PathBuf>,
    pub game_folder_sort: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            clip_discovery: Discovery::NotAsked,
            clip_directories: vec![],
            game_folder_sort: "newest_clip".into(),
        }
    }
}

pub fn directory() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|p| p.join(".spacer"))
        .ok_or_else(|| "Cannot locate your home directory".into())
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

#[derive(Serialize, Deserialize)]
struct WatchedRow {
    sha256: String,
    watched_at: String,
}

pub type History = BTreeMap<String, String>;

pub fn read_history(dir: &Path) -> Result<History, String> {
    let path = dir.join("watched.csv");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(History::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut reader = csv::Reader::from_reader(file);
    if reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .collect::<Vec<_>>()
        != ["sha256", "watched_at"]
    {
        return Err("Invalid watched.csv headers; file left unchanged".into());
    }
    let mut history = History::new();
    for row in reader.deserialize::<WatchedRow>() {
        let row = row.map_err(|e| format!("Invalid watched.csv: {e}"))?;
        if row.sha256.len() != 64
            || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || chrono::DateTime::parse_from_rfc3339(&row.watched_at).is_err()
        {
            return Err("Invalid watched.csv record; file left unchanged".into());
        }
        history.insert(row.sha256.to_lowercase(), row.watched_at);
    }
    Ok(history)
}

pub fn save_history(dir: &Path, history: &History) -> Result<(), String> {
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(Vec::new());
    writer
        .write_record(["sha256", "watched_at"])
        .map_err(|e| e.to_string())?;
    for (sha256, watched_at) in history {
        writer
            .serialize(WatchedRow {
                sha256: sha256.clone(),
                watched_at: watched_at.clone(),
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
    Ok(format!("{:x}", Sha256::digest(name.as_encoded_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let history = History::from([("a".repeat(64), "2026-10-14T18:42:00Z".into())]);
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
        assert_eq!(
            hash_filename(&a).unwrap(),
            hash_filename(&dir.path().join("another-folder/a.mp4")).unwrap()
        );
        // No file needs to exist: this operation never reads video bytes.
        assert!(hash_filename(Path::new("missing-video.mp4")).is_ok());
    }
}
