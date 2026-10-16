//! Persistent folder icons, independent of the evictable video thumbnail cache.
use crate::{clip_store, steam_games};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

pub const CUSTOM_ICON_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "ico", "exe"];

fn cache_custom(dir: &Path, source: &Path) -> Result<String, String> {
    let allowed = source
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|extension| {
            CUSTOM_ICON_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        });
    if !allowed {
        return Err("Choose a PNG, JPEG, ICO, or EXE file".into());
    }
    // Extract resource index 0 for executables. This reads resources, never
    // launches the selected program, and publishes the resulting cached PNG.
    cache_source(dir, source, 0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub name: String,
    pub image: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Override {
    Custom(String),
    Game(u32),
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Assignment {
    pub assigned: Option<u32>,
    pub override_icon: Option<Override>,
}
#[derive(Clone, Default)]
pub struct State {
    pub games: BTreeMap<u32, Game>,
    pub folders: BTreeMap<String, Assignment>,
}
impl State {
    pub fn image(&self, folder: &Path) -> Option<&str> {
        let assignment = self.folders.get(&folder_key(folder))?;
        match &assignment.override_icon {
            Some(Override::Custom(image)) => Some(image),
            Some(Override::Game(id)) => self.games.get(id)?.image.as_deref(),
            None => self.games.get(&assignment.assigned?)?.image.as_deref(),
        }
    }
    pub fn has_override(&self, folder: &Path) -> bool {
        self.folders
            .get(&folder_key(folder))
            .is_some_and(|a| a.override_icon.is_some())
    }
    fn match_folders(&mut self, folders: &[PathBuf]) -> bool {
        let mut changed = false;
        for folder in folders {
            let key = folder_key(folder);
            if self.folders.get(&key).is_some_and(|a| a.assigned.is_some()) {
                continue;
            }
            let name = normalize(&folder.file_name().unwrap_or_default().to_string_lossy());
            if name.is_empty() {
                continue;
            }
            let mut matches = self
                .games
                .iter()
                .filter(|(_, game)| normalize(&game.name) == name);
            if let Some((&id, _)) = matches.next()
                && matches.next().is_none()
            {
                self.folders.entry(key).or_default().assigned = Some(id);
                changed = true;
            }
        }
        changed
    }
}

pub fn folder_key(path: &Path) -> String {
    // No canonicalization here: disconnected drives must retain their identity.
    let mut key = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = key.strip_prefix("\\\\?\\") {
        key = rest.to_owned();
    }
    #[cfg(windows)]
    {
        key = key.to_lowercase();
    }
    key.trim_end_matches('\\').to_owned()
}
fn normalize(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Serialize, Deserialize)]
struct CatalogFile {
    version: u32,
    games: BTreeMap<u32, Game>,
}
#[derive(Serialize, Deserialize)]
struct FoldersFile {
    version: u32,
    folders: BTreeMap<String, Assignment>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}. File left unchanged.", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}
fn load(dir: &Path) -> Result<State, String> {
    let catalog = read_json::<CatalogFile>(&dir.join("catalog.json"))?;
    let folders = read_json::<FoldersFile>(&dir.join("folders.json"))?;
    if catalog.as_ref().is_some_and(|c| c.version != 1)
        || folders.as_ref().is_some_and(|f| f.version != 1)
    {
        return Err("Unsupported game icon data version. Files left unchanged.".into());
    }
    let state = State {
        games: catalog.map(|c| c.games).unwrap_or_default(),
        folders: folders.map(|f| f.folders).unwrap_or_default(),
    };
    if state
        .games
        .values()
        .filter_map(|g| g.image.as_deref())
        .any(|s| !valid_hash(s))
        || state
            .folders
            .values()
            .any(|a| matches!(&a.override_icon, Some(Override::Custom(s)) if !valid_hash(s)))
    {
        return Err("Invalid game icon image reference. Files left unchanged.".into());
    }
    Ok(state)
}
fn save_catalog(dir: &Path, state: &State) -> Result<(), String> {
    clip_store::atomic_write(
        &dir.join("catalog.json"),
        &serde_json::to_vec_pretty(&CatalogFile {
            version: 1,
            games: state.games.clone(),
        })
        .map_err(|e| e.to_string())?,
    )
}
fn save_folders(dir: &Path, state: &State) -> Result<(), String> {
    clip_store::atomic_write(
        &dir.join("folders.json"),
        &serde_json::to_vec_pretty(&FoldersFile {
            version: 1,
            folders: state.folders.clone(),
        })
        .map_err(|e| e.to_string())?,
    )
}

fn decode(path: &Path) -> Result<image::RgbaImage, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("Icon file exceeds 16 MB".into());
    }
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Ico)
    ) {
        return Err("Choose a PNG, JPEG, or ICO image".into());
    }
    let format = reader.format();
    reader.limits(image_limits());
    let decoded = match reader.decode() {
        Ok(image) => image,
        Err(error) if format == Some(image::ImageFormat::Ico) => {
            // image's ICO decoder requires embedded PNGs to already be RGBA.
            // Steam also ships valid RGB/indexed PNG frames. Decode those as
            // ordinary PNGs, then normalize their channels ourselves.
            let mut bytes = Vec::new();
            fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            decode_ico_png(&bytes)
                .map_err(|fallback| format!("{error}; embedded PNG fallback: {fallback}"))?
        }
        Err(error) => return Err(error.to_string()),
    };
    let width = decoded.width().min(128);
    let height = decoded.height().min(128);
    Ok(decoded.thumbnail(width, height).into_rgba8())
}

fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits
}

fn decode_ico_png(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Icon file exceeds 16 MB".into());
    }
    if bytes.get(..4) != Some(&[0, 0, 1, 0]) {
        return Err("Invalid ICO header".into());
    }
    let count = bytes
        .get(4..6)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .ok_or("Incomplete ICO header")?;
    if count == 0 || count > 256 {
        return Err("Invalid ICO frame count".into());
    }
    let directory_end = 6 + count * 16;
    if bytes.len() < directory_end {
        return Err("Incomplete ICO directory".into());
    }
    let mut frames = Vec::new();
    for entry in bytes[6..directory_end].chunks_exact(16) {
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        if offset < directory_end {
            continue;
        }
        let Some(end) = offset.checked_add(length) else {
            continue;
        };
        let Some(payload) = bytes.get(offset..end) else {
            continue;
        };
        if !payload.starts_with(b"\x89PNG\r\n\x1a\n") {
            continue;
        }
        let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
        let height = if entry[1] == 0 { 256 } else { entry[1] as u32 };
        let depth = u16::from_le_bytes([entry[6], entry[7]]);
        frames.push((width * height, depth, payload));
    }
    frames.sort_by_key(|(area, depth, _)| std::cmp::Reverse((*area, *depth)));
    for (_, _, payload) in frames {
        let mut reader =
            image::ImageReader::with_format(Cursor::new(payload), image::ImageFormat::Png);
        reader.limits(image_limits());
        if let Ok(image) = reader.decode() {
            return Ok(image);
        }
    }
    Err("No usable embedded PNG frame".into())
}
fn cache_pixels(dir: &Path, pixels: image::RgbaImage) -> Result<String, String> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let bytes = bytes.into_inner();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    // Atomic replacement also repairs a damaged existing cache file.
    clip_store::atomic_write(&dir.join("images").join(format!("{hash}.png")), &bytes)?;
    Ok(hash)
}
fn cache_source(dir: &Path, path: &Path, index: i32) -> Result<String, String> {
    let pixels = if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("exe") || s.eq_ignore_ascii_case("dll"))
    {
        executable_icon(path, index)?
    } else {
        decode(path)?
    };
    cache_pixels(dir, pixels)
}
fn cached_image(dir: &Path, hash: &str) -> Result<image::RgbaImage, String> {
    if !valid_hash(hash) {
        return Err("Invalid icon reference".into());
    }
    decode(&dir.join("images").join(format!("{hash}.png")))
}

