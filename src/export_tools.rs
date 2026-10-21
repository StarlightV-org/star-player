//! Optional FFmpeg, never embedded. Downloads require an explicit UI action and
//! the package manifest compiled into this exact StarPlayer version.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const ASSET: &str = "star-player-ffmpeg-windows-x86_64.zip";
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    version: String,
    target: String,
    asset: String,
    pub size: u64,
    sha256: String,
    files: Vec<Resource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Resource {
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Selection {
    path: PathBuf,
    sha256: String,
}

fn check(stop: &AtomicBool) -> Result<(), String> {
    if stop.load(Ordering::Acquire) {
        Err("Export cancelled".into())
    } else {
        Ok(())
    }
}

pub fn package() -> Result<Package, String> {
    let package: Option<Package> = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/export_package.json"
    )))
    .map_err(|e| format!("Invalid export package manifest: {e}"))?;
    let package = package.ok_or("This development build has no release download package. Choose a compatible ffmpeg.exe, or use an official release")?;
    package.validate()?;
    Ok(package)
}

impl Package {
    fn validate(&self) -> Result<(), String> {
        let hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        if self.version != env!("CARGO_PKG_VERSION")
            || self.target != "x86_64-pc-windows-msvc"
            || self.asset != ASSET
            || self.size == 0
            || self.size > MAX_PACKAGE
            || !hash(&self.sha256)
            || self.files.is_empty()
            || self.files.len() > 4
        {
            return Err("Export package does not match this StarPlayer version or target".into());
        }
        let mut names = HashSet::new();
        let mut total = 0u64;
        for file in &self.files {
            if !matches!(
                file.name.as_str(),
                "ffmpeg.exe" | "LICENSE" | "README.txt" | "THIRD_PARTY_NOTICES.md"
            ) || !names.insert(&file.name)
                || !hash(&file.sha256)
                || file.size == 0
                || file.size > MAX_UNPACKED
            {
                return Err("Invalid export package file list".into());
            }
            total += file.size;
        }
        if !names.contains(&"ffmpeg.exe".to_owned())
            || !names.contains(&"LICENSE".to_owned())
            || total > MAX_UNPACKED
        {
            return Err(
                "Export package must contain FFmpeg and its license within the size limit".into(),
            );
        }
        Ok(())
    }

    fn url(&self) -> String {
        // Never request latest, another tag, or a URL supplied by the server.
        format!(
            "https://github.com/StarlightV-org/star-player/releases/download/v{}/{}",
            self.version, self.asset
        )
    }

