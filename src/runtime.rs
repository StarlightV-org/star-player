//! Only the decoder embedded at build time is loaded. Nothing is found on PATH.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

struct EmbeddedFile {
    name: &'static str,
    compressed: &'static [u8],
    sha256: &'static str,
    size: u64,
}

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
    #[cfg(not(test))]
    let root = crate::clip_store::directory()?.join("runtime");
    // Native decoder tests must not migrate or write the user's real profile.
    #[cfg(test)]
    let root = {
        static TEST_DATA: OnceLock<tempfile::TempDir> = OnceLock::new();
        TEST_DATA
            .get_or_init(|| tempfile::tempdir().expect("Temporary decoder test cache"))
            .path()
            .join("runtime")
    };
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
                "Bundled decoder cache is invalid: {e}. Remove {} and restart StarPlayer to restore it",
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
    for resource in EMBEDDED_FILES {
        let relative = safe_relative(resource.name)?;
        let path = staging.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut output = BufWriter::new(file);
        decompress_resource(resource.compressed, resource.size, &mut output)
            .map_err(|e| format!("Cannot extract {}: {e}", resource.name))?;
        output.flush().map_err(|e| e.to_string())?;
    }
    verify(staging.path())?;
    match fs::rename(staging.path(), destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.exists() => verify(destination),
        Err(error) => Err(format!("Unable to publish the bundled decoder: {error}")),
    }
}

fn decompress_resource(
    compressed: &[u8],
    expected_size: u64,
    output: &mut impl Write,
) -> std::io::Result<()> {
    let limit = expected_size.checked_add(1).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid resource size")
    })?;
    let decoder = zstd::stream::read::Decoder::with_buffer(compressed)?;
    // Stream to disk rather than allocating the entire DLL. Bound output even
    // if the compressed frame is damaged or has an incorrect declared size.
    let copied = std::io::copy(&mut decoder.take(limit), output)?;
    if copied != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Resource size mismatch: expected {expected_size}, got {copied}"),
        ));
    }
    Ok(())
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
    for resource in EMBEDDED_FILES {
        let path = directory.join(safe_relative(resource.name)?);
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Bundled resource is a symlink: {}", path.display()));
        }
        if metadata.len() != resource.size {
            return Err(format!(
                "Bundled resource size mismatch: {}",
                path.display()
            ));
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
        if format!("{:x}", digest.finalize()) != resource.sha256 {
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
    use super::{EMBEDDED_FILES, decompress_resource, install_at, safe_relative, verify};
    use std::io::Write;
    #[test]
    fn decompresses_resource_with_exact_size() {
        let bytes = b"decoder resource test";
        let compressed = zstd::stream::encode_all(&bytes[..], 3).unwrap();
        let mut output = Vec::new();
        decompress_resource(&compressed, bytes.len() as u64, &mut output).unwrap();
        assert_eq!(output, bytes);
    }

    #[test]
    fn decompresses_empty_resource_and_propagates_write_errors() {
        let compressed = zstd::stream::encode_all(&b""[..], 3).unwrap();
        let mut output = Vec::new();
        decompress_resource(&compressed, 0, &mut output).unwrap();
        assert!(output.is_empty());

        let compressed = zstd::stream::encode_all(&b"decoder"[..], 3).unwrap();
        // A fixed empty slice cannot accept output, like a failed disk write.
        let mut full_output = &mut [][..];
        assert!(decompress_resource(&compressed, 7, &mut full_output).is_err());
    }

    #[test]
    fn rejects_damaged_compressed_resources_and_wrong_sizes() {
        let bytes = b"decoder resource test";
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut bad_checksum = compressed.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        assert!(decompress_resource(&bad_checksum, bytes.len() as u64, &mut Vec::new()).is_err());
        for damaged in [&compressed[..compressed.len() - 1], &b"invalid frame"[..]] {
            assert!(decompress_resource(damaged, bytes.len() as u64, &mut Vec::new()).is_err());
        }
        for size in [0, bytes.len() as u64 - 1, bytes.len() as u64 + 1, u64::MAX] {
            let mut output = Vec::new();
            assert!(decompress_resource(&compressed, size, &mut output).is_err());
            assert!(output.len() as u64 <= size.saturating_add(1));
        }
    }
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
        // Cached launches must not rewrite or decompress the existing DLL.
        let dll = destination.join(EMBEDDED_FILES[0].name);
        let modified = std::fs::metadata(&dll).unwrap().modified().unwrap();
        install_at(root.path(), &destination).unwrap();
        assert_eq!(
            std::fs::metadata(&dll).unwrap().modified().unwrap(),
            modified
        );
        // Change one byte without changing the size to exercise SHA-256 checks.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&dll)
            .unwrap()
            .write_all(b"!")
            .unwrap();
        assert!(verify(&destination).is_err());
        assert!(install_at(root.path(), &destination).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn extracted_decoder_loads_and_exports_mpv_api() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("bundle");
        install_at(root.path(), &destination).unwrap();
        let dll = std::fs::canonicalize(destination.join(super::DECODER_DLL)).unwrap();
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
        };
        // SAFETY: load only the verified decoder from this test's private cache.
        unsafe {
            let library = Library::load_with_flags(
                &dll,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
            .unwrap();
            let version = library
                .get::<unsafe extern "C" fn() -> std::os::raw::c_ulong>(b"mpv_client_api_version\0")
                .unwrap();
            assert!(version() > 0);
        }
    }
}
