//! Only the decoder embedded at build time is loaded. Nothing is found on PATH.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

include!(concat!(env!("OUT_DIR"), "/decoder_assets.rs"));

static EXTRACTION_LOCK: Mutex<()> = Mutex::new(());
static READY: OnceLock<PathBuf> = OnceLock::new();

/// Extract into the user's home, never alongside the executable or into PATH.
fn decoder_path() -> Result<PathBuf, String> {
    if let Some(path) = READY.get() {
        return Ok(path.clone());
    }
    let _guard = EXTRACTION_LOCK
        .lock()
        .map_err(|_| "Decoder extraction lock failed")?;
    if let Some(path) = READY.get() {
        return Ok(path.clone());
    }
    let home = dirs::home_dir().ok_or("Unable to locate the user's home directory")?;
    let root = home.join(".spacer").join("runtime");
    fs::create_dir_all(&root).map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
    let destination = root.join(BUNDLE_ID);
    install_at(&root, &destination)?;
    let dll = fs::canonicalize(destination.join(DECODER_DLL)).map_err(|e| e.to_string())?;
    let _ = READY.set(dll.clone());
    Ok(dll)
}

fn install_at(root: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return verify(destination).map_err(|e| {
            format!(
                "Bundled decoder cache is invalid: {e}. Remove {} and restart Spacer to restore it",
                destination.display()
            )
        });
    }
    // Build a complete directory before publishing it. Another process can win
    // the rename, but it can never make us load a partially extracted bundle.
    let staging = tempfile::Builder::new()
        .prefix("extract-")
        .tempdir_in(root)
        .map_err(|e| e.to_string())?;
    for &(name, bytes, _) in EMBEDDED_FILES {
        let relative = safe_relative(name)?;
        let path = staging.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(path, bytes).map_err(|e| e.to_string())?;
    }
    verify(staging.path())?;
    match fs::rename(staging.path(), destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.exists() => verify(destination),
        Err(error) => Err(format!("Unable to publish the bundled decoder: {error}")),
    }
}

fn safe_relative(name: &str) -> Result<&Path, String> {
    let path = Path::new(name);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("Invalid bundled resource path: {name}"));
    }
    Ok(path)
}

fn verify(directory: &Path) -> Result<(), String> {
    for &(name, _, expected) in EMBEDDED_FILES {
        let path = directory.join(safe_relative(name)?);
        if fs::symlink_metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .file_type()
            .is_symlink()
        {
            return Err(format!("Bundled resource is a symlink: {}", path.display()));
        }
        let mut file = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != expected {
            return Err(format!("Checksum mismatch for {}", path.display()));
        }
    }
    Ok(())
}

pub fn load_library() -> Result<libloading::Library, String> {
    let path = decoder_path()?;
    crate::logging::Print(
        crate::logging::Level::Info,
        format!("Loading verified bundled decoder {}", path.display()),
    );
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
        };
        // SAFETY: this absolute path contains the verified embedded build. The
        // flags restrict dependent DLLs to its directory and Windows system32.
        unsafe {
            Library::load_with_flags(
                &path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Into::into)
        .map_err(|e| format!("Unable to load bundled decoder {}: {e}", path.display()))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("The self-contained decoder currently supports Windows x86_64 only".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{EMBEDDED_FILES, install_at, safe_relative, verify};
    #[test]
    fn rejects_resource_path_traversal() {
        assert!(safe_relative("../decoder.dll").is_err());
        assert!(safe_relative("C:/decoder.dll").is_err());
        assert!(safe_relative("").is_err());
        assert!(safe_relative("notices/license.txt").is_ok());
    }
    #[test]
    fn extracts_verified_bundle_and_rejects_modified_cache() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("bundle");
        install_at(root.path(), &destination).unwrap();
        verify(&destination).unwrap();
        install_at(root.path(), &destination).unwrap();
        std::fs::write(destination.join(EMBEDDED_FILES[0].0), b"modified").unwrap();
        assert!(verify(&destination).is_err());
        assert!(install_at(root.path(), &destination).is_err());
    }
}
