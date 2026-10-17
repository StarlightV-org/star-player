//! Signed, full-binary updates for the portable Windows release.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

const REPOSITORY: &str = "StarlightV-org/star-player";
const TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_BINARY: u64 = 512 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    version: String,
    target: String,
    asset: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug)]
pub struct SelectedRelease {
    manifest: Manifest,
    url: String,
    metadata: Vec<u8>,
    signature: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum State {
    Idle,
    Checking,
    Current,
    Available(SelectedRelease),
    Downloading(u64, u64),
    Verifying,
    Ready(PathBuf, String),
    PreparingRestart,
    Failed(String),
}

impl State {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "Not checked yet".into(),
            Self::Checking => "Checking for updates...".into(),
            Self::Current => "You're up to date".into(),
            Self::Available(release) => format!("Download update {}", release.manifest.version),
            Self::Downloading(done, total) => {
                format!("Downloading... {}%", done * 100 / total.max(&1))
            }
            Self::Verifying => "Verifying update...".into(),
            Self::Ready(_, _) => "Restart to update".into(),
            Self::PreparingRestart => "Preparing restart...".into(),
            Self::Failed(_) => "Update failed. Click to retry".into(),
        }
    }
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(..) | Self::Verifying | Self::PreparingRestart
        )
    }
    pub fn visible(&self) -> bool {
        !matches!(self, Self::Idle | Self::Current)
    }
}

pub struct Updater {
    pub state: State,
    tx: Sender<State>,
    rx: Receiver<State>,
    last_check: Option<Instant>,
}

impl Default for Updater {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: State::Idle,
            tx,
            rx,
            last_check: None,
        }
    }
}

impl Updater {
    pub fn poll(&mut self, config: &crate::clip_store::UpdateConfig) -> bool {
        let mut changed = false;
        for state in self.rx.try_iter() {
            self.state = state;
            changed = true;
        }
        if config.automatic_download && matches!(self.state, State::Available(_)) {
            self.download();
            changed = true;
        }
        // Start once saved preferences are loaded, including development builds.
        // Unit tests exercise scheduling without making real network requests.
        if !cfg!(test)
            && cfg!(all(windows, target_arch = "x86_64"))
            && self.should_check(config, Instant::now())
        {
            self.check();
            changed = true;
        }
        changed
    }

    fn should_check(&self, config: &crate::clip_store::UpdateConfig, now: Instant) -> bool {
        config.automatic_check
            && self
                .last_check
                .is_none_or(|last| now.saturating_duration_since(last) >= INTERVAL)
            && !self.state.busy()
            && !matches!(self.state, State::Ready(..) | State::Available(_))
    }

    pub fn check(&mut self) {
        if self.state.busy() || matches!(self.state, State::Ready(..)) {
            return;
        }
        self.last_check = Some(Instant::now());
        self.state = State::Checking;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let state = check_release()
                .map(|release| release.map_or(State::Current, State::Available))
                .unwrap_or_else(State::Failed);
            let _ = tx.send(state);
        });
    }

    pub fn download(&mut self) {
        let State::Available(release) = &self.state else {
            return;
        };
        let release = release.clone();
        self.state = State::Downloading(0, release.manifest.size);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let state = download(&release, &tx)
                .map(|path| State::Ready(path, release.manifest.version))
                .unwrap_or_else(State::Failed);
            let _ = tx.send(state);
        });
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("StarPlayer/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())
}

fn get_small(client: &reqwest::blocking::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![];
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Update metadata is too large".into());
    }
    Ok(bytes)
}

