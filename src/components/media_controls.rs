use gpui_kit::component::{
    IconName,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

pub enum MediaControlsEvent {
    Toggle,
    SeekBackward,
    SeekForward,
    Stop,
}

#[derive(Default)]
pub struct MediaControls {
    playing: bool,
}

impl MediaControls {
    pub fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playing != playing {
            self.playing = playing;
            cx.notify();
        }
    }
}

impl EventEmitter<MediaControlsEvent> for MediaControls {}

impl Render for MediaControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("seek-back")
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    // .label("10s")
                    .tooltip("Back 10 seconds")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(MediaControlsEvent::SeekBackward);
                    })),
            )
            .child(
                Button::new("play-pause")
                    .primary()
                    .icon(if self.playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    // .label(if self.playing { "Pause" } else { "Play" })
                    .tooltip(if self.playing { "Pause" } else { "Play" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        let _ = this;
                        cx.emit(MediaControlsEvent::Toggle);
                    })),
            )
            .child(
                Button::new("stop")
                    .ghost()
                    .icon(IconName::Square)
                    .tooltip("Stop")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.playing = false;
                        cx.emit(MediaControlsEvent::Stop);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("seek-forward")
                    .ghost()
                    .icon(IconName::ArrowRight)
                    // .label("10s")
                    .tooltip("Forward 10 seconds")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(MediaControlsEvent::SeekForward);
                    })),
            )
    }
}
