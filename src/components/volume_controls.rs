use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants},
    slider::{Slider, SliderEvent, SliderState},
};
use gpui_kit::*;

pub enum VolumeEvent {
    Volume(f64),
    Mute(bool),
}

pub struct VolumeControls {
    state: Entity<SliderState>,
    muted: bool,
    syncing: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<VolumeEvent> for VolumeControls {}

impl VolumeControls {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(100.0)
        });
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |this, _, event: &SliderEvent, cx| {
                if !this.syncing {
                    if let SliderEvent::Change(value) = event {
                        cx.emit(VolumeEvent::Volume(value.start() as f64));
                    }
                }
            }),
        ];
        Self {
            state,
            muted: false,
            syncing: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn sync(&mut self, volume: f64, muted: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.syncing = true;
        self.muted = muted;
        if (self.state.read(cx).value().start() - volume as f32).abs() > 0.1 {
            self.state
                .update(cx, |state, cx| state.set_value(volume as f32, window, cx));
        }
        self.syncing = false;
        cx.notify();
    }
}

impl Render for VolumeControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let volume = self.state.read(cx).value().start();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("mute")
                    .ghost()
                    .label(if self.muted { "Muted" } else { "Sound" })
                    .tooltip("Mute / unmute (M)")
                    .on_click(
                        cx.listener(|this, _, _, cx| cx.emit(VolumeEvent::Mute(!this.muted))),
                    ),
            )
            .child(div().w(px(88.0)).child(Slider::new(&self.state)))
            .child(
                div()
                    .w(px(36.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{}%", if self.muted { 0 } else { volume as u32 })),
            )
    }
}
