use gpui_kit::component::{
    IconName,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

pub enum MediaControlsEvent {
    Toggle,
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
        div().flex().items_center().gap_2().child(
            Button::new("play-pause")
                .primary()
                .rounded_full()
                .w(px(40.0))
                .h(px(40.0))
                .icon(if self.playing {
                    IconName::Pause
                } else {
                    IconName::Play
                })
                .tooltip(if self.playing { "Pause" } else { "Play" })
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(MediaControlsEvent::Toggle);
                })),
        )
    }
}
