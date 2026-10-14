//! Read-only Steam discovery. Called only by an explicitly approved scan.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub struct Game {
    pub id: u32,
    pub name: String,
    pub icons: Vec<(PathBuf, i32)>,
}

pub struct Scan {
    pub games: Vec<Game>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
enum Value {
    Text(String),
    Object(BTreeMap<String, Value>),
}
impl Value {
    fn object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

// Valve's text KeyValues format, including comments and escaped Windows paths.
fn parse(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '{' || c == '}' {
            tokens.push(c.to_string());
            continue;
        }
        let mut token = String::new();
        if c == '"' {
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '"' {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    let next = chars.next().ok_or("Incomplete escape in Steam metadata")?;
                    if next != '\\' && next != '"' {
                        token.push('\\');
                    }
                    token.push(next);
                } else {
                    token.push(c);
                }
            }
            if !closed {
                return Err("Unclosed string in Steam metadata".into());
            }
        } else {
            token.push(c);
            while chars
                .peek()
                .is_some_and(|c| !c.is_whitespace() && *c != '{' && *c != '}')
            {
                token.push(chars.next().unwrap());
            }
        }
        // Prefix text so quoted braces cannot be mistaken for structure.
        tokens.push(format!("t{token}"));
    }
    fn object(
        tokens: &[String],
        pos: &mut usize,
        nested: bool,
        depth: usize,
    ) -> Result<BTreeMap<String, Value>, String> {
        if depth > 32 {
            return Err("Steam metadata is nested too deeply".into());
        }
        let mut map = BTreeMap::new();
        while let Some(key) = tokens.get(*pos) {
            if key == "}" {
                if !nested {
                    return Err("Unexpected closing brace in Steam metadata".into());
                }
                *pos += 1;
                return Ok(map);
            }
            let key = key
                .strip_prefix('t')
                .ok_or("Expected Steam metadata key")?
                .to_owned();
            *pos += 1;
            let token = tokens.get(*pos).ok_or("Missing Steam metadata value")?;
            *pos += 1;
            let value = if token == "{" {
                Value::Object(object(tokens, pos, true, depth + 1)?)
            } else {
                Value::Text(
                    token
                        .strip_prefix('t')
                        .ok_or("Expected Steam metadata value")?
                        .to_owned(),
                )
            };
            map.insert(key.to_ascii_lowercase(), value);
        }
        if nested {
            Err("Unclosed object in Steam metadata".into())
        } else {
            Ok(map)
        }
    }
    object(&tokens, &mut 0, false, 0)
}

fn read_metadata(path: &Path) -> Result<BTreeMap<String, Value>, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
        return Err("Steam metadata exceeds 4 MB".into());
    }
    parse(&fs::read_to_string(path).map_err(|e| e.to_string())?)
}

#[cfg(windows)]
pub fn locate() -> Result<PathBuf, String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    for (key, subkey, value) in [
        (HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath"),
        (
            HKEY_LOCAL_MACHINE,
            "Software\\WOW6432Node\\Valve\\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, "Software\\Valve\\Steam", "InstallPath"),
    ] {
        let subkey = wide(subkey);
        let value = wide(value);
        let mut buffer = [0u16; 32768];
        let mut len = (buffer.len() * 2) as u32;
        let result = unsafe {
            RegGetValueW(
                key,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if result == 0 {
            let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
            let path = PathBuf::from(String::from_utf16_lossy(&buffer[..end]));
            if path.is_absolute() && path.join("steamapps").is_dir() {
                return Ok(path);
            }
        }
    }
    Err("Cannot locate Steam. Use Choose Steam folder to select its installation directory.".into())
}
#[cfg(not(windows))]
pub fn locate() -> Result<PathBuf, String> {
    Err("Select the Steam installation folder manually.".into())
}

pub fn shortcut_directories() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = dirs::desktop_dir() {
        paths.push(path);
    }
    if let Some(path) = dirs::data_dir() {
        paths.push(path.join("Microsoft/Windows/Start Menu/Programs/Steam"));
    }
    if let Some(path) = std::env::var_os("PUBLIC") {
        paths.push(PathBuf::from(path).join("Desktop"));
    }
    paths
}

fn shortcuts(directories: &[PathBuf]) -> BTreeMap<u32, (PathBuf, i32)> {
    let mut result = BTreeMap::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("url"))
            {
                continue;
            }
            if fs::metadata(&path).map_or(true, |m| m.len() > 65536) {
                continue;
            }
            let Ok(bytes) = fs::read(path) else {
                continue;
            };
            let text = if bytes.starts_with(&[0xff, 0xfe]) {
                let utf16 = bytes[2..]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>();
                match String::from_utf16(&utf16) {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            } else {
                match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            };
            let values = text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().trim_matches('"')))
                .collect::<BTreeMap<_, _>>();
            let Some(id) = values
                .get("url")
                .and_then(|url| url.strip_prefix("steam://rungameid/"))
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(icon) = values.get("iconfile") else {
                continue;
            };
            let icon = PathBuf::from(icon);
            if icon.is_absolute() && icon.is_file() {
                let index = values
                    .get("iconindex")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                result.insert(id, (icon, index));
            }
        }
    }
    result
}

