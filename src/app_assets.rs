//! Add the seek icons to GPUI Kit's default embedded assets.
use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

icon_assets!(
    SeekIcons,
    [RotateCcwClock, Pencil, Trash, VolumeX, Volume1, Volume2]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "spacer/icon.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/icon.png"))));
        }
        if let Some(bytes) = SeekIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(SeekIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppAssets, AssetSource};

    #[test]
    fn seek_and_default_icons_are_both_embedded() {
        assert!(
            AppAssets
                .load("icons/rotate-ccw-clock.svg")
                .unwrap()
                .is_some()
        );
        assert!(AppAssets.load("icons/play.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/pencil.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/trash.svg").unwrap().is_some());
        for path in [
            "icons/volume-x.svg",
            "icons/volume-1.svg",
            "icons/volume-2.svg",
        ] {
            assert!(AppAssets.load(path).unwrap().is_some());
        }
        assert!(AppAssets.load("spacer/icon.png").unwrap().is_some());
        let paths = AppAssets.list("icons").unwrap();
        assert!(
            paths
                .iter()
                .any(|path| path.as_ref() == "icons/rotate-ccw-clock.svg")
        );
    }
}