    pub fn unpacked_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

fn root() -> Result<PathBuf, String> {
    #[cfg(not(test))]
    {
        Ok(crate::clip_store::directory()?
            .join("runtime")
            .join("exports"))
    }
    #[cfg(test)]
    {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        Ok(ROOT
            .get_or_init(|| tempfile::tempdir().expect("Export tools test cache"))
            .path()
            .to_owned())
    }
}

fn digest(path: &Path, stop: &AtomicBool) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check(stop)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verify_install(directory: &Path, package: &Package, stop: &AtomicBool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Invalid FFmpeg cache directory".into());
    }
    for resource in &package.files {
        let path = directory.join(&resource.name);
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != resource.size
            || !digest(&path, stop)?.eq_ignore_ascii_case(&resource.sha256)
        {
            return Err(format!(
                "FFmpeg cache verification failed: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn candidates(value: &std::ffi::OsStr, cwd: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    std::env::split_paths(value)
        .filter_map(|directory| {
            if !directory.is_absolute() {
                return None;
            }
            let directory = fs::canonicalize(directory).ok()?;
            if fs::canonicalize(cwd).ok().as_ref() == Some(&directory) {
                return None;
            }
            let executable = directory.join("ffmpeg.exe");
            if !executable.is_file() || !seen.insert(executable.to_string_lossy().to_lowercase()) {
                return None;
            }
            Some(executable)
        })
        .take(32)
        .collect()
}

/// Resolve saved/global tools without any network request or installation.
pub fn find(stop: &AtomicBool) -> Result<Option<PathBuf>, String> {
    check(stop)?;
    #[cfg(test)]
    if let Some(path) = std::env::var_os("STAR_PLAYER_TEST_FFMPEG") {
        let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
        validate(&path, stop)?;
        return Ok(Some(path));
    }
    let root = root()?;
    let selected = root.join("selection.json");
    if selected.is_file() {
        let mut bytes = Vec::new();
        File::open(&selected)
            .map_err(|e| e.to_string())?
            .take(32769)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 32768 {
            return Err("FFmpeg selection file is too large".into());
        }
        let selection: Selection = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !selection.path.is_absolute() || digest(&selection.path, stop)? != selection.sha256 {
            return Err("The selected FFmpeg installation has changed. Choose it again or download the tested package".into());
        }
        validate(&selection.path, stop)?;
        return Ok(Some(selection.path));
    }
    if let Ok(package) = package() {
        let directory = root.join(&package.sha256);
        if directory.exists() {
            verify_install(&directory, &package, stop)
                .map_err(|e| format!("{e}. Remove {} to download it again", directory.display()))?;
            let path = directory.join("ffmpeg.exe");
            validate(&path, stop)?;
            return Ok(Some(path));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for executable in candidates(&path, &std::env::current_dir().map_err(|e| e.to_string())?) {
            check(stop)?;
            match validate(&executable, stop) {
                Ok(()) => return Ok(Some(executable)),
                Err(error) => crate::logging::Print(
                    crate::logging::Level::Info,
                    format!(
                        "Skipping incompatible FFmpeg {}: {error}",
                        executable.display()
                    ),
                ),
            }
        }
    }
    Ok(None)
}

pub fn select(path: &Path, stop: &AtomicBool) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|e| format!("Cannot access FFmpeg: {e}"))?;
    validate(&path, stop)?;
    let selection = Selection {
        sha256: digest(&path, stop)?,
        path: path.clone(),
    };
    let root = root()?;
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(&root).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&selection).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    check(stop)?;
    file.persist(root.join("selection.json"))
        .map_err(|e| e.to_string())?;
    Ok(path)
}

fn listing_has(output: &[u8], required: &[&str]) -> bool {
    let text = String::from_utf8_lossy(output);
    let names = text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .flat_map(|name| name.split(','))
        .collect::<HashSet<_>>();
    required.iter().all(|name| names.contains(name))
}

pub fn validate(path: &Path, stop: &AtomicBool) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("Choose an absolute path to ffmpeg.exe".into());
    }
    let query = |option: &str| {
        crate::clip_export::run(path, &["-hide_banner".into(), option.into()], stop, None)
    };
    let version = query("-version")?;
    let version = String::from_utf8_lossy(&version);
    if !version.starts_with("ffmpeg version ") {
        return Err("The selected executable is not FFmpeg".into());
    }
    if let Some(major) = version
        .split_whitespace()
        .nth(2)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        && major < 7
    {
        return Err("FFmpeg 7 or newer is required".into());
    }
    for (option, required) in [
        (
            "-encoders",
            &["libx264", "aac", "libvpx-vp9", "libopus", "libmp3lame"][..],
        ),
        (
            "-decoders",
            &[
                "libdav1d",
                "h264",
                "hevc",
                "vp9",
                "aac",
                "opus",
                "pcm_s16le",
            ][..],
        ),
        (
            "-filters",
            &["aresample", "aformat", "volume", "amix", "alimiter"][..],
        ),
        ("-muxers", &["mp4", "matroska", "webm", "mp3"][..]),
    ] {
        if !listing_has(&query(option)?, required) {
            return Err(format!(
                "FFmpeg is missing required {}. Download StarPlayer's tested package",
                option.trim_start_matches('-')
            ));
        }
    }
    let help = crate::clip_export::run(
        path,
        &["-hide_banner".into(), "-h".into(), "full".into()],
        stop,
        None,
    )?;
    let help = String::from_utf8_lossy(&help);
    if [
        "-fps_mode",
        "-enc_time_base",
        "-progress",
        "-xerror",
        "-protocol_whitelist",
    ]
    .iter()
    .any(|option| !help.contains(option))
    {
        return Err("FFmpeg lacks required export options".into());
    }
    Ok(())
}