fn verify_manifest(metadata: &[u8], signature: &[u8], key: &str) -> Result<Manifest, String> {
    let key: [u8; 32] = STANDARD
        .decode(key.trim())
        .map_err(|_| "Invalid update public key")?
        .try_into()
        .map_err(|_| "Invalid update public key length")?;
    let signature = STANDARD
        .decode(signature.trim_ascii())
        .map_err(|_| "Invalid update signature")?;
    VerifyingKey::from_bytes(&key)
        .map_err(|_| "Invalid update public key")?
        .verify_strict(
            metadata,
            &Signature::from_slice(&signature).map_err(|_| "Invalid update signature length")?,
        )
        .map_err(|_| "Update signature verification failed")?;
    let manifest: Manifest = serde_json::from_slice(metadata).map_err(|e| e.to_string())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn public_key() -> Result<&'static str, String> {
    option_env!("STAR_PLAYER_UPDATE_PUBLIC_KEY")
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            "This build has no update verification key. Download an official release from GitHub."
                .into()
        })
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    let version =
        semver::Version::parse(&manifest.version).map_err(|_| "Invalid update version")?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    if version <= current || !version.pre.is_empty() {
        return Err("Update is not a newer stable version".into());
    }
    if manifest.target != TARGET
        || manifest.asset != "star-player-windows-x86_64.exe"
        || manifest.size == 0
        || manifest.size > MAX_BINARY
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Update target, asset, size, or checksum is invalid".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn check_release() -> Result<Option<SelectedRelease>, String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("Updates support Windows x86_64 only".into());
    }
    let client = client()?;
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    let mut bytes = vec![];
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Release response is too large".into());
    }
    let release: Release = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = semver::Version::parse(version).map_err(|_| "Invalid release tag")?;
    if release.draft
        || release.prerelease
        || !version.pre.is_empty()
        || version
            <= semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?
    {
        return Ok(None);
    }
    let asset_url = |name: &str| -> Result<String, String> {
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("Release is missing {name}"))?;
        let prefix = format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/",
            release.tag_name
        );
        if !asset.browser_download_url.starts_with(&prefix) {
            return Err("Unexpected release asset URL".into());
        }
        Ok(asset.browser_download_url.clone())
    };
    let metadata = get_small(&client, &asset_url("update.json")?, 16384)?;
    let signature = get_small(&client, &asset_url("update.json.sig")?, 1024)?;
    let manifest = verify_manifest(&metadata, &signature, public_key()?)?;
    if manifest.version != version.to_string() {
        return Err("Signed version does not match release tag".into());
    }
    let url = asset_url(&manifest.asset)?;
    Ok(Some(SelectedRelease {
        manifest,
        url,
        metadata,
        signature,
    }))
}

