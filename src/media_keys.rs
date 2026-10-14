//! Windows media keys are polled only while this process owns the foreground window.
//! This does not consume keys globally or interfere with other media applications.
#[derive(Default)]
pub struct MediaKeys {
    previous: [bool; 7],
}

#[derive(Clone, Copy)]
pub enum MediaAction {
    Toggle,
    Stop,
    Backward,
    Forward,
    Mute,
    VolumeDown,
    VolumeUp,
}

impl MediaKeys {
    #[cfg(windows)]
    pub fn poll(&mut self) -> Vec<MediaAction> {
        use windows_sys::Win32::{
            System::Threading::GetCurrentProcessId,
            UI::{
                Input::KeyboardAndMouse::GetAsyncKeyState,
                WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
            },
        };
        let mut process = 0;
        let active = unsafe {
            GetWindowThreadProcessId(GetForegroundWindow(), &mut process);
            process == GetCurrentProcessId()
        };
        let keys = [0xB3, 0xB2, 0xB1, 0xB0, 0xAD, 0xAE, 0xAF];
        let actions = [
            MediaAction::Toggle,
            MediaAction::Stop,
            MediaAction::Backward,
            MediaAction::Forward,
            MediaAction::Mute,
            MediaAction::VolumeDown,
            MediaAction::VolumeUp,
        ];
        let mut pressed = Vec::new();
        for (index, key) in keys.into_iter().enumerate() {
            let down = unsafe { GetAsyncKeyState(key) } < 0;
            // Windows already handles the system volume keys. Do not adjust the
            // player's gain too, which would apply the volume change twice.
            if active && down && !self.previous[index] && index < 4 {
                pressed.push(actions[index]);
            }
            self.previous[index] = down;
        }
        pressed
    }

    #[cfg(not(windows))]
    pub fn poll(&mut self) -> Vec<MediaAction> {
        Vec::new()
    }
}
