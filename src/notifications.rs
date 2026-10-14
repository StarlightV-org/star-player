//! One presentation for user-facing failures throughout the app.
use gpui_kit::component::{WindowExt, notification::Notification};
use gpui_kit::{App, SharedString, Window};

struct AppError;

pub fn error(window: &mut Window, cx: &mut App, title: &str, message: &str) {
    crate::logging::Print(crate::logging::Level::Warn, message);
    window.push_notification(
        Notification::error(message.to_owned())
            .title(title.to_owned())
            .id1::<AppError>(SharedString::from(format!("{title}:{message}")))
            .autohide(false),
        cx,
    );
}
