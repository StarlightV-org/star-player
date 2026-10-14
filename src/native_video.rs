//! UI-thread-owned child window. GPUI provides layout, mpv owns its GPU output.
use crate::logging::{Level, Print};
use gpui_kit::{Bounds, Pixels, Window};
use std::cell::Cell;

pub struct NativeVideo {
    hwnd: Cell<usize>,
    geometry: Cell<Option<([i32; 4], f32)>>,
    // Prevent moving this owner off the window's UI thread.
    _ui_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl NativeVideo {
    #[cfg(windows)]
    pub fn new(window: &Window) -> Result<Self, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        // Public Win32 static-control style, not exposed by windows-sys.
        const SS_BLACKRECT: u32 = 0x00000004;
        let RawWindowHandle::Win32(parent) = HasWindowHandle::window_handle(window)
            .map_err(|e| e.to_string())?
            .as_raw()
        else {
            return Err("GPUI did not provide a Windows HWND".into());
        };
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // Disabled children do not take mouse/keyboard input. Clicks hit GPUI's
        // parent panel instead, even when mpv creates its own nested VO window.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_DISABLED | SS_BLACKRECT,
                0,
                0,
                1,
                1,
                parent.hwnd.get() as _,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(format!(
                "Create video panel: {}",
                std::io::Error::last_os_error()
            ));
        }
        Print(
            Level::Info,
            format!("Created native video panel HWND={hwnd:p}"),
        );
        Ok(Self {
            hwnd: Cell::new(hwnd as usize),
            geometry: Cell::new(None),
            _ui_thread: std::marker::PhantomData,
        })
    }

    #[cfg(not(windows))]
    pub fn new(_: &Window) -> Result<Self, String> {
        Err("Native playback supports Windows x86_64 only".into())
    }

    pub fn hwnd(&self) -> usize {
        self.hwnd.get()
    }

    pub fn layout(&self, bounds: Bounds<Pixels>, scale: f32) {
        let rect = panel_rect(bounds, scale);
        if self.hwnd.get() == 0 || self.geometry.get() == Some((rect, scale)) {
            return;
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let visible = rect[2] > 0 && rect[3] > 0;
            if SetWindowPos(
                self.hwnd.get() as _,
                std::ptr::null_mut(),
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                SWP_NOACTIVATE
                    | SWP_NOZORDER
                    | if visible {
                        SWP_SHOWWINDOW
                    } else {
                        SWP_HIDEWINDOW
                    },
            ) == 0
            {
                Print(
                    Level::Error,
                    format!("Position video panel: {}", std::io::Error::last_os_error()),
                );
                return;
            }
        }
        self.geometry.set(Some((rect, scale)));
        Print(
            Level::Debug,
            format!("Native panel rect={rect:?} DPI scale={scale}"),
        );
    }

    /// Only call after Playback has stopped and joined its worker.
    pub fn destroy(&self) {
        let hwnd = self.hwnd.replace(0);
        if hwnd == 0 {
            return;
        }
        Print(
            Level::Info,
            "Destroying native video panel after playback shutdown",
        );
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd as _);
        }
    }
}

impl Drop for NativeVideo {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn panel_rect(bounds: Bounds<Pixels>, scale: f32) -> [i32; 4] {
    let left = (f32::from(bounds.origin.x) * scale).round() as i32;
    let top = (f32::from(bounds.origin.y) * scale).round() as i32;
    let right = (f32::from(bounds.origin.x + bounds.size.width) * scale).round() as i32;
    let bottom = (f32::from(bounds.origin.y + bounds.size.height) * scale).round() as i32;
    [left, top, (right - left).max(0), (bottom - top).max(0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_bounds_round_edges_at_fractional_dpi() {
        let bounds = Bounds::new(
            gpui_kit::point(gpui_kit::px(10.0), gpui_kit::px(20.0)),
            gpui_kit::size(gpui_kit::px(301.0), gpui_kit::px(101.0)),
        );
        assert_eq!(panel_rect(bounds, 1.5), [15, 30, 452, 152]);
        assert_eq!(panel_rect(bounds, 1.0), [10, 20, 301, 101]);
    }

    /// Requires an interactive desktop and a working D3D11 adapter.
    #[test]
    #[cfg(windows)]
    #[ignore = "interactive native D3D11 playback and window lifecycle"]
    fn native_gpu_playback_steps_and_60fps_throughput() {
        use crate::playback::{Command, Playback};
        use std::time::{Duration, Instant};
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let parent = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                100,
                100,
                640,
                480,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!parent.is_null());
        struct Parent(windows_sys::Win32::Foundation::HWND);
        impl Drop for Parent {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let _parent = Parent(parent);
        let child = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_DISABLED | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                320,
                240,
                parent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!child.is_null());
        let host = NativeVideo {
            hwnd: Cell::new(child as usize),
            geometry: Cell::new(None),
            _ui_thread: std::marker::PhantomData,
        };
        let player = Playback::native(host.hwnd()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native-60fps.avi");
        crate::playback::test_fixtures::avi_timed(&path, 2, false, 320, 240, 60, 240);
        let wait = |predicate: &dyn Fn(&crate::playback::Snapshot) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                unsafe {
                    let mut msg = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                let state = player.snapshot();
                assert!(state.error.is_none(), "{:?}", state.error);
                if predicate(&state) {
                    return state;
                }
                assert!(
                    Instant::now() < deadline,
                    "Native playback stalled: {state:?}"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let started = Instant::now();
        player.send(Command::Load(path)).unwrap();
        let state = wait(&|s| s.position >= 3.5);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Native output did not sustain real-time 60fps"
        );
        assert_eq!(state.video_output.as_deref(), Some("gpu"));
        // A 30fps presentation cap would drop about half this 60fps fixture.
        assert!(
            state.dropped_frames.unwrap() + state.decoder_dropped_frames.unwrap_or(0.0) < 40.0,
            "{state:?}"
        );
        player.send(Command::Pause).unwrap();
        wait(&|s| s.paused);
        player.send(Command::SetVolume(37.0)).unwrap();
        for muted in [false, true] {
            player.send(Command::SetMute(muted)).unwrap();
            player.send(Command::SeekAbsolute(1.0)).unwrap();
            wait(&|s| s.paused && (s.position - 1.0).abs() < 0.001 && s.muted == muted);
            player.send(Command::FrameStep).unwrap();
            let state = wait(&|s| (s.position - (1.0 + 1.0 / 60.0)).abs() < 0.001);
            assert!(state.paused && state.effective_muted);
            assert_eq!(state.volume, 37.0);
            player.send(Command::FrameBackStep).unwrap();
            let state = wait(&|s| (s.position - 1.0).abs() < 0.001);
            assert!(state.paused && state.effective_muted);
            player.send(Command::Play).unwrap();
            wait(&|s| !s.paused && s.effective_muted == muted);
            player.send(Command::Pause).unwrap();
            wait(&|s| s.paused && s.effective_muted);
        }
        host.layout(
            Bounds::new(
                gpui_kit::point(gpui_kit::px(10.0), gpui_kit::px(20.0)),
                gpui_kit::size(gpui_kit::px(200.0), gpui_kit::px(100.0)),
            ),
            1.5,
        );
        // Join with a live UI message pump, as destruction may send HWND messages.
        player.send(Command::Shutdown).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !player.is_stopped() {
            unsafe {
                let mut msg = std::mem::zeroed();
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(player);
        host.destroy();
    }
}
