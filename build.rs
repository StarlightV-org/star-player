use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const ARCHIVE_URL: &str = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261014/mpv-dev-x86_64-20261014-git-ec38361c56.7z";
const ARCHIVE_SHA256: &str = "325555b4e0b1d87fb7c92c96010776581599c64e4988052cb9d05e130b52f939";
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=licenses/THIRD_PARTY_NOTICES.md");
    println!("cargo:rerun-if-env-changed=SPACER_MPV_ARCHIVE");
    if let Err(error) = bundle() {
        panic!("Unable to embed Spacer's decoder: {error}");
    }
}

fn bundle() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is missing")?);
    if env::var("CARGO_CFG_TARGET_OS")? != "windows"
        || env::var("CARGO_CFG_TARGET_ARCH")? != "x86_64"
    {
        return Err("the self-contained decoder currently supports Windows x86_64 only".into());
    }
    let archive = if let Some(path) = env::var_os("SPACER_MPV_ARCHIVE") {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        path
    } else {
        let path = output.join("mpv.7z");
        if !path.is_file() {
            download(&path)?;
        }
        path
    };
    let digest = file_digest(&archive)?;
    if digest != ARCHIVE_SHA256 {
        return Err(format!("decoder archive checksum mismatch: expected {ARCHIVE_SHA256}, got {digest}. Remove {} and rebuild, or supply the pinned archive with SPACER_MPV_ARCHIVE", archive.display()).into());
    }
    let extracted = output.join("decoder-files");
    if extracted.exists() {
        fs::remove_dir_all(&extracted)?;
    }
    sevenz_rust2::decompress_file(&archive, &extracted)?;
    let mut files = Vec::new();
    collect(&extracted, &mut files)?;
    files.sort();
    let mut manifest = "pub const EMBEDDED_FILES: &[(&str, &[u8], &str)] = &[\n".to_owned();
    let mut bundle_digest = Sha256::new();
    bundle_digest.update(ARCHIVE_SHA256.as_bytes());
    let mut main_dll = None;
    let mut names = std::collections::HashSet::new();
    for file in files {
        let filename = file
            .file_name()
            .ok_or("invalid decoder filename")?
            .to_str()
            .ok_or("non-UTF8 decoder filename")?;
        let lower = filename.to_ascii_lowercase();
        // Embed every runtime DLL and the notices included by the upstream build.
        let is_dll = lower.ends_with(".dll");
        let is_notice =
            lower.ends_with(".txt") || lower.contains("license") || lower.contains("copying");
        if !is_dll && !is_notice {
            continue;
        }
        let destination = if is_dll {
            filename.to_owned()
        } else {
            format!(
                "notices/{}",
                file.strip_prefix(&extracted)?
                    .to_string_lossy()
                    .replace('\\', "/")
            )
        };
        if !names.insert(destination.to_ascii_lowercase()) {
            return Err(format!("duplicate bundled filename: {destination}").into());
        }
        if ["libmpv-2.dll", "mpv-2.dll"].contains(&lower.as_str()) {
            main_dll = Some(destination.clone());
        }
        let file_hash = file_digest(&file)?;
        bundle_digest.update(destination.as_bytes());
        bundle_digest.update(file_hash.as_bytes());
        manifest.push_str(&format!(
            "    ({destination:?}, include_bytes!({:?}), {file_hash:?}),\n",
            file.to_str().ok_or("non-UTF8 build path")?
        ));
    }
    let main_dll =
        main_dll.ok_or("the pinned archive does not contain libmpv-2.dll or mpv-2.dll")?;
    let notice =
        Path::new(&env::var("CARGO_MANIFEST_DIR")?).join("licenses/THIRD_PARTY_NOTICES.md");
    let notice_hash = file_digest(&notice)?;
    let notice_name = "notices/THIRD_PARTY_NOTICES.md";
    if !names.insert(notice_name.to_ascii_lowercase()) {
        return Err("duplicate third-party notice filename".into());
    }
    bundle_digest.update(notice_name.as_bytes());
    bundle_digest.update(notice_hash.as_bytes());
    manifest.push_str(&format!(
        "    ({notice_name:?}, include_bytes!({:?}), {notice_hash:?}),\n",
        notice.to_str().ok_or("non-UTF8 notice path")?
    ));
    // Version all resources together, so notice changes cannot invalidate an
    // existing cache that has the same decoder archive but an older manifest.
    let bundle_id = format!("{:x}", bundle_digest.finalize());
    manifest.push_str(&format!(
        "];\npub const DECODER_DLL: &str = {main_dll:?};\npub const BUNDLE_ID: &str = {bundle_id:?};\n"
    ));
    fs::write(output.join("decoder_assets.rs"), manifest)?;
    Ok(())
}

fn download(destination: &Path) -> Result<(), Box<dyn Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let mut response = client.get(ARCHIVE_URL).send()?.error_for_status()?;
    let partial = destination.with_extension("part");
    let mut file = fs::File::create(&partial)?;
    let copied = std::io::copy(&mut (&mut response).take(MAX_DOWNLOAD + 1), &mut file)?;
    file.flush()?;
    drop(file);
    if copied > MAX_DOWNLOAD {
        fs::remove_file(&partial)?;
        return Err("decoder archive exceeds the download size limit".into());
    }
    let digest = file_digest(&partial)?;
    if digest != ARCHIVE_SHA256 {
        fs::remove_file(&partial)?;
        return Err(format!("download checksum mismatch: {digest}").into());
    }
    fs::rename(partial, destination)?;
    Ok(())
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), files)?;
        } else if entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn file_digest(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
