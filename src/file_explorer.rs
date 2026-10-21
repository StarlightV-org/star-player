//! Reveal local files through Windows Shell, without command-line quoting.
use gpui_kit::{App, Window};
use std::path::{Path, PathBuf};

pub fn reveal(path: PathBuf, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let work = cx.background_executor().spawn(async move {
        // Shell calls need COM. Use a dedicated STA rather than changing the
        // apartment of a GPUI executor thread that might already be MTA.
        std::thread::Builder::new()
            .name("reveal-in-explorer".into())
            .spawn(move || reveal_native(&path))
            .map_err(|e| format!("Cannot start File Explorer request: {e}"))?
            .join()
            .map_err(|_| "File Explorer request failed".to_owned())?
    });
    cx.spawn(async move |cx| {
        if let Err(error) = work.await {
            let _ = handle.update(cx, |_, window, cx| {
                crate::notifications::error(window, cx, "Cannot reveal in File Explorer", &error);
            });
        }
    })
    .detach();
}

fn reveal_native(path: &Path) -> Result<(), String> {
    let path = std::fs::canonicalize(path).map_err(|e| {
        format!(
            "Cannot access {}: {e}",
            crate::clip_store::display_path(path)
        )
    })?;
    #[cfg(windows)]
    {
        use std::ptr;
        use windows_sys::Win32::{
            System::Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{Common::ITEMIDLIST, SHOpenFolderAndSelectItems, SHParseDisplayName},
        };

        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        struct Item(*mut ITEMIDLIST);
        impl Drop for Item {
            fn drop(&mut self) {
                unsafe { CoTaskMemFree(self.0.cast()) };
            }
        }

        // SAFETY: this function runs on a new thread. Every successful COM
        // initialization is balanced, including S_FALSE, and the PIDL is freed
        // before uninitializing the apartment.
        let initialized = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if initialized < 0 {
            return Err(shell_error("Initialize Windows Shell", initialized));
        }
        let _apartment = Apartment;
        let wide = shell_path(&path);
        let mut item = ptr::null_mut();
        let parsed = unsafe {
            SHParseDisplayName(
                wide.as_ptr(),
                ptr::null_mut(),
                &mut item,
                0,
                ptr::null_mut(),
            )
        };
        let item = Item(item);
        if parsed < 0 {
            return Err(shell_error("Resolve item in File Explorer", parsed));
        }
        if item.0.is_null() {
            return Err("Windows Shell could not resolve this item".into());
        }
        // With cidl=0 the absolute PIDL identifies the item to select. Windows
        // opens its parent and selects it, for both clips and folders.
        let opened = unsafe { SHOpenFolderAndSelectItems(item.0, 0, ptr::null(), 0) };
        if opened < 0 {
            return Err(shell_error("Open File Explorer", opened));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Revealing files is currently supported on Windows only".into())
    }
}

#[cfg(windows)]
fn shell_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    // canonicalize produces extended-length paths. Explorer's Shell namespace
    // expects normal drive/UNC paths. Keep UTF-16 intact, including surrogates.
    let extended: Vec<u16> = r"\\?\".encode_utf16().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let mut result = if wide.starts_with(&unc) {
        r"\\"
            .encode_utf16()
            .chain(wide[unc.len()..].iter().copied())
            .collect::<Vec<_>>()
    } else if wide.starts_with(&extended) && wide.get(5) == Some(&(b':' as u16)) {
        wide[extended.len()..].to_vec()
    } else {
        wide
    };
    result.push(0);
    result
}

#[cfg(windows)]
fn shell_error(operation: &str, code: i32) -> String {
    let description = if code as u32 & 0xffff0000 == 0x80070000 {
        std::io::Error::from_raw_os_error(code & 0xffff).to_string()
    } else {
        format!("Windows error 0x{:08X}", code as u32)
    };
    format!("{operation}: {description}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_item_returns_an_error_without_launching_explorer() {
        let dir = tempfile::tempdir().unwrap();
        assert!(reveal_native(&dir.path().join("missing.mp4")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn shell_paths_handle_extended_drive_unc_unicode_and_commas() {
        for (path, expected) in [
            (
                r"\\?\D:\Clips ü & games\clip, [1].mp4",
                r"D:\Clips ü & games\clip, [1].mp4",
            ),
            (
                r"\\?\UNC\server\share\clip ü.mp4",
                r"\\server\share\clip ü.mp4",
            ),
            (r"D:\Clips\clip.mp4", r"D:\Clips\clip.mp4"),
        ] {
            assert_eq!(
                shell_path(Path::new(path)),
                expected.encode_utf16().chain(Some(0)).collect::<Vec<_>>()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "opens Windows Explorer to verify native file and folder selection"]
    fn reveal_read_only_unicode_file_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Clips ü, & games");
        std::fs::create_dir(&folder).unwrap();
        let clip = folder.join("Clip ü, [1].mp4");
        std::fs::write(&clip, b"original bytes").unwrap();
        let metadata = std::fs::metadata(&clip).unwrap();
        let original_permissions = metadata.permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&clip, permissions).unwrap();
        let file = clip.clone();
        std::thread::spawn(move || reveal_native(&file))
            .join()
            .unwrap()
            .unwrap();
        std::thread::spawn(move || reveal_native(&folder))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(&clip).unwrap(), b"original bytes");
        assert_eq!(
            std::fs::metadata(&clip).unwrap().modified().unwrap(),
            metadata.modified().unwrap()
        );
        assert!(std::fs::metadata(&clip).unwrap().permissions().readonly());
        std::fs::set_permissions(&clip, original_permissions).unwrap();
    }
}