pub enum Request {
    /// Requires saved Steam discovery permission. Never sent automatically at startup.
    Detect(Option<PathBuf>),
    Folders(Vec<PathBuf>),
    Custom(PathBuf, PathBuf),
    Choose(PathBuf, u32),
    Restore(PathBuf),
    Image(String),
    Shutdown,
}
pub enum Event {
    State(State),
    Image(String, Option<image::RgbaImage>),
    Detected(String),
    Warning(String),
    Error(String),
}
pub struct Worker {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self::with_shortcuts(directory, None)
    }
    fn with_shortcuts(directory: Option<PathBuf>, shortcut_dirs: Option<Vec<PathBuf>>) -> Self {
        let (tx, requests) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let directory = directory.map(Ok).unwrap_or_else(clip_store::directory);
            match directory {
                Ok(directory) => run(
                    &directory.join("game-icons"),
                    requests,
                    events,
                    shortcut_dirs,
                ),
                Err(e) => {
                    let _ = events.send(Event::Error(e));
                }
            }
        });
        Self {
            tx,
            rx,
            thread: Some(thread),
        }
    }
    pub fn send(&self, request: Request) {
        let _ = self.tx.send(request);
    }
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.rx.try_iter()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn publish_scan(
    dir: &Path,
    state: &mut State,
    next: &mut State,
    folders: &[PathBuf],
    events: &mpsc::Sender<Event>,
) -> Result<(), String> {
    save_catalog(dir, next)?;
    state.games = next.games.clone();
    let saved = if next.match_folders(folders) {
        save_folders(dir, next)
    } else {
        Ok(())
    };
    if saved.is_ok() {
        *state = next.clone();
    }
    // Even if saving assignments fails, the successfully saved catalog is usable.
    let _ = events.send(Event::State(state.clone()));
    saved
}

fn run(
    dir: &Path,
    requests: mpsc::Receiver<Request>,
    events: mpsc::Sender<Event>,
    shortcut_dirs: Option<Vec<PathBuf>>,
) {
    let loaded = load(dir);
    let valid = loaded.is_ok();
    let mut state = loaded.unwrap_or_else(|e| {
        let _ = events.send(Event::Error(e));
        State::default()
    });
    let _ = events.send(Event::State(state.clone()));
    let mut folders = Vec::new();
    for request in requests {
        if matches!(request, Request::Shutdown) {
            break;
        }
        if let Request::Image(hash) = request {
            let pixels = cached_image(dir, &hash).ok();
            let _ = events.send(Event::Image(hash, pixels));
            continue;
        }
        if !valid {
            let _ = events.send(Event::Error(
                "Repair game icon JSON files before changing icons.".into(),
            ));
            continue;
        }
        let result = (|| {
            let mut next = state.clone();
            match request {
                Request::Detect(root) => {
                    let store = dir.parent().ok_or("Missing icon store parent")?;
                    if clip_store::read_config(store)?.steam_discovery
                        != clip_store::Discovery::Enabled
                    {
                        return Err("Approve local Steam game searches in Settings first.".into());
                    }
                    let root = root.map(Ok).unwrap_or_else(steam_games::locate)?;
                    let directories = shortcut_dirs
                        .clone()
                        .unwrap_or_else(steam_games::shortcut_directories);
                    let scan = steam_games::scan(&root, &directories)?;
                    let count = scan.games.len();
                    let mut warnings = scan.warnings;
                    let mut copied = 0;
                    // Include all names before matching any folders. A matching
                    // title in a later batch must still make an earlier one ambiguous.
                    for game in &scan.games {
                        next.games
                            .entry(game.id)
                            .and_modify(|cached| cached.name = game.name.clone())
                            .or_insert(Game {
                                name: game.name.clone(),
                                image: None,
                            });
                    }
                    for (index, game) in scan.games.into_iter().enumerate() {
                        let mut image = next.games.get(&game.id).and_then(|g| g.image.clone());
                        for (path, index) in game.icons {
                            match cache_source(dir, &path, index) {
                                Ok(hash) => {
                                    image = Some(hash);
                                    copied += 1;
                                    break;
                                }
                                Err(e) => warnings.push(format!("{}: {e}", path.display())),
                            }
                        }
                        next.games.insert(
                            game.id,
                            Game {
                                name: game.name,
                                image,
                            },
                        );
                        // Publish small batches so found icons appear while the
                        // rest of the search runs, without writing JSON per game.
                        if index % 8 == 7 {
                            publish_scan(dir, &mut state, &mut next, &folders, &events)?;
                        }
                    }
                    publish_scan(dir, &mut state, &mut next, &folders, &events)?;
                    for warning in &warnings {
                        crate::logging::Print(crate::logging::Level::Warn, warning);
                    }
                    if !warnings.is_empty() {
                        let details = warnings
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        let remaining = warnings.len().saturating_sub(3);
                        let extra = if remaining > 0 {
                            format!("\n{remaining} additional warnings were logged.")
                        } else {
                            String::new()
                        };
                        let _ = events.send(Event::Warning(format!("{} metadata or image warnings. Other games are still usable.\n{details}{extra}", warnings.len())));
                    }
                    let _ = events.send(Event::Detected(format!("Detected {count} Steam games; copied {copied} icons. {} metadata or image warnings. Games without local artwork keep the folder icon.", warnings.len())));
                }
                Request::Folders(paths) => {
                    folders = paths;
                    if !next.match_folders(&folders) {
                        return Ok(());
                    }
                    save_folders(dir, &next)?;
                }
                Request::Custom(folder, source) => {
                    if !folder.is_absolute() {
                        return Err("Icon folder must be absolute".into());
                    }
                    let hash = cache_custom(dir, &source)?;
                    next.folders
                        .entry(folder_key(&folder))
                        .or_default()
                        .override_icon = Some(Override::Custom(hash));
                    save_folders(dir, &next)?;
                }
                Request::Choose(folder, id) => {
                    let hash = next
                        .games
                        .get(&id)
                        .and_then(|g| g.image.as_deref())
                        .ok_or("This game has no cached icon")?;
                    cached_image(dir, hash).map_err(|_| "This game's cached icon is missing or damaged. Detect it again or choose a custom image.".to_owned())?;
                    next.folders
                        .entry(folder_key(&folder))
                        .or_default()
                        .override_icon = Some(Override::Game(id));
                    save_folders(dir, &next)?;
                }
                Request::Restore(folder) => {
                    if let Some(assignment) = next.folders.get_mut(&folder_key(&folder)) {
                        assignment.override_icon = None;
                    }
                    save_folders(dir, &next)?;
                }
                Request::Image(_) | Request::Shutdown => unreachable!(),
            }
            state = next;
            let _ = events.send(Event::State(state.clone()));
            Ok::<_, String>(())
        })();
        if let Err(e) = result {
            let _ = events.send(Event::Error(e));
        }
    }
}

