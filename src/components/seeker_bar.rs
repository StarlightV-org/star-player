use gpui_kit::component::{
    ActiveTheme as _,
    slider::{Slider, SliderState},
};
use gpui_kit::*;

pub struct SeekerBar {
    state: Entity<SliderState>,
    duration: f32,
    _subscription: Subscription,
}

impl SeekerBar {
    pub fn new(duration: f32, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(duration)
                .step(1.0)
                .default_value(0.0)
        });
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());

        Self {
            state,
            duration,
            _subscription: subscription,
        }
    }

    pub fn seek_by(&mut self, seconds: f32, window: &mut Window, cx: &mut Context<Self>) {
        let position = (self.state.read(cx).value().start() + seconds).clamp(0.0, self.duration);
        self.state.update(cx, |state, cx| {
            state.set_value(position, window, cx);
        });
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.set_value(0.0, window, cx);
        });
    }
}

fn timestamp(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u32;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

impl Render for SeekerBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let position = self.state.read(cx).value().start();

        div()
            .flex()
            .items_center()
            .gap_4()
            .child(div().w(px(44.0)).text_xs().child(timestamp(position)))
            .child(div().flex_1().child(Slider::new(&self.state)))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(timestamp(self.duration)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::timestamp;

    #[test]
    fn formats_preview_timestamps() {
        assert_eq!(timestamp(0.0), "00:00");
        assert_eq!(timestamp(9.0), "00:09");
        assert_eq!(timestamp(65.0), "01:05");
        assert_eq!(timestamp(300.0), "05:00");
        assert_eq!(timestamp(-1.0), "00:00");
    }
}
