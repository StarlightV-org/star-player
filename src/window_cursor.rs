//! UI-thread cursor visibility, scoped to the player's client area.
#[cfg(windows)]
mod platform {
    use std::cell::Cell;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::ScreenToClient,
        UI::{
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    };

    // WinUser.h message, exposed by windows-sys under the Controls feature.
    const WM_MOUSELEAVE: u32 = 0x02A3;

    struct State {
        requested: Cell<bool>,
        decrements: Cell<u32>,
    }

    impl State {
        fn restore(&self) {
            // Balance only our own changes to Win32's per-thread display count.
            for _ in 0..self.decrements.replace(0) {
                unsafe {
                    ShowCursor(1);
                }
            }
        }

        fn update(&self, hwnd: HWND) {
            if !self.requested.get() || !over_client(hwnd) {
                self.restore();
                return;
            }
            if self.decrements.get() == 0 {
                let mut count = 0;
                loop {
                    count += 1;
                    if unsafe { ShowCursor(0) } < 0 {
                        break;
                    }
                }
                self.decrements.set(count);
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut tracking);
                }
            }
        }
    }

    fn over_client(hwnd: HWND) -> bool {
        unsafe {
            let mut point = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut point) == 0 {
                return false;
            }
            let hovered = WindowFromPoint(point);
            if hovered != hwnd && IsChild(hwnd, hovered) == 0 {
                return false;
            }
            let mut rect: RECT = std::mem::zeroed();
            if ScreenToClient(hwnd, &mut point) == 0 || GetClientRect(hwnd, &mut rect) == 0 {
                return false;
            }
            point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
        }
    }

    unsafe extern "system" fn cursor_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        data: usize,
    ) -> LRESULT {
        // The boxed state stays alive until this subclass is removed.
        let state = unsafe { &*(data as *const State) };
        match message {
            WM_MOUSEMOVE => {
                state.requested.set(false);
                state.restore();
            }
            WM_MOUSELEAVE | WM_NCMOUSEMOVE | WM_NCDESTROY => state.restore(),
            WM_ACTIVATE if wparam & 0xffff == WA_INACTIVE as usize => state.restore(),
            WM_SETCURSOR => {
                if lparam as u16 == HTCLIENT as u16 {
                    state.update(hwnd);
                } else {
                    state.restore();
                }
            }
            _ => {}
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    pub struct WindowCursor {
        hwnd: HWND,
        state: Box<State>,
    }

    impl WindowCursor {
        pub fn new(hwnd: HWND) -> Result<Self, String> {
            let state = Box::new(State {
                requested: Cell::new(false),
                decrements: Cell::new(0),
            });
            let data = &*state as *const State as usize;
            if unsafe { SetWindowSubclass(hwnd, Some(cursor_proc), data, data) } == 0 {
                return Err(format!(
                    "Install cursor handler: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { hwnd, state })
        }

        pub fn set_hidden(&self, hidden: bool) {
            self.state.requested.set(hidden);
            self.state.update(self.hwnd);
        }
    }

    impl Drop for WindowCursor {
        fn drop(&mut self) {
            self.state.restore();
            let id = &*self.state as *const State as usize;
            unsafe {
                RemoveWindowSubclass(self.hwnd, Some(cursor_proc), id);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn display_count() -> i32 {
            unsafe {
                ShowCursor(1);
                ShowCursor(0)
            }
        }

        #[test]
        fn movement_leave_and_drop_restore_our_cursor_changes() {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    100,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            let cursor = WindowCursor::new(hwnd).unwrap();
            let before = display_count();
            for message in [WM_MOUSEMOVE, WM_MOUSELEAVE, WM_NCMOUSEMOVE, WM_ACTIVATE] {
                cursor.state.requested.set(true);
                unsafe {
                    ShowCursor(0);
                }
                cursor.state.decrements.set(1);
                unsafe {
                    SendMessageW(hwnd, message, 0, 0);
                }
                assert_eq!(cursor.state.decrements.get(), 0);
                assert_eq!(display_count(), before);
                if message == WM_MOUSEMOVE {
                    assert!(!cursor.state.requested.get());
                }
            }
            // A hidden window is never under the pointer, so it must not hide it.
            cursor.set_hidden(true);
            assert_eq!(display_count(), before);
            unsafe {
                ShowCursor(0);
            }
            cursor.state.decrements.set(1);
            drop(cursor);
            assert_eq!(display_count(), before);
            unsafe {
                DestroyWindow(hwnd);
            }
        }
    }
}

#[cfg(windows)]
pub use platform::WindowCursor;