#[cfg(not(windows))]
fn executable_icon(_: &Path, _: i32) -> Result<image::RgbaImage, String> {
    Err("Executable icons require Windows".into())
}

#[cfg(windows)]
fn executable_icon(path: &Path, index: i32) -> Result<image::RgbaImage, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Graphics::Gdi::*,
        UI::{
            Shell::ExtractIconExW,
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO},
        },
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        let mut icon = std::ptr::null_mut();
        if ExtractIconExW(path.as_ptr(), index, &mut icon, std::ptr::null_mut(), 1) == 0
            || icon.is_null()
        {
            return Err("No executable icon available".into());
        }
        let mut info: ICONINFO = std::mem::zeroed();
        let result = (|| {
            if GetIconInfo(icon, &mut info) == 0 || info.hbmColor.is_null() {
                return Err("No color icon available".into());
            }
            let mut bitmap: BITMAP = std::mem::zeroed();
            if GetObjectW(
                info.hbmColor,
                std::mem::size_of::<BITMAP>() as i32,
                (&mut bitmap as *mut BITMAP).cast(),
            ) == 0
            {
                return Err("Cannot read executable icon".into());
            }
            let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);
            if width <= 0 || height <= 0 || width > 256 || height > 256 {
                return Err("Invalid executable icon size".into());
            }
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err("Cannot create icon decoder context".into());
            }
            let mut header: BITMAPINFO = std::mem::zeroed();
            header.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            header.bmiHeader.biWidth = width;
            header.bmiHeader.biHeight = -height;
            header.bmiHeader.biPlanes = 1;
            header.bmiHeader.biBitCount = 32;
            header.bmiHeader.biCompression = BI_RGB;
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            let read = GetDIBits(
                dc,
                info.hbmColor,
                0,
                height as u32,
                pixels.as_mut_ptr().cast(),
                &mut header,
                DIB_RGB_COLORS,
            );
            let mut mask = vec![0u8; pixels.len()];
            let mask_read = if !info.hbmMask.is_null() {
                GetDIBits(
                    dc,
                    info.hbmMask,
                    0,
                    height as u32,
                    mask.as_mut_ptr().cast(),
                    &mut header,
                    DIB_RGB_COLORS,
                )
            } else {
                0
            };
            DeleteDC(dc);
            if read != height {
                return Err("Cannot decode executable icon pixels".into());
            }
            let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
            for (p, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                p.swap(0, 2);
                if !has_alpha {
                    p[3] = if mask_read == height && m[0] != 0 {
                        0
                    } else {
                        255
                    };
                } else if p[3] > 0 {
                    // Windows icon color bitmaps use premultiplied alpha.
                    for c in 0..3 {
                        p[c] = ((p[c] as u32 * 255) / p[3] as u32).min(255) as u8;
                    }
                }
            }
            image::RgbaImage::from_raw(width as u32, height as u32, pixels)
                .ok_or("Invalid icon pixels".into())
        })();
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
        DestroyIcon(icon);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_is_unique_and_manual_choices_survive() {
        let mut state = State::default();
        state.games.insert(
            1,
            Game {
                name: "Counter-Strike 2".into(),
                image: None,
            },
        );
        let folder = PathBuf::from("C:/Clips/Counter Strike 2");
        let nested = PathBuf::from("C:/Clips/Counter Strike 2/Highlights");
        state.folders.insert(
            folder_key(&folder),
            Assignment {
                assigned: None,
                override_icon: Some(Override::Game(2)),
            },
        );
        assert!(state.match_folders(std::slice::from_ref(&folder)));
        assert_eq!(state.folders[&folder_key(&folder)].assigned, Some(1));
        assert!(matches!(
            state.folders[&folder_key(&folder)].override_icon,
            Some(Override::Game(2))
        ));
        assert!(!state.folders.contains_key(&folder_key(&nested)));
        state.games.insert(
            2,
            Game {
                name: "Counter Strike 2".into(),
                image: None,
            },
        );
        let other = PathBuf::from("D:/Clips/Counter Strike 2");
        assert!(!state.match_folders(&[other]));
        assert!(!state.match_folders(&[folder]));
    }
    #[test]
    fn cache_survives_source_removal_and_restore() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("game-icons");
        let source = temp.path().join("source.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]))
            .save(&source)
            .unwrap();
        let hash = cache_source(&dir, &source, 0).unwrap();
        fs::remove_file(&source).unwrap();
        assert!(cached_image(&dir, &hash).is_ok());
        let mut state = State::default();
        state.games.insert(
            1,
            Game {
                name: "Game".into(),
                image: Some(hash.clone()),
            },
        );
        let folder = temp.path().join("Game");
        state.folders.insert(
            folder_key(&folder),
            Assignment {
                assigned: Some(1),
                override_icon: Some(Override::Custom("a".repeat(64))),
            },
        );
        save_catalog(&dir, &state).unwrap();
        save_folders(&dir, &state).unwrap();
        let mut loaded = load(&dir).unwrap();
        assert_eq!(loaded.image(&folder), Some("a".repeat(64).as_str()));
        loaded
            .folders
            .get_mut(&folder_key(&folder))
            .unwrap()
            .override_icon = None;
        assert_eq!(loaded.image(&folder), Some(hash.as_str()));
    }
    #[test]
    fn invalid_store_and_images_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("catalog.json"), b"broken").unwrap();
        assert!(load(temp.path()).is_err());
        assert_eq!(
            fs::read(temp.path().join("catalog.json")).unwrap(),
            b"broken"
        );
        assert!(cached_image(temp.path(), "../outside").is_err());
        let path = temp.path().join("bad.png");
        fs::write(&path, b"not an image").unwrap();
        assert!(decode(&path).is_err());
        assert!(cache_pixels(&path, image::RgbaImage::new(1, 1)).is_err());
    }

    #[test]
    fn custom_icons_reject_unlisted_extensions_even_with_valid_image_contents() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("cache");
        let png = encoded_png(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(8, 8, image::Rgba([12, 34, 56, 255])),
        ));
        for extension in ["bmp", "webp", "gif", "svg", "dll", "txt", ""] {
            let source = temp.path().join(format!("custom.{extension}"));
            fs::write(&source, &png).unwrap();
            assert!(
                cache_custom(&store, &source)
                    .unwrap_err()
                    .contains("PNG, JPEG, ICO, or EXE")
            );
        }
        assert!(!store.exists());
        for extension in ["png", "PNG", "jpg", "JPEG", "ico", "ICO"] {
            let source = temp.path().join(format!("custom.{extension}"));
            fs::write(&source, &png).unwrap();
            assert!(cache_custom(&store, &source).is_ok());
        }
    }

    #[cfg(windows)]
    #[test]
    fn custom_executable_icon_is_extracted_and_persists_without_the_executable() {
        let temp = tempfile::tempdir().unwrap();
        let system = std::env::current_exe().unwrap();
        // The test executable includes StarPlayer's embedded application icon.
        // ExtractIconEx reads its resources; no process is started.
        let exe = temp.path().join("selected.EXE");
        fs::copy(system, &exe).unwrap();
        let expected = executable_icon(&exe, 0).unwrap();
        let folder = temp.path().join("Clips/Game");
        let store = temp.path().join("store");
        let worker = Worker::with_shortcuts(Some(store.clone()), Some(Vec::new()));
        let _ = receive_state(&worker);
        worker.send(Request::Custom(folder.clone(), exe.clone()));
        let state = receive_state(&worker);
        let hash = state.image(&folder).unwrap().to_owned();
        fs::remove_file(exe).unwrap();
        assert_eq!(
            cached_image(&store.join("game-icons"), &hash).unwrap(),
            expected
        );
        drop(worker);
        let worker = Worker::with_shortcuts(Some(store), Some(Vec::new()));
        assert_eq!(receive_state(&worker).image(&folder), Some(hash.as_str()));
    }
    #[test]
    fn worker_startup_does_not_discover_steam() {
        let temp = tempfile::tempdir().unwrap();
        let worker = Worker::new(Some(temp.path().to_owned()));
        let event = worker
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(matches!(event, Event::State(state) if state.games.is_empty()));
        assert!(!temp.path().join("game-icons").exists());
        assert!(worker.rx.try_recv().is_err());
    }

    fn receive_state(worker: &Worker) -> State {
        loop {
            match worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
            {
                Event::State(state) => return state,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    }
    fn finish_scan(worker: &Worker) -> State {
        loop {
            match worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
            {
                Event::Detected(_) => return receive_state(worker),
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    }
    #[test]
    fn worker_scan_overrides_uninstall_and_restart() {
        let temp = tempfile::tempdir().unwrap();
        let steam = temp.path().join("Steam");
        let store = temp.path().join("store");
        let folder = temp.path().join("Clips/First Game");
        let no_match = temp.path().join("Clips/Unmatched");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::create_dir_all(steam.join("appcache/librarycache")).unwrap();
        for (id, name, color) in [
            (1, "First Game", [255, 0, 0, 255]),
            (2, "Second Game", [0, 255, 0, 255]),
        ] {
            fs::write(
                steam.join(format!("steamapps/appmanifest_{id}.acf")),
                format!("AppState {{ appid {id} name \"{name}\" }}"),
            )
            .unwrap();
            image::RgbaImage::from_pixel(16, 16, image::Rgba(color))
                .save(steam.join(format!("appcache/librarycache/{id}_icon.png")))
                .unwrap();
        }
        clip_store::save_config(
            &store,
            &clip_store::Config {
                steam_discovery: clip_store::Discovery::Enabled,
                ..Default::default()
            },
        )
        .unwrap();
        let worker = Worker::with_shortcuts(Some(store.clone()), Some(Vec::new()));
        assert!(receive_state(&worker).games.is_empty());
        worker.send(Request::Folders(vec![folder.clone(), no_match.clone()]));
        worker.send(Request::Detect(Some(steam.clone())));
        let assigned = finish_scan(&worker);
        let original = assigned.image(&folder).unwrap().to_owned();
        assert!(assigned.image(&no_match).is_none());
        worker.send(Request::Choose(folder.clone(), 2));
        let selected = receive_state(&worker);
        assert_eq!(selected.image(&folder), selected.games[&2].image.as_deref());
        assert_ne!(selected.image(&folder), Some(original.as_str()));

        let custom = temp.path().join("custom.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 255, 255]))
            .save(&custom)
            .unwrap();
        worker.send(Request::Custom(folder.clone(), custom.clone()));
        let custom_state = receive_state(&worker);
        let custom_hash = custom_state.image(&folder).unwrap().to_owned();
        fs::remove_file(custom).unwrap();
        for id in [1, 2] {
            fs::remove_file(steam.join(format!("steamapps/appmanifest_{id}.acf"))).unwrap();
            fs::remove_file(steam.join(format!("appcache/librarycache/{id}_icon.png"))).unwrap();
        }
        worker.send(Request::Detect(Some(steam)));
        let uninstalled = finish_scan(&worker);
        assert_eq!(uninstalled.games.len(), 2);
        assert_eq!(uninstalled.image(&folder), Some(custom_hash.as_str()));
        drop(worker);
        let worker = Worker::with_shortcuts(Some(store), Some(Vec::new()));
        assert_eq!(
            receive_state(&worker).image(&folder),
            Some(custom_hash.as_str())
        );
        worker.send(Request::Restore(folder.clone()));
        assert_eq!(
            receive_state(&worker).image(&folder),
            Some(original.as_str())
        );
        worker.send(Request::Choose(no_match.clone(), 2));
        assert!(receive_state(&worker).image(&no_match).is_some());
        worker.send(Request::Restore(no_match.clone()));
        assert!(receive_state(&worker).image(&no_match).is_none());
    }
    #[test]
    fn worker_refuses_to_overwrite_invalid_store() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("game-icons");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("folders.json"), b"broken").unwrap();
        let worker = Worker::with_shortcuts(Some(temp.path().to_owned()), Some(Vec::new()));
        assert!(matches!(
            worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Event::Error(_)
        ));
        let _ = receive_state(&worker);
        worker.send(Request::Restore(temp.path().join("Game")));
        assert!(matches!(
            worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Event::Error(_)
        ));
        assert_eq!(fs::read(dir.join("folders.json")).unwrap(), b"broken");
    }

    #[test]
    fn searches_require_saved_permission_and_respect_revocation() {
        let temp = tempfile::tempdir().unwrap();
        let worker = Worker::with_shortcuts(Some(temp.path().to_owned()), Some(Vec::new()));
        let _ = receive_state(&worker);
        let invalid_root = temp.path().join("Missing Steam");
        worker.send(Request::Detect(Some(invalid_root.clone())));
        assert!(
            matches!(worker.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), Event::Error(e) if e.contains("Approve"))
        );
        assert!(!temp.path().join("game-icons").exists());
        let mut config = clip_store::Config {
            steam_discovery: clip_store::Discovery::Enabled,
            ..Default::default()
        };
        clip_store::save_config(temp.path(), &config).unwrap();
        worker.send(Request::Detect(Some(invalid_root.clone())));
        assert!(
            matches!(worker.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), Event::Error(e) if e.contains("steamapps"))
        );
        config.steam_discovery = clip_store::Discovery::Declined;
        clip_store::save_config(temp.path(), &config).unwrap();
        worker.send(Request::Detect(Some(invalid_root)));
        assert!(
            matches!(worker.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), Event::Error(e) if e.contains("Approve"))
        );
    }

    #[test]
    fn searches_publish_batches_and_warning_events_without_ambiguous_matches() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("store");
        let steam = temp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        for id in 1..=10 {
            let name = match id {
                1 => "Game-A".to_owned(),
                10 => "GameA".to_owned(),
                _ => format!("Game0 {id}"),
            };
            fs::write(
                steam.join(format!("steamapps/appmanifest_{id}.acf")),
                format!("AppState {{ appid {id} name \"{name}\" }}"),
            )
            .unwrap();
        }
        fs::write(steam.join("steamapps/appmanifest_99.acf"), "broken {").unwrap();
        clip_store::save_config(
            &store,
            &clip_store::Config {
                steam_discovery: clip_store::Discovery::Enabled,
                ..Default::default()
            },
        )
        .unwrap();
        let worker = Worker::with_shortcuts(Some(store), Some(Vec::new()));
        let _ = receive_state(&worker);
        let folder = temp.path().join("Clips/Game A");
        worker.send(Request::Folders(vec![folder.clone()]));
        worker.send(Request::Detect(Some(steam)));
        let mut batches = 0;
        let mut warning = false;
        loop {
            match worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
            {
                Event::State(state) => {
                    batches += 1;
                    assert_eq!(state.games.len(), 10);
                    assert!(!state.folders.contains_key(&folder_key(&folder)));
                }
                Event::Warning(message) => {
                    warning = true;
                    assert!(message.contains("appmanifest_99.acf"));
                }
                Event::Detected(_) => break,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(batches >= 2);
        assert!(warning);
    }
    #[test]
    fn decoder_limits_dimensions_and_file_size() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wide.png");
        image::RgbaImage::new(4097, 1).save(&path).unwrap();
        assert!(decode(&path).is_err());
        let large = temp.path().join("large.png");
        fs::File::create(&large)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(decode(&large).unwrap_err().contains("16 MB"));
    }

    fn ico_with_png_frames(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut ico = vec![0, 0, 1, 0];
        ico.extend_from_slice(&(frames.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * frames.len();
        for (size, png) in frames {
            ico.extend_from_slice(&[*size as u8, *size as u8, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += png.len();
        }
        for (_, png) in frames {
            ico.extend_from_slice(png);
        }
        ico
    }
    fn encoded_png(image: image::DynamicImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    #[test]
    fn steam_ico_rgb_and_grayscale_png_frames_are_normalized() {
        let temp = tempfile::tempdir().unwrap();
        let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            16,
            image::Rgb([12, 34, 56]),
        ));
        let gray = image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(
            16,
            16,
            image::Luma([90]),
        ));
        for (name, image, expected) in [
            ("rgb", rgb, [12, 34, 56, 255]),
            ("gray", gray, [90, 90, 90, 255]),
        ] {
            let path = temp.path().join(format!("{name}.ico"));
            fs::write(&path, ico_with_png_frames(&[(16, encoded_png(image))])).unwrap();
            let pixels = decode(&path).unwrap();
            assert_eq!(pixels.dimensions(), (16, 16));
            assert_eq!(pixels.get_pixel(0, 0).0, expected);
            let hash = cache_source(temp.path(), &path, 0).unwrap();
            fs::remove_file(path).unwrap();
            assert_eq!(
                cached_image(temp.path(), &hash).unwrap().get_pixel(0, 0).0,
                expected
            );
        }
    }
    #[test]
    fn steam_ico_indexed_png_frame_is_normalized() {
        // A one-pixel indexed PNG with an opaque palette. No extra PNG encoder
        // dependency is needed just to construct this regression fixture.
        fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            let mut crc = u32::MAX;
            for byte in kind.iter().chain(data) {
                crc ^= *byte as u32;
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
                }
            }
            png.extend_from_slice(&(!crc).to_be_bytes());
        }
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 3, 0, 0, 0]);
        chunk(&mut png, b"PLTE", &[12, 34, 56]);
        chunk(
            &mut png,
            b"IDAT",
            &[0x78, 1, 1, 2, 0, 0xfd, 0xff, 0, 0, 0, 2, 0, 1],
        );
        chunk(&mut png, b"IEND", &[]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("indexed.ico");
        fs::write(&path, ico_with_png_frames(&[(1, png)])).unwrap();
        assert_eq!(decode(&path).unwrap().get_pixel(0, 0).0, [12, 34, 56, 255]);
    }
    #[test]
    fn ico_png_fallback_selects_largest_usable_frame_and_bounds_offsets() {
        let small = encoded_png(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 4])),
        ));
        let large = encoded_png(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            32,
            32,
            image::Rgb([12, 34, 56]),
        )));
        let bytes = ico_with_png_frames(&[
            (8, small),
            (32, large),
            (64, b"\x89PNG\r\n\x1a\nbroken".to_vec()),
        ]);
        let decoded = decode_ico_png(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 32));
        assert_eq!(decoded.into_rgba8().get_pixel(0, 0).0, [12, 34, 56, 255]);
        for bytes in [vec![], vec![0, 0, 1, 0, 1, 0], vec![0, 0, 1, 0, 0, 0]] {
            assert!(decode_ico_png(&bytes).is_err());
        }
        let mut bad_offset = ico_with_png_frames(&[(1, vec![0; 8])]);
        bad_offset[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_ico_png(&bad_offset).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn extracts_windows_resource_icon_without_launching() {
        let windows = std::env::var_os("SystemRoot").unwrap();
        let pixels =
            executable_icon(&PathBuf::from(windows).join("System32/shell32.dll"), 0).unwrap();
        assert!(pixels.width() > 0 && pixels.height() > 0);
        assert!(pixels.pixels().any(|pixel| pixel[3] > 0));
    }
}
