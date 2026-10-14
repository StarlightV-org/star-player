use crate::logging::{Level, Print};
use gpui_kit::component::{
    Theme, ThemeMode,
    theme::{Colorize, oklch},
};
use gpui_kit::{App, Hsla, rgb, rgba};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};

pub fn apply(cx: &mut App) {
    let colors = match load() {
        Ok(colors) => colors,
        Err(error) => {
            Print(
                Level::Warn,
                format!(
                    "Theme: {error}. Using the original palette; the custom file was not changed."
                ),
            );
            parse(DEFAULT_THEME).expect("Embedded default theme is valid")
        }
    };
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
        theme.title_bar = theme.background;
        theme.title_bar_border = border;
        theme.status_bar = card;
        theme.status_bar_border = border;
    });
}

const DEFAULT_THEME: &str = include_str!("default-theme.json");
const MAX_THEME_BYTES: u64 = 64 * 1024;
type Palette = BTreeMap<String, Hsla>;

fn load() -> Result<Palette, String> {
    let root = dirs::home_dir()
        .ok_or("Cannot find the home directory")?
        .join(".spacer");
    fs::create_dir_all(&root).map_err(|e| format!("Create {}: {e}", root.display()))?;
    let path = root.join("theme.json");
    create_default(&path)?;
    let mut source = String::new();
    fs::File::open(&path)
        .map_err(|e| format!("Read {}: {e}", path.display()))?
        .take(MAX_THEME_BYTES + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() as u64 > MAX_THEME_BYTES {
        return Err("theme.json exceeds 64 KiB".into());
    }
    let colors = parse(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    Print(Level::Info, format!("Loaded theme {}", path.display()));
    Ok(colors)
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
    if let Some(name) = object.get("name") {
        if !name.is_string() {
            return Err("Theme name must be text".into());
        }
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
    fn original_palette_and_partial_custom_colors() {
        let defaults = parse(DEFAULT_THEME).unwrap();
        assert_eq!(defaults["primary"], oklch(0.455, 0.188, 13.697));
        assert_eq!(defaults["background"], oklch(0.145, 0.008, 326.0));
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