fn verify_file(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != manifest.size {
        return Err("Update size mismatch".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != manifest.sha256.to_lowercase() {
        return Err("Update checksum mismatch".into());
    }
    Ok(())
}

fn download(release: &SelectedRelease, tx: &Sender<State>) -> Result<PathBuf, String> {
    let root = crate::clip_store::directory()?.join("updates");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new()
        .prefix("release-")
        .tempdir_in(root)
        .map_err(|e| e.to_string())?;
    let mut response = client()?
        .get(&release.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let path = stage.path().join("star-player.exe");
    let mut file = File::create(&path).map_err(|e| e.to_string())?;
    let mut done = 0;
    let mut last_progress = Instant::now();
    let mut buffer = [0; 65536];
    loop {
        let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        done += count as u64;
        if done > release.manifest.size {
            return Err("Download exceeds signed size".into());
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        if last_progress.elapsed() >= Duration::from_millis(200) {
            let _ = tx.send(State::Downloading(done, release.manifest.size));
            last_progress = Instant::now();
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let _ = tx.send(State::Verifying);
    verify_file(&path, &release.manifest)?;
    fs::write(stage.path().join("update.json"), &release.metadata).map_err(|e| e.to_string())?;
    fs::write(stage.path().join("update.json.sig"), &release.signature)
        .map_err(|e| e.to_string())?;
    Ok(stage.keep())
}

fn verify_stage(stage: &Path) -> Result<Manifest, String> {
    let metadata = fs::read(stage.join("update.json")).map_err(|e| e.to_string())?;
    let signature = fs::read(stage.join("update.json.sig")).map_err(|e| e.to_string())?;
    let manifest = verify_manifest(&metadata, &signature, public_key()?)?;
    verify_file(&stage.join("star-player.exe"), &manifest)?;
    Ok(manifest)
}

/// Blocking. Run on a worker, and quit only after the helper starts successfully.
pub fn prepare_restart(stage: &Path, reopen: Option<&Path>) -> Result<(), String> {
    verify_stage(stage)?;
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = current.parent().ok_or("Executable has no directory")?;
    // Stage on the same volume as the destination before the running app exits.
    let sibling = tempfile::Builder::new()
        .prefix(".star-player-update-")
        .tempdir_in(parent)
        .map_err(|e| format!("Cannot write to the application folder: {e}"))?;
    for name in ["star-player.exe", "update.json", "update.json.sig"] {
        fs::copy(stage.join(name), sibling.path().join(name)).map_err(|e| e.to_string())?;
        File::options()
            .write(true)
            .open(sibling.path().join(name))
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    verify_stage(sibling.path())?;
    let helper = stage.join("update-helper.exe");
    fs::copy(&current, &helper).map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(helper);
    command
        .arg("--star-player-update-helper")
        .arg(std::process::id().to_string())
        .arg(&current)
        .arg(sibling.path());
    command.args(reopen);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .spawn()
        .map_err(|e| format!("Cannot launch update helper: {e}"))?;
    let _ = sibling.keep();
    Ok(())
}

/// Must run before initializing GPUI or opening user data.
pub fn run_helper() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .get(1)
        .is_none_or(|arg| arg != "--star-player-update-helper")
    {
        return false;
    }
    let result = (|| -> Result<(), String> {
        if !(5..=6).contains(&args.len()) {
            return Err("Invalid updater arguments".into());
        }
        let pid: u32 = args[2]
            .to_string_lossy()
            .parse()
            .map_err(|_| "Invalid parent PID")?;
        let target = PathBuf::from(&args[3]);
        let stage = PathBuf::from(&args[4]);
        verify_stage(&stage)?;
        wait_for_exit(pid)?;
        verify_stage(&stage)?;
        replace_and_launch(&target, &stage, |path| {
            std::process::Command::new(path)
                .args(args.get(5))
                .spawn()
                .map(|_| ())
        })
    })();
    if let Err(error) = result {
        crate::logging::Print(crate::logging::Level::Error, &error);
        #[cfg(windows)]
        rfd::MessageDialog::new()
            .set_title("StarPlayer update failed")
            .set_description(&error)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    true
}

fn replace_and_launch(
    target: &Path,
    stage: &Path,
    mut launch: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let backup = stage.join("previous.exe");
    fs::copy(target, &backup).map_err(|e| format!("Cannot preserve previous executable: {e}"))?;
    File::options()
        .write(true)
        .open(&backup)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("Cannot flush previous executable: {e}"))?;
    // Same-volume replacement leaves the original in place if rename fails.
    fs::rename(stage.join("star-player.exe"), target)
        .map_err(|e| format!("Cannot replace executable: {e}"))?;
    if let Err(error) = launch(target) {
        fs::rename(&backup, target).map_err(|e| {
            format!(
                "Relaunch failed: {error}; restore failed: {e}. Backup: {}",
                backup.display()
            )
        })?;
        let restored = launch(target);
        return Err(format!(
            "Relaunch failed; restored the previous executable: {error}. Previous version relaunch: {restored:?}"
        ));
    }
    // A created process is not proof of healthy startup. Keep the backup.
    Ok(())
}

#[cfg(windows)]
fn wait_for_exit(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, WaitForSingleObject},
    };
    unsafe {
        let handle = OpenProcess(0x00100000, 0, pid); // SYNCHRONIZE
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            // ERROR_INVALID_PARAMETER means the process already exited.
            return if error.raw_os_error() == Some(87) {
                Ok(())
            } else {
                Err(error.to_string())
            };
        }
        let result = WaitForSingleObject(handle, 120_000);
        CloseHandle(handle);
        if result != WAIT_OBJECT_0 {
            return Err("StarPlayer did not exit within two minutes; update not applied".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn wait_for_exit(_: u32) -> Result<(), String> {
    Err("Portable updates support Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn manifest() -> Manifest {
        Manifest {
            version: "999.0.0".into(),
            target: TARGET.into(),
            asset: "star-player-windows-x86_64.exe".into(),
            size: 3,
            sha256: format!("{:x}", Sha256::digest(b"abc")),
        }
    }

    #[test]
    fn signed_metadata_rejects_tampering_and_wrong_key() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let bytes = serde_json::to_vec(&manifest()).unwrap();
        let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
        let public = STANDARD.encode(key.verifying_key().to_bytes());
        assert!(verify_manifest(&bytes, signature.as_bytes(), &public).is_ok());
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(verify_manifest(&changed, signature.as_bytes(), &public).is_err());
        assert!(verify_manifest(&bytes, signature.as_bytes(), &STANDARD.encode([8; 32])).is_err());
    }

    #[test]
    fn rejects_wrong_target_downgrade_and_invalid_sizes() {
        let mut value = manifest();
        value.target = "aarch64-pc-windows-msvc".into();
        assert!(validate_manifest(&value).is_err());
        value = manifest();
        value.version = "0.0.0".into();
        assert!(validate_manifest(&value).is_err());
        value = manifest();
        value.size = MAX_BINARY + 1;
        assert!(validate_manifest(&value).is_err());
        value = manifest();
        value.version = "999.0.0-beta.1".into();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn verifies_download_size_and_digest() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"abc").unwrap();
        assert!(verify_file(file.path(), &manifest()).is_ok());
        fs::write(file.path(), b"abd").unwrap();
        assert!(verify_file(file.path(), &manifest()).is_err());
        fs::write(file.path(), b"ab").unwrap();
        assert!(verify_file(file.path(), &manifest()).is_err());
    }

    #[test]
    fn settings_default_on_for_existing_configs() {
        let config: crate::clip_store::Config = serde_json::from_str("{}").unwrap();
        assert!(config.updates.automatic_check && config.updates.automatic_download);
    }

    #[test]
    fn replacement_preserves_backup_and_restores_on_launch_failure() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("installed.exe");
        let stage = dir.path().join("stage");
        fs::create_dir(&stage).unwrap();
        fs::write(&target, b"old").unwrap();
        fs::write(stage.join("star-player.exe"), b"new").unwrap();
        replace_and_launch(&target, &stage, |path| {
            assert_eq!(fs::read(path).unwrap(), b"new");
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(stage.join("previous.exe")).unwrap(), b"old");
        fs::write(stage.join("star-player.exe"), b"broken").unwrap();
        let mut launches = 0;
        let result = replace_and_launch(&target, &stage, |path| {
            launches += 1;
            if launches == 1 {
                Err(std::io::Error::other("simulated launch failure"))
            } else {
                assert_eq!(fs::read(path).unwrap(), b"new");
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(launches, 2);
        assert_eq!(fs::read(target).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn locked_destination_leaves_old_executable_unchanged() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("installed.exe");
        let stage = dir.path().join("stage");
        fs::create_dir(&stage).unwrap();
        fs::write(&target, b"old").unwrap();
        fs::write(stage.join("star-player.exe"), b"new").unwrap();
        let _lock = File::options()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        assert!(replace_and_launch(&target, &stage, |_| panic!("must not launch")).is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn busy_states_ignore_repeated_checks() {
        let mut updater = Updater {
            state: State::Downloading(1, 2),
            ..Updater::default()
        };
        updater.check();
        assert!(matches!(updater.state, State::Downloading(1, 2)));
        updater.state = State::PreparingRestart;
        updater.check();
        assert!(matches!(updater.state, State::PreparingRestart));
    }

    #[test]
    fn startup_checks_immediately_and_respects_saved_preference() {
        let updater = Updater::default();
        let enabled = crate::clip_store::UpdateConfig::default();
        assert!(updater.should_check(&enabled, Instant::now()));
        let disabled = crate::clip_store::UpdateConfig {
            automatic_check: false,
            ..enabled
        };
        assert!(!updater.should_check(&disabled, Instant::now()));
    }

    #[test]
    fn automatic_checks_wait_for_interval_and_do_not_interrupt_updates() {
        let now = Instant::now();
        let config = crate::clip_store::UpdateConfig::default();
        let mut updater = Updater {
            last_check: Some(now),
            ..Updater::default()
        };
        assert!(!updater.should_check(&config, now));
        assert!(!updater.should_check(&config, now + INTERVAL - Duration::from_secs(1)));
        assert!(updater.should_check(&config, now + INTERVAL));
        for state in [
            State::Checking,
            State::Downloading(0, 1),
            State::Verifying,
            State::Ready(PathBuf::new(), "999.0.0".into()),
            State::PreparingRestart,
        ] {
            updater.state = state;
            assert!(!updater.should_check(&config, now + INTERVAL));
        }
    }
}