pub fn scan(root: &Path, shortcut_dirs: &[PathBuf]) -> Result<Scan, String> {
    if !root.is_absolute() || !root.join("steamapps").is_dir() {
        return Err("Select a Steam installation folder containing steamapps.".into());
    }
    let mut scan = Scan {
        games: Vec::new(),
        warnings: Vec::new(),
    };
    let mut libraries = vec![root.to_owned()];
    let library_file = root.join("steamapps/libraryfolders.vdf");
    if library_file.exists() {
        match read_metadata(&library_file) {
            Ok(metadata) => {
                if let Some(library) = metadata.get("libraryfolders").and_then(Value::object) {
                    for (key, value) in library {
                        if key.parse::<u32>().is_err() {
                            continue;
                        }
                        let path = value.text().or_else(|| value.object()?.get("path")?.text());
                        if let Some(path) = path {
                            let path = PathBuf::from(path);
                            if path.is_absolute() && !libraries.contains(&path) {
                                libraries.push(path);
                            }
                        }
                    }
                }
            }
            Err(e) => scan
                .warnings
                .push(format!("{}: {e}", library_file.display())),
        }
    }
    let shortcuts = shortcuts(shortcut_dirs);
    let cache = root.join("appcache/librarycache");
    let mut seen = std::collections::HashSet::new();
    for library in libraries {
        let apps = library.join("steamapps");
        let entries = match fs::read_dir(&apps) {
            Ok(entries) => entries,
            Err(e) => {
                scan.warnings.push(format!("{}: {e}", apps.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some(expected) = file
                .strip_prefix("appmanifest_")
                .and_then(|s| s.strip_suffix(".acf"))
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let parsed = (|| {
                let metadata = read_metadata(&path)?;
                let app = metadata
                    .get("appstate")
                    .and_then(Value::object)
                    .ok_or("Missing AppState")?;
                let id = app
                    .get("appid")
                    .and_then(Value::text)
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or("Missing app ID")?;
                if id != expected {
                    return Err("App ID does not match manifest filename".into());
                }
                let name = app
                    .get("name")
                    .and_then(Value::text)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("Missing game name")?
                    .to_owned();
                Ok::<_, String>((id, name))
            })();
            let (id, name) = match parsed {
                Ok(game) => game,
                Err(e) => {
                    scan.warnings.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            if !seen.insert(id) {
                continue;
            }
            let mut icons = Vec::new();
            if let Some(icon) = shortcuts.get(&id) {
                icons.push(icon.clone());
            }
            // Steam has used both flat and per-app artwork caches.
            for suffix in [
                "icon.jpg",
                "icon.png",
                "logo.png",
                "library_600x900.jpg",
                "header.jpg",
            ] {
                icons.push((cache.join(format!("{id}_{suffix}")), 0));
            }
            if let Ok(entries) = fs::read_dir(cache.join(id.to_string())) {
                let mut images = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().is_some_and(|s| {
                            let s = s.to_string_lossy().to_ascii_lowercase();
                            s.contains("icon")
                                || s.contains("logo")
                                || s.contains("library_600x900")
                                || s.contains("header")
                        })
                    })
                    .collect::<Vec<_>>();
                images.sort_by_key(|p| {
                    let s = p
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_ascii_lowercase();
                    (
                        if s.contains("icon") {
                            0
                        } else if s.contains("logo") {
                            1
                        } else {
                            2
                        },
                        p.clone(),
                    )
                });
                icons.extend(images.into_iter().map(|p| (p, 0)));
            }
            icons.retain(|(p, _)| p.is_file());
            // Prefer actual icons even when newer per-app caches coexist with
            // older flat header/cover files. Stable sorting retains shortcut priority.
            icons.sort_by_key(|(path, _)| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                if name.ends_with(".ico") || name.ends_with(".exe") || name.ends_with(".dll") {
                    0
                } else if name.contains("icon") {
                    1
                } else if name.contains("logo") {
                    2
                } else if name.contains("library_600x900") {
                    3
                } else {
                    4
                }
            });
            scan.games.push(Game { id, name, icons });
        }
    }
    scan.games
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyvalues_handles_comments_escapes_and_rejects_damage() {
        let values =
            parse("// comment\n\"root\" { \"name\" \"A \\\"game\\\"\" \"path\" \"D:\\\\Steam\" }")
                .unwrap();
        let root = values["root"].object().unwrap();
        assert_eq!(root["name"].text(), Some("A \"game\""));
        assert_eq!(root["path"].text(), Some("D:\\Steam"));
        for text in ["root {", "root", "root \"broken", "}"] {
            assert!(parse(text).is_err());
        }
    }
    #[test]
    fn discovers_multiple_libraries_and_skips_bad_manifests() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Steam");
        let other = temp.path().join("Other");
        for p in [&root, &other] {
            fs::create_dir_all(p.join("steamapps")).unwrap();
        }
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} }}",
                other.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        fs::write(
            root.join("steamapps/appmanifest_1.acf"),
            "AppState { appid 1 name \"First game\" }",
        )
        .unwrap();
        fs::write(
            other.join("steamapps/appmanifest_2.acf"),
            "AppState { appid 2 name \"Second game\" }",
        )
        .unwrap();
        fs::write(other.join("steamapps/appmanifest_3.acf"), "broken {").unwrap();
        let scan = scan(&root, &[]).unwrap();
        assert_eq!(scan.games.len(), 2);
        assert_eq!(scan.warnings.len(), 1);
    }

    #[test]
    fn shortcuts_match_by_app_id_and_preserve_icon_index() {
        let temp = tempfile::tempdir().unwrap();
        let icon = temp.path().join("icon.ico");
        fs::write(&icon, b"fixture").unwrap();
        let shortcut = format!(
            "[InternetShortcut]\nURL=steam://rungameid/42\nIconFile={}\nIconIndex=-7\n",
            icon.display()
        );
        fs::write(temp.path().join("renamed.url"), &shortcut).unwrap();
        let found = shortcuts(&[temp.path().to_owned()]);
        assert_eq!(found[&42], (icon.clone(), -7));
        let mut bytes = vec![0xff, 0xfe];
        for unit in shortcut
            .replace("rungameid/42", "rungameid/43")
            .encode_utf16()
        {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(temp.path().join("unicode.url"), bytes).unwrap();
        let found = shortcuts(&[temp.path().to_owned()]);
        assert_eq!(found[&43], (icon, -7));
    }
    #[test]
    fn per_app_cache_prioritizes_icons_over_artwork() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/appmanifest_1.acf"),
            "AppState { appid 1 name Game }",
        )
        .unwrap();
        let cache = root.join("appcache/librarycache/1");
        fs::create_dir_all(&cache).unwrap();
        for file in ["hash_header.jpg", "hash_icon.jpg", "hash_logo.png"] {
            fs::write(cache.join(file), b"fixture").unwrap();
        }
        let scanned = scan(root, &[]).unwrap();
        assert_eq!(scanned.games[0].icons[0].0, cache.join("hash_icon.jpg"));
        assert_eq!(scanned.games[0].icons[1].0, cache.join("hash_logo.png"));
    }
}
