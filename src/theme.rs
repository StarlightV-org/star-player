use crate::logging::{Level, Print};
use gpui_kit::component::{
    Theme, ThemeMode,
    theme::{Colorize, oklch},
};
use gpui_kit::{App, Hsla, rgb, rgba};
use notify::Watcher as _;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const ORIGINAL: &str = "spacer-original.json";

#[derive(Clone, Debug, PartialEq)]
pub struct ThemeEntry {
    pub file: String,
    pub name: String,
}

pub struct ThemeLibrary {
    pub directory: PathBuf,
    pub entries: Vec<ThemeEntry>,
    pub active: String,
    pub error: Option<String>,
    pub revision: u64,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
    _watcher: notify::RecommendedWatcher,
    pending: Option<Instant>,
    last_good: String,
}
impl gpui_kit::Global for ThemeLibrary {}

impl ThemeLibrary {
    pub fn open(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        create_default(&directory.join(ORIGINAL))?;
        let active = fs::read(directory.join("../appearance.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|v| v["theme"].as_str().map(str::to_owned))
            .filter(|file| {
                Path::new(file).file_name().and_then(|s| s.to_str()) == Some(file.as_str())
            })
            .unwrap_or_else(|| ORIGINAL.into());
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .map_err(|e| e.to_string())?;
        watcher
            .watch(&directory, notify::RecursiveMode::NonRecursive)
            .map_err(|e| e.to_string())?;
        let mut library = Self {
            directory,
            entries: vec![],
            active,
            error: None,
            revision: 0,
            receiver,
            _watcher: watcher,
            pending: None,
            last_good: DEFAULT_THEME.into(),
        };
        library.refresh();
        Ok(library)
    }

    fn refresh(&mut self) {
        self.error = None;
        let mut entries = vec![];
        match fs::read_dir(&self.directory) {
            Ok(files) => {
                for file in files.flatten() {
                    let path = file.path();
                    if !path
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.eq_ignore_ascii_case("json"))
                    {
                        continue;
                    }
                    let result = read_theme(&path).and_then(|text| {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).map_err(|e| e.to_string())?;
                        let name = value["name"]
                            .as_str()
                            .filter(|s| !s.trim().is_empty())
                            .ok_or_else(|| format!("{}: missing theme name", path.display()))?;
                        Ok(ThemeEntry {
                            file: file.file_name().to_string_lossy().into_owned(),
                            name: name.trim().into(),
                        })
                    });
                    match result {
                        Ok(entry) => entries.push(entry),
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        entries.sort_by_key(|e| e.name.to_lowercase());
        for pair in entries.windows(2) {
            if pair[0].name.to_lowercase() == pair[1].name.to_lowercase() {
                self.error = Some(format!(
                    "Duplicate theme name: {}. Rename one of these files.",
                    pair[0].name
                ));
            }
        }
        self.entries = entries;
        self.revision += 1;
    }

    fn reload_active(&mut self) -> Result<Palette, String> {
        let missing = !self.directory.join(&self.active).exists();
        if missing {
            fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
            create_default(&self.directory.join(ORIGINAL))?;
            self.active = ORIGINAL.into();
            self.save_selection()?;
            self.refresh();
        }
        let source = match read_theme(&self.directory.join(&self.active)) {
            Ok(source) => source,
            Err(error) if missing => {
                self.error = Some(error);
                DEFAULT_THEME.into()
            }
            Err(error) => return Err(error),
        };
        let palette = parse(&source)?;
        self.last_good = source;
        Ok(palette)
    }

    fn save_selection(&self) -> Result<(), String> {
        atomic_json(
            &self.directory.join("../appearance.json"),
            &serde_json::json!({"theme": self.active}),
            false,
        )
    }

    pub fn select(&mut self, file: &str) -> Result<(), String> {
        self.refresh();
        let entry = self
            .entries
            .iter()
            .find(|e| e.file == file)
            .ok_or("Theme is unavailable")?;
        if self
            .entries
            .iter()
            .any(|e| e.file != entry.file && e.name.to_lowercase() == entry.name.to_lowercase())
        {
            return Err(
                "Duplicate theme name. Rename one of these files before selecting it.".into(),
            );
        }
        read_theme(&self.directory.join(file))?;
        let previous = std::mem::replace(&mut self.active, file.into());
        if let Err(error) = self.save_selection() {
            self.active = previous;
            return Err(error);
        }
        self.revision += 1;
        Ok(())
    }

    pub fn create(&mut self) -> Result<(), String> {
        self.refresh();
        let mut number = 1;
        loop {
            let file = if number == 1 {
                "custom.json".into()
            } else {
                format!("custom-{number}.json")
            };
            let name = if number == 1 {
                "Custom".into()
            } else {
                format!("Custom {number}")
            };
            if !self.directory.join(&file).exists()
                && !self
                    .entries
                    .iter()
                    .any(|e| e.file.eq_ignore_ascii_case(&file))
                && !self
                    .entries
                    .iter()
                    .any(|e| e.name.to_lowercase() == name.to_lowercase())
            {
                let mut document: serde_json::Value =
                    serde_json::from_str(&self.last_good).map_err(|e| e.to_string())?;
                document["name"] = name.into();
                atomic_json(&self.directory.join(&file), &document, true)?;
                self.refresh();
                return self.select(&file);
            }
            number += 1;
        }
    }

    pub fn restore_original(&mut self) -> Result<(), String> {
        create_default(&self.directory.join(ORIGINAL))?;
        self.refresh();
        self.select(ORIGINAL)
    }

    pub fn rename(&mut self, name: &str) -> Result<(), String> {
        self.rename_file(&self.active.clone(), name)
    }

    pub fn rename_file(&mut self, source: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        let slug = theme_slug(name)?;
        let file = format!("{slug}.json");
        self.refresh();
        if !self.entries.iter().any(|e| e.file == source) {
            return Err("Theme is unavailable".into());
        }
        if self.entries.iter().any(|e| {
            e.file != source
                && (e.name.to_lowercase() == name.to_lowercase()
                    || e.file.eq_ignore_ascii_case(&file))
        }) {
            return Err("A theme with this name or filename already exists".into());
        }
        let old = self.directory.join(source);
        let new = self.directory.join(&file);
        if new != old && new.exists() {
            return Err("This filename already exists".into());
        }
        let mut document: serde_json::Value =
            serde_json::from_str(&read_theme(&old)?).map_err(|e| e.to_string())?;
        document["name"] = name.into();
        atomic_json(&new, &document, new != old)?;
        if new != old
            && let Err(error) = fs::remove_file(&old)
        {
            let _ = fs::remove_file(&new);
            return Err(error.to_string());
        }
        if self.active == source {
            self.active = file;
            self.save_selection()?;
        }
        self.refresh();
        Ok(())
    }
}

fn theme_slug(name: &str) -> Result<String, String> {
    let slug = name
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty()
        || slug.len() > 120
        || slug
            .chars()
            .any(|c| !(c.is_alphanumeric() || c == '-' || c == '_'))
        || matches!(slug.as_str(), "con" | "prn" | "aux" | "nul")
        || (slug.len() == 4
            && (slug.starts_with("com") || slug.starts_with("lpt"))
            && slug.ends_with(|c: char| c.is_ascii_digit()))
    {
        return Err("Use a name with letters, numbers, spaces, hyphens or underscores".into());
    }
    Ok(slug)
}

pub(crate) fn atomic_json(
    path: &Path,
    value: &impl serde::Serialize,
    new: bool,
) -> Result<(), String> {
    let mut staging =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing parent directory")?)
            .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut staging, value).map_err(|e| e.to_string())?;
    staging.write_all(b"\n").map_err(|e| e.to_string())?;
    staging.as_file().sync_all().map_err(|e| e.to_string())?;
    if new {
        staging.persist_noclobber(path).map_err(|e| e.to_string())?;
    } else {
        staging.persist(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Called by the UI heartbeat; filesystem callbacks never touch GPUI state.
pub fn poll(cx: &mut App) -> bool {
    if !cx.has_global::<ThemeLibrary>() {
        return false;
    }
    let library = cx.global_mut::<ThemeLibrary>();
    for event in library.receiver.try_iter() {
        match event {
            Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => {
                library.pending = Some(Instant::now())
            }
            Err(error) => library.error = Some(format!("Theme watcher: {error}")),
            _ => {}
        }
    }
    if library
        .pending
        .is_none_or(|time| time.elapsed() < Duration::from_millis(200))
    {
        return false;
    }
    library.pending = None;
    apply(cx);
    true
}

pub fn apply(cx: &mut App) {
    if !cx.has_global::<ThemeLibrary>() {
        apply_palette(parse(DEFAULT_THEME).expect("Embedded theme"), cx);
        match crate::clip_store::directory()
            .and_then(|root| ThemeLibrary::open(root.join("themes")))
        {
            Ok(library) => cx.set_global(library),
            Err(error) => {
                Print(Level::Error, format!("Themes: {error}"));
                apply_palette(parse(DEFAULT_THEME).expect("Embedded theme"), cx);
                return;
            }
        }
    }
    let library = cx.global_mut::<ThemeLibrary>();
    library.refresh();
    let result = library.reload_active();
    match result {
        Ok(colors) => apply_palette(colors, cx),
        Err(error) => {
            cx.global_mut::<ThemeLibrary>().error = Some(error.clone());
            Print(Level::Warn, format!("Theme: {error}"));
        }
    }
}

fn apply_palette(colors: Palette, cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        let foreground = colors["foreground"];
        let card = colors["card"];
        let primary = colors["primary"];
        let primary_foreground = colors["primary_foreground"];
        let secondary = colors["secondary"];
        let muted = colors["muted"];
        let border = colors["border"];
        let ring = colors["ring"];

        theme.background = colors["background"];
        theme.foreground = foreground;
        theme.group_box = card;
        theme.group_box_foreground = foreground;
        theme.popover = card;
        theme.popover_foreground = foreground;
        theme.primary = primary;
        theme.primary_foreground = primary_foreground;
        theme.secondary = secondary;
        theme.secondary_foreground = foreground;
        theme.muted = muted;
        theme.muted_foreground = colors["muted_foreground"];
        theme.accent = muted;
        theme.accent_foreground = foreground;
        theme.danger = colors["danger"];
        theme.danger_foreground = theme.background;
        theme.border = border;
        theme.input = colors["input"];
        theme.ring = ring;

        theme.chart_1 = colors["chart_1"];
        theme.chart_2 = colors["chart_2"];
        theme.chart_3 = colors["chart_3"];
        theme.chart_4 = colors["chart_4"];
        theme.chart_5 = primary;

        theme.sidebar = colors["sidebar"];
        theme.sidebar_foreground = foreground;
        theme.sidebar_primary = theme.chart_2;
        theme.sidebar_primary_foreground = primary_foreground;
        theme.sidebar_accent = muted;
        theme.sidebar_accent_foreground = foreground;
        theme.sidebar_border = border;
        // Sidebar focus uses the shared ring, which matches the supplied sidebar ring.

        // Kit has separate component colors in addition to its semantic palette.
        theme.primary_hover = theme.chart_4;
        theme.primary_active = primary.darken(0.1);
        theme.button_primary = primary;
        theme.button_primary_foreground = primary_foreground;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.secondary_hover = muted;
        theme.secondary_active = muted.darken(0.1);
        theme.button_secondary = secondary;
        theme.button_secondary_foreground = foreground;
        theme.button_secondary_hover = theme.secondary_hover;
        theme.button_secondary_active = theme.secondary_active;
        theme.button = secondary;
        theme.button_foreground = foreground;
        theme.button_hover = muted;
        theme.button_active = theme.secondary_active;
        theme.slider_bar = primary;
        theme.slider_thumb = primary_foreground;
        theme.progress_bar = primary;
        theme.selection = primary.opacity(0.3);
        theme.title_bar = colors["title_bar"];
        theme.title_bar_border = border;
        theme.status_bar = card;
        theme.status_bar_border = border;
        theme.table_active = primary.opacity(0.2);
        theme.table = theme.background;
        theme.table_even = card;
        theme.table_head = theme.background;
        theme.table_head_foreground = theme.muted_foreground;
        theme.table_foot = card;
        theme.table_foot_foreground = foreground;
        theme.table_hover = muted;
        theme.table_active_border = ring;
        theme.table_row_border = border;
        theme.colors.list = card;
        theme.list_even = card;
        theme.list_head = card;
        theme.list_hover = muted;
        theme.list_active = primary.opacity(0.2);
        theme.list_active_border = ring;
        theme.switch = secondary;
        theme.switch_thumb = primary_foreground;
        // Some Kit components read resolved tokens instead of ThemeColor.
        // Keep both representations on the same palette after a live reload.
        macro_rules! sync_tokens {
            ($($field:ident),* $(,)?) => { $(theme.tokens.$field=theme.colors.$field.into();)* };
        }
        sync_tokens!(
            background,
            foreground,
            primary,
            primary_foreground,
            primary_hover,
            primary_active,
            secondary,
            secondary_foreground,
            secondary_hover,
            secondary_active,
            muted,
            muted_foreground,
            accent,
            accent_foreground,
            border,
            input,
            ring,
            popover,
            popover_foreground,
            group_box,
            group_box_foreground,
            button,
            button_foreground,
            button_hover,
            button_active,
            button_primary,
            button_primary_foreground,
            button_primary_hover,
            button_primary_active,
            button_secondary,
            button_secondary_foreground,
            button_secondary_hover,
            button_secondary_active,
            sidebar,
            sidebar_foreground,
            sidebar_primary,
            sidebar_primary_foreground,
            sidebar_accent,
            sidebar_accent_foreground,
            sidebar_border,
            table,
            table_even,
            table_head,
            table_head_foreground,
            table_foot,
            table_foot_foreground,
            table_hover,
            table_active,
            table_active_border,
            table_row_border,
            list,
            list_even,
            list_head,
            list_hover,
            list_active,
            list_active_border,
            switch,
            switch_thumb,
            selection,
            slider_bar,
            slider_thumb,
            progress_bar,
            title_bar,
            title_bar_border,
            status_bar,
            status_bar_border,
            danger,
            danger_foreground
        );
    });
}

const DEFAULT_THEME: &str = include_str!("default-theme.json");
const MAX_THEME_BYTES: u64 = 64 * 1024;
type Palette = BTreeMap<String, Hsla>;

fn read_theme(path: &Path) -> Result<String, String> {
    let mut source = String::new();
    fs::File::open(path)
        .map_err(|e| format!("Read {}: {e}", path.display()))?
        .take(MAX_THEME_BYTES + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() as u64 > MAX_THEME_BYTES {
        return Err("Theme exceeds 64 KiB".into());
    }
    parse(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    let document: serde_json::Value = serde_json::from_str(&source).map_err(|e| e.to_string())?;
    if document["name"]
        .as_str()
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err(format!(
            "{}: a nonempty theme name is required",
            path.display()
        ));
    }
    Ok(source)
}

fn create_default(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let mut staging =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Theme path has no parent")?)
            .map_err(|e| e.to_string())?;
    staging
        .write_all(DEFAULT_THEME.as_bytes())
        .map_err(|e| e.to_string())?;
    staging.flush().map_err(|e| e.to_string())?;
    match staging.persist_noclobber(path) {
        Ok(_) => {
            Print(
                Level::Info,
                format!("Created default theme {}", path.display()),
            );
            Ok(())
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!("Create default theme: {}", error.error)),
    }
}

fn parse(source: &str) -> Result<Palette, String> {
    let defaults: serde_json::Value =
        serde_json::from_str(DEFAULT_THEME).map_err(|e| e.to_string())?;
    let default_colors = defaults["colors"]
        .as_object()
        .ok_or("Default colors must be an object")?;
    let document: serde_json::Value = serde_json::from_str(source).map_err(|e| e.to_string())?;
    let object = document.as_object().ok_or("Theme must be a JSON object")?;
    if object.keys().any(|key| key != "name" && key != "colors") {
        return Err("Theme accepts only name and colors fields".into());
    }
    if let Some(name) = object.get("name")
        && !name.is_string()
    {
        return Err("Theme name must be text".into());
    }
    let overrides = document["colors"]
        .as_object()
        .ok_or("colors must be an object")?;
    if let Some(key) = overrides
        .keys()
        .find(|key| !default_colors.contains_key(*key))
    {
        return Err(format!("Unknown theme color: {key}"));
    }
    default_colors
        .iter()
        .map(|(key, value)| {
            color(overrides.get(key).unwrap_or(value))
                .map(|value| (key.clone(), value))
                .map_err(|error| format!("{key}: {error}"))
        })
        .collect()
}

fn color(value: &serde_json::Value) -> Result<Hsla, String> {
    if let Some(hex) = value.as_str() {
        let digits = hex
            .strip_prefix('#')
            .ok_or("Hex colors must start with #")?;
        if digits.len() != 6 && digits.len() != 8 {
            return Err("Use #RRGGBB or #RRGGBBAA".into());
        }
        let number = u32::from_str_radix(digits, 16).map_err(|_| "Invalid hex color")?;
        return Ok(if digits.len() == 6 {
            rgb(number).into()
        } else {
            rgba(number).into()
        });
    }
    let channels = value
        .as_array()
        .ok_or("Use a hex color or [lightness, chroma, hue, optional alpha]")?;
    if channels.len() != 3 && channels.len() != 4 {
        return Err("OKLCH colors require three or four numbers".into());
    }
    let numbers: Vec<f64> = channels
        .iter()
        .map(|v| {
            v.as_f64()
                .filter(|n| n.is_finite())
                .ok_or("Color channels must be finite numbers")
        })
        .collect::<Result<_, _>>()?;
    let alpha = numbers.get(3).copied().unwrap_or(1.0);
    if !(0.0..=1.0).contains(&numbers[0])
        || !(0.0..=0.5).contains(&numbers[1])
        || !(0.0..=360.0).contains(&numbers[2])
        || !(0.0..=1.0).contains(&alpha)
    {
        return Err("OKLCH ranges: lightness 0..1, chroma 0..0.5, hue 0..360, alpha 0..1".into());
    }
    Ok(oklch(numbers[0] as f32, numbers[1] as f32, numbers[2] as f32).opacity(alpha as f32))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renaming_an_inactive_theme_does_not_change_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ThemeLibrary::open(dir.path().join("themes")).unwrap();
        library.reload_active().unwrap();
        library.create().unwrap();
        library.restore_original().unwrap();
        library.rename_file("custom.json", "Other Theme").unwrap();
        assert_eq!(library.active, ORIGINAL);
        assert!(!library.directory.join("custom.json").exists());
        assert!(library.directory.join("other-theme.json").exists());
        assert!(
            library
                .rename_file("other-theme.json", "Spacer original")
                .is_err()
        );
        assert!(
            library
                .rename_file("../appearance.json", "Outside")
                .is_err()
        );
    }
    #[test]
    fn theme_library_create_rename_duplicates_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("themes");
        let mut library = ThemeLibrary::open(folder.clone()).unwrap();
        library.reload_active().unwrap();
        assert_eq!(library.entries[0].name, "Spacer original");
        library.create().unwrap();
        assert_eq!(library.active, "custom.json");
        library.reload_active().unwrap();
        library.create().unwrap();
        assert_eq!(library.active, "custom-2.json");
        assert!(library.rename("CUSTOM").is_err());
        assert!(library.rename("Spacer Original").is_err());
        library.rename("Midnight Rose").unwrap();
        assert_eq!(library.active, "midnight-rose.json");
        assert!(!folder.join("custom-2.json").exists());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &fs::read_to_string(folder.join(&library.active)).unwrap()
            )
            .unwrap()["name"],
            "Midnight Rose"
        );
        drop(library);
        let mut library = ThemeLibrary::open(folder.clone()).unwrap();
        assert_eq!(library.active, "midnight-rose.json");
        fs::remove_file(folder.join(&library.active)).unwrap();
        library.reload_active().unwrap();
        assert_eq!(library.active, ORIGINAL);
        fs::remove_file(folder.join(ORIGINAL)).unwrap();
        library.reload_active().unwrap();
        assert!(folder.join(ORIGINAL).exists());
    }
    #[test]
    fn malformed_active_theme_keeps_last_good_copy_and_never_gets_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ThemeLibrary::open(dir.path().join("themes")).unwrap();
        library.reload_active().unwrap();
        let good = library.last_good.clone();
        let path = library.directory.join(&library.active);
        fs::write(&path, "{unfinished").unwrap();
        assert!(library.reload_active().is_err());
        assert_eq!(library.last_good, good);
        assert_eq!(fs::read_to_string(path).unwrap(), "{unfinished");
    }
    #[test]
    fn external_duplicate_names_are_reported_and_cannot_be_selected() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ThemeLibrary::open(dir.path().join("themes")).unwrap();
        fs::write(library.directory.join("duplicate.json"), DEFAULT_THEME).unwrap();
        library.refresh();
        assert!(library.error.as_ref().unwrap().contains("Duplicate"));
        assert!(library.select("duplicate.json").is_err());
    }
    #[test]
    fn unsafe_theme_filenames_are_rejected() {
        for name in [
            "",
            "../outside",
            "C:\\theme",
            "CON",
            "nul",
            "COM1",
            "theme.json",
            "a/b",
        ] {
            assert!(theme_slug(name).is_err(), "{name}");
        }
        assert_eq!(theme_slug("  Midnight   Rose ").unwrap(), "midnight-rose");
    }
    #[test]
    fn watcher_observes_external_file_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ThemeLibrary::open(dir.path().join("themes")).unwrap();
        let path = library.directory.join(ORIGINAL);
        let source = DEFAULT_THEME.replace("Spacer original", "Edited externally");
        atomic_json(
            &path,
            &serde_json::from_str::<serde_json::Value>(&source).unwrap(),
            false,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = library
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
                .unwrap();
            if !matches!(event.kind, notify::EventKind::Access(_)) {
                break;
            }
        }
        library.refresh();
        assert_eq!(library.entries[0].name, "Edited externally");
        library.reload_active().unwrap();
        assert!(library.last_good.contains("Edited externally"));
    }
    #[test]
    fn original_palette_and_partial_custom_colors() {
        let defaults = parse(DEFAULT_THEME).unwrap();
        assert_eq!(defaults["primary"], Hsla::from(rgb(0xa50036)));
        assert_eq!(defaults["background"], Hsla::from(rgb(0x0c090c)));
        assert_eq!(defaults["card"], Hsla::from(rgb(0x1d161e)));
        assert_eq!(defaults["muted"], Hsla::from(rgb(0x2a212c)));
        assert_eq!(defaults["sidebar"], Hsla::from(rgb(0x1d161e)));
        assert_eq!(defaults["title_bar"], Hsla::from(rgb(0x1d161e)));
        let custom = parse(r##"{"colors":{"primary":"#33aaff","border":"#ffffff20"}}"##).unwrap();
        assert_eq!(custom["primary"], Hsla::from(rgb(0x33aaff)));
        assert_eq!(custom["background"], defaults["background"]);
        assert_eq!(custom["border"], Hsla::from(rgba(0xffffff20)));
    }
    #[test]
    fn invalid_themes_report_errors() {
        for text in [
            "[]",
            "{}",
            r##"{"colors":{"primray":"#ffffff"}}"##,
            r#"{"colors":{"primary":[2,0,0]}}"#,
            r#"{"colors":{"primary":"bad"}}"#,
            r#"{"colors":{"primary":[0.5,0,0,2]}}"#,
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
    #[test]
    fn default_file_is_created_without_overwriting_customizations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("theme.json");
        create_default(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_THEME);
        fs::write(&path, "my custom theme").unwrap();
        create_default(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "my custom theme");
    }
}