fn extract(
    archive: &Path,
    destination: &Path,
    package: &Package,
    stop: &AtomicBool,
) -> Result<(), String> {
    package.validate()?;
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if zip.len() != package.files.len() {
        return Err("Unexpected files in FFmpeg download".into());
    }
    let mut seen = HashSet::new();
    for i in 0..zip.len() {
        check(stop)?;
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_owned();
        let resource = package
            .files
            .iter()
            .find(|f| f.name == name)
            .ok_or("Unexpected FFmpeg ZIP entry")?;
        if !seen.insert(name.clone())
            || entry.enclosed_name().as_deref() != Some(Path::new(&name))
            || entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || entry.size() != resource.size
        {
            return Err("Unsafe or invalid FFmpeg ZIP entry".into());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(&name))
            .map_err(|e| e.to_string())?;
        let mut remaining = resource.size;
        let mut buffer = [0; 64 * 1024];
        loop {
            check(stop)?;
            let count = entry.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            remaining = remaining
                .checked_sub(count as u64)
                .ok_or("FFmpeg ZIP entry exceeds its declared size")?;
            file.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        if remaining != 0 {
            return Err("Truncated FFmpeg ZIP entry".into());
        }
        file.sync_all().map_err(|e| e.to_string())?;
    }
    verify_install(destination, package, stop)
}

fn fetch_chunks(
    url: String,
    expected_size: u64,
) -> std::sync::mpsc::Receiver<Result<Vec<u8>, String>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    // The transport owns no files and cannot install or execute anything.
    // Dropping its receiver cancels the consumer immediately, even if the
    // server has stalled inside a blocking read. Its request remains bounded.
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let client = reqwest::blocking::Client::builder()
                .user_agent(concat!("StarPlayer/", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(600))
                .build()
                .map_err(|e| e.to_string())?;
            let mut response = client
                .get(url)
                .send()
                .and_then(|r| r.error_for_status())
                .map_err(|e| e.to_string())?;
            if response
                .content_length()
                .is_some_and(|size| size != expected_size)
            {
                return Err("FFmpeg download size does not match this release".into());
            }
            loop {
                let mut buffer = vec![0; 64 * 1024];
                let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
                buffer.truncate(count);
                if tx.send(Ok(buffer)).is_err() || count == 0 {
                    return Ok(());
                }
            }
        })();
        if let Err(error) = result {
            let _ = tx.send(Err(error));
        }
    });
    rx
}

fn receive_archive(
    rx: &std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    file: &mut impl Write,
    package: &Package,
    stop: &AtomicBool,
    progress: impl Fn(u64, u64),
) -> Result<(), String> {
    use std::sync::mpsc::RecvTimeoutError;
    let mut done = 0u64;
    let mut hash = Sha256::new();
    progress(0, package.size);
    loop {
        check(stop)?;
        let bytes = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => result?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("FFmpeg download interrupted".into()),
        };
        if bytes.is_empty() {
            break;
        }
        done += bytes.len() as u64;
        if done > package.size {
            return Err("FFmpeg download exceeds its declared size".into());
        }
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        hash.update(&bytes);
        progress(done, package.size);
    }
    if done != package.size
        || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&package.sha256)
    {
        return Err("FFmpeg download checksum or size mismatch".into());
    }
    check(stop)
}

