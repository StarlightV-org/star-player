use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants},
    slider::{Slider, SliderEvent, SliderState},
};
use gpui_kit::*;

pub struct VolumeControls {
    state: Entity<SliderState>,
    muted: bool,
    _subscriptions: Vec<Subscription>,
}

impl VolumeControls {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(75.0)
        });
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    if value.start() > 0.0 {
                        this.muted = false;
                    }
                    cx.notify();
                }
            }),
        ];

        Self {
            state,
            muted: false,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for VolumeControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let volume = self.state.read(cx).value().start();
        let silent = self.muted || volume == 0.0;

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                Button::new("mute")
                    .ghost()
                    .label(if silent { "Unmute" } else { "Mute" })
                    .tooltip("Mute / unmute")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.state.read(cx).value().start() == 0.0 {
                            this.state.update(cx, |state, cx| {
                                state.set_value(75.0, window, cx);
                            });
                            this.muted = false;
                        } else {
                            this.muted = !this.muted;
                        }
                        cx.notify();
                    })),
            )
            .child(div().w(px(112.0)).child(Slider::new(&self.state)))
            .child(
                div()
                    .w(px(36.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{}%", if silent { 0 } else { volume as u32 })),
            )
    }
}
