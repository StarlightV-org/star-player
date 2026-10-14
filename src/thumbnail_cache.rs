//! Only generated, regular JPEG cache files belong to this budget.
use std::{fs, path::Path, time::SystemTime};

pub const LIMIT: u64 = 2_000_000;

pub fn prune(directory: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name
            .strip_suffix(".jpg")
            .is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        total = total.saturating_add(metadata.len());
        files.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            entry.path(),
            metadata.len(),
        ));
    }
    files.sort();
    let mut errors = Vec::new();
    for (_, path, size) in files {
        if total <= LIMIT {
            break;
        }
        match fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(size),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

pub fn save(path: &Path, image: &image::RgbaImage) -> Result<(), String> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image.clone())
        .into_rgb8()
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    if bytes.get_ref().len() as u64 <= LIMIT {
        crate::clip_store::atomic_write(path, bytes.get_ref())?;
    }
    prune(path.parent().ok_or("Missing cache directory")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicts_oldest_and_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(format!("{}.jpg", "a".repeat(64)));
        fs::write(&old, vec![0; 1_200_000]).unwrap();
        let new = dir.path().join(format!("{}.jpg", "b".repeat(64)));
        fs::write(&new, vec![0; 1_200_000]).unwrap();
        fs::write(dir.path().join("keep.txt"), b"keep").unwrap();
        prune(dir.path()).unwrap();
        assert!(!old.exists());
        assert!(new.exists());
        assert!(dir.path().join("keep.txt").exists());
    }
    #[test]
    fn removes_a_single_oversized_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.jpg", "a".repeat(64)));
        fs::write(&path, vec![0; LIMIT as usize + 1]).unwrap();
        prune(dir.path()).unwrap();
        assert!(!path.exists());
    }
}