/// Only called after the user presses Download. All bytes must match the
/// manifest embedded in the app, including before executing the new FFmpeg.
pub fn download(stop: &AtomicBool, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    let package = package()?;
    let _guard = INSTALL_LOCK
        .try_lock()
        .map_err(|_| "Another FFmpeg download is already running. Try again when it finishes")?;
    check(stop)?;
    let root = root()?;
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let destination = root.join(&package.sha256);
    if destination.exists() {
        verify_install(&destination, &package, stop)?;
    } else {
        let temp = tempfile::Builder::new()
            .prefix("download-")
            .tempdir_in(&root)
            .map_err(|e| e.to_string())?;
        let archive = temp.path().join("ffmpeg.zip");
        let mut file = File::create(&archive).map_err(|e| e.to_string())?;
        let rx = fetch_chunks(package.url(), package.size);
        receive_archive(&rx, &mut file, &package, stop, progress)?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        let staging = temp.path().join("installed");
        fs::create_dir(&staging).map_err(|e| e.to_string())?;
        extract(&archive, &staging, &package, stop)?;
        validate(&staging.join("ffmpeg.exe"), stop)?;
        check(stop)?;
        if let Err(error) = fs::rename(&staging, &destination) {
            if !destination.exists() {
                return Err(error.to_string());
            }
            verify_install(&destination, &package, stop)?;
        }
    }
    // Replace only our saved external-installation preference, never the
    // external executable, when the user explicitly chooses the download.
    check(stop)?;
    let selection = root.join("selection.json");
    if selection.exists() {
        fs::remove_file(selection).map_err(|e| e.to_string())?;
    }
    Ok(destination.join("ffmpeg.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_for(bytes: &[u8]) -> Package {
        let resource = |name: &str, contents: &[u8]| Resource {
            name: name.into(),
            size: contents.len() as u64,
            sha256: format!("{:x}", Sha256::digest(contents)),
        };
        Package {
            version: env!("CARGO_PKG_VERSION").into(),
            target: "x86_64-pc-windows-msvc".into(),
            asset: ASSET.into(),
            size: 42,
            sha256: "a".repeat(64),
            files: vec![
                resource("ffmpeg.exe", bytes),
                resource("LICENSE", b"license"),
            ],
        }
    }

    #[test]
    fn package_is_bound_to_the_running_release() {
        let mut package = package_for(b"test");
        package.validate().unwrap();
        assert!(package.url().contains(&format!(
            "/releases/download/v{}/",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!package.url().contains("latest"));
        package.version = "0.0.0".into();
        assert!(package.validate().is_err());
        package.version = env!("CARGO_PKG_VERSION").into();
        package.asset = "ffprobe.exe".into();
        assert!(package.validate().is_err());
    }

    #[test]
    fn manifest_rejects_unsafe_duplicate_missing_or_oversized_files() {
        for name in [
            "../ffmpeg.exe",
            "C:/ffmpeg.exe",
            "ffmpeg.exe:other",
            "FFMPEG.EXE",
            "ffprobe.exe",
        ] {
            let mut package = package_for(b"test");
            package.files[0].name = name.into();
            assert!(package.validate().is_err(), "{name}");
        }
        let mut package = package_for(b"test");
        package.files.push(package.files[0].clone());
        assert!(package.validate().is_err());
        let mut package = package_for(b"test");
        package.files[0].size = MAX_UNPACKED;
        assert!(package.validate().is_err());
    }

    #[test]
    fn capabilities_match_exact_names_not_descriptions() {
        assert!(listing_has(
            b" V....D libdav1d AV1 decoder\n",
            &["libdav1d"]
        ));
        assert!(!listing_has(
            b" V....D libaom-av1 use libdav1d instead\n",
            &["libdav1d"]
        ));
        assert!(listing_has(
            b" E matroska,webm Matroska\n",
            &["matroska", "webm"]
        ));
    }

    #[test]
    fn downloads_reject_wrong_hash_size_disconnect_and_cancellation_without_network() {
        use std::sync::mpsc;
        let mut package = package_for(b"test");
        package.size = 4;
        package.sha256 = format!("{:x}", Sha256::digest(b"test"));
        let stop = AtomicBool::new(false);
        for bytes in [
            b"test".as_slice(),
            b"evil".as_slice(),
            b"too long".as_slice(),
            b"cut".as_slice(),
        ] {
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(bytes.to_vec())).unwrap();
            tx.send(Ok(Vec::new())).unwrap();
            let result = receive_archive(&rx, &mut Vec::new(), &package, &stop, |_, _| {});
            assert_eq!(result.is_ok(), bytes == b"test");
        }
        let (tx, rx) = mpsc::channel();
        drop(tx);
        assert!(receive_archive(&rx, &mut Vec::new(), &package, &stop, |_, _| {}).is_err());
        let (_tx, rx) = mpsc::channel();
        let started = std::time::Instant::now();
        assert_eq!(
            receive_archive(
                &rx,
                &mut Vec::new(),
                &package,
                &AtomicBool::new(true),
                |_, _| {}
            )
            .unwrap_err(),
            "Export cancelled"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn path_search_ignores_empty_relative_and_current_directories() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ffmpeg.exe"), b"do not execute").unwrap();
        fs::write(elsewhere.path().join("ffmpeg.exe"), b"candidate").unwrap();
        let paths = std::env::join_paths([
            Path::new(""),
            Path::new("."),
            dir.path(),
            elsewhere.path(),
            elsewhere.path(),
        ])
        .unwrap();
        assert_eq!(
            candidates(&paths, dir.path()),
            vec![
                fs::canonicalize(elsewhere.path())
                    .unwrap()
                    .join("ffmpeg.exe")
            ]
        );
    }

    #[test]
    fn extraction_checks_contents_and_rejects_unexpected_files_and_cancellation() {
        use zip::{ZipWriter, write::SimpleFileOptions};
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("tools.zip");
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        for (name, bytes) in [("ffmpeg.exe", &b"test"[..]), ("LICENSE", &b"license"[..])] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let destination = dir.path().join("installed");
        fs::create_dir(&destination).unwrap();
        let package = package_for(b"test");
        let stop = AtomicBool::new(false);
        extract(&archive, &destination, &package, &stop).unwrap();
        fs::write(destination.join("ffmpeg.exe"), b"evil").unwrap();
        assert!(verify_install(&destination, &package, &stop).is_err());
        let other = dir.path().join("cancelled");
        fs::create_dir(&other).unwrap();
        assert!(extract(&archive, &other, &package, &AtomicBool::new(true)).is_err());
        assert!(!other.join("ffmpeg.exe").exists());
        let mut wrong = package;
        wrong.files[0].name = "README.txt".into();
        assert!(extract(&archive, &other, &wrong, &stop).is_err());
    }

    #[test]
    fn extraction_rejects_zip_paths_even_with_a_valid_manifest() {
        use zip::{ZipWriter, write::SimpleFileOptions};
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["../ffmpeg.exe", "ffprobe.exe", "ffmpeg.exe:other"]
            .iter()
            .enumerate()
        {
            let archive = dir.path().join(format!("unsafe-{i}.zip"));
            let mut zip = ZipWriter::new(File::create(&archive).unwrap());
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(b"test").unwrap();
            zip.start_file("LICENSE", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"license").unwrap();
            zip.finish().unwrap();
            let destination = dir.path().join(format!("installed-{i}"));
            fs::create_dir(&destination).unwrap();
            assert!(
                extract(
                    &archive,
                    &destination,
                    &package_for(b"test"),
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
        }
        assert!(!dir.path().join("ffmpeg.exe").exists());
    }

    #[test]
    #[ignore = "requires a prepared release ZIP; set STAR_PLAYER_TEST_EXPORT_ZIP"]
    fn prepared_release_zip_matches_embedded_manifest_and_runs_without_ffprobe() {
        let archive = PathBuf::from(
            std::env::var_os("STAR_PLAYER_TEST_EXPORT_ZIP")
                .expect("Set STAR_PLAYER_TEST_EXPORT_ZIP"),
        );
        let package = package().unwrap();
        let stop = AtomicBool::new(false);
        assert_eq!(fs::metadata(&archive).unwrap().len(), package.size);
        assert_eq!(digest(&archive, &stop).unwrap(), package.sha256);
        let directory = tempfile::tempdir().unwrap();
        extract(&archive, directory.path(), &package, &stop).unwrap();
        validate(&directory.path().join("ffmpeg.exe"), &stop).unwrap();
        assert!(!directory.path().join("ffprobe.exe").exists());
        assert!(!directory.path().join("ffplay.exe").exists());
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            package.files.len()
        );
    }
}
