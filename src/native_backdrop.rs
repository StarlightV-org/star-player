//! An opaque Windows backing surface below mpv and GPUI's transparent controls.
//! It outlives every decoder and resizes before GPUI's next rendered frame.
#[cfg(windows)]
mod platform {
    use crate::logging::{Level, Print};
    use std::{cell::Cell, sync::OnceLock};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{BLACK_BRUSH, GetStockObject, InvalidateRect, UpdateWindow},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    };

    struct State {
        hwnd: HWND,
        size: Cell<Option<[i32; 2]>>,
    }

    impl State {
        fn resize(&self, parent: HWND) -> Result<(), String> {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            if unsafe { GetClientRect(parent, &mut rect) } == 0 {
                return Err(format!(
                    "Background client bounds: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let size = [rect.right - rect.left, rect.bottom - rect.top];
            if self.size.get() == Some(size) {
                return Ok(());
            }
            if unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_BOTTOM,
                    0,
                    0,
                    size[0],
                    size[1],
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            } == 0
            {
                return Err(format!(
                    "Resize background: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // Paint the backing bitmap now, not after an unrelated UI/frame update.
            unsafe {
                SetLayeredWindowAttributes(self.hwnd, 0, 255, LWA_ALPHA);
                InvalidateRect(self.hwnd, std::ptr::null(), 1);
                UpdateWindow(self.hwnd);
            }
            self.size.set(Some(size));
            Ok(())
        }
    }

    unsafe extern "system" fn parent_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        data: usize,
    ) -> LRESULT {
        let state = unsafe { &*(data as *const State) };
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if message == WM_SIZE {
            if let Err(error) = state.resize(hwnd) {
                Print(Level::Warn, error);
            }
        }
        result
    }

    pub struct NativeBackdrop {
        parent: HWND,
        state: Box<State>,
        _ui_thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl NativeBackdrop {
        pub fn new(parent: HWND) -> Result<Self, String> {
            static CLASS: OnceLock<Result<u16, String>> = OnceLock::new();
            let class: Vec<u16> = "SpacerOpaqueBackdrop\0".encode_utf16().collect();
            CLASS
                .get_or_init(|| {
                    let class_def = WNDCLASSW {
                        style: CS_HREDRAW | CS_VREDRAW,
                        lpfnWndProc: Some(DefWindowProcW),
                        hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                        lpszClassName: class.as_ptr(),
                        hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) as _ },
                        ..unsafe { std::mem::zeroed() }
                    };
                    let atom = unsafe { RegisterClassW(&class_def) };
                    if atom == 0 {
                        Err(format!(
                            "Register background class: {}",
                            std::io::Error::last_os_error()
                        ))
                    } else {
                        Ok(atom)
                    }
                })
                .as_ref()
                .map_err(Clone::clone)?;
            // The layered child gets its own redirected backing bitmap, even if
            // GPUI's parent uses WS_EX_NOREDIRECTIONBITMAP. Alpha is always 255.
            // Do not clip siblings out of this bitmap: it must remain solid behind
            // the video, including when mpv removes/replaces its presentation surface.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_NOACTIVATE,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_CHILD | WS_DISABLED,
                    0,
                    0,
                    1,
                    1,
                    parent,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if hwnd.is_null() {
                return Err(format!(
                    "Create opaque background: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let backdrop = Self {
                parent,
                state: Box::new(State {
                    hwnd,
                    size: Cell::new(None),
                }),
                _ui_thread: std::marker::PhantomData,
            };
            if unsafe { SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) } == 0 {
                return Err(format!(
                    "Set background opacity: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let data = &*backdrop.state as *const State as usize;
            if unsafe { SetWindowSubclass(parent, Some(parent_proc), data, data) } == 0 {
                return Err(format!(
                    "Install background resize handler: {}",
                    std::io::Error::last_os_error()
                ));
            }
            backdrop.state.resize(parent)?;
            Print(
                Level::Info,
                "Created persistent opaque background below native video and GPUI",
            );
            Ok(backdrop)
        }

        #[cfg(test)]
        pub fn hwnd(&self) -> HWND {
            self.state.hwnd
        }

        pub fn create_video_host(&self) -> Result<HWND, String> {
            let class: Vec<u16> = "SpacerOpaqueBackdrop\0".encode_utf16().collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_DISABLED,
                    0,
                    0,
                    1,
                    1,
                    self.state.hwnd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if hwnd.is_null() {
                Err(format!(
                    "Create video panel: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(hwnd)
            }
        }
    }

    impl Drop for NativeBackdrop {
        fn drop(&mut self) {
            let data = &*self.state as *const State as usize;
            unsafe {
                if IsWindow(self.parent) != 0 {
                    RemoveWindowSubclass(self.parent, Some(parent_proc), data);
                }
                if IsWindow(self.state.hwnd) != 0 {
                    DestroyWindow(self.state.hwnd);
                }
            }
        }
    }
}

#[cfg(windows)]
pub use platform::NativeBackdrop;
