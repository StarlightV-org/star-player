use crate::playback::AudioTrack;
use gpui_kit::component::{
    ActiveTheme as _,
    slider::{Slider, SliderEvent, SliderState},
};
use gpui_kit::*;

pub struct TrackGainEvent {
    pub id: i64,
    pub gain: f64,
}
struct TrackRow {
    id: i64,
    title: String,
    gain: f64,
    slider: Entity<SliderState>,
}

pub struct AudioTracks {
    rows: Vec<TrackRow>,
    subscriptions: Vec<Subscription>,
    selected: usize,
}
impl EventEmitter<TrackGainEvent> for AudioTracks {}

impl AudioTracks {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            rows: Vec::new(),
            subscriptions: Vec::new(),
            selected: 0,
        }
    }

    pub fn sync(
        &mut self,
        tracks: &[AudioTrack],
        selected: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = selected;
        if self.rows.iter().map(|r| r.id).collect::<Vec<_>>()
            != tracks.iter().map(|r| r.id).collect::<Vec<_>>()
        {
            self.rows.clear();
            self.subscriptions.clear();
            for track in tracks {
                let slider = cx.new(|_| {
                    SliderState::new()
                        .min(0.0)
                        .max(100.0)
                        .step(1.0)
                        .default_value((track.gain * 100.0) as f32)
                });
                let id = track.id;
                self.subscriptions.push(cx.subscribe(
                    &slider,
                    move |_, _, event: &SliderEvent, cx| {
                        if let SliderEvent::Change(value) = event {
                            cx.emit(TrackGainEvent {
                                id,
                                gain: value.start() as f64 / 100.0,
                            });
                        }
                    },
                ));
                self.rows.push(TrackRow {
                    id,
                    title: track.title.clone(),
                    gain: track.gain,
                    slider,
                });
            }
        }
        for (row, track) in self.rows.iter_mut().zip(tracks) {
            row.title = track.title.clone();
            row.gain = track.gain;
            let value = (track.gain * 100.0) as f32;
            if (row.slider.read(cx).value().start() - value).abs() > 0.1 {
                row.slider
                    .update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        cx.notify();
    }
}

impl Render for AudioTracks {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("audio-tracks")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(150.0))
            .overflow_y_scroll()
            .children(self.rows.iter().enumerate().map(|(index, row)| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(220.0))
                            .text_xs()
                            .text_color(if index == self.selected {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(format!(
                                "{} {}",
                                if index == self.selected { ">" } else { " " },
                                row.title
                            )),
                    )
                    .child(div().flex_1().child(Slider::new(&row.slider)))
                    .child(
                        div()
                            .w(px(44.0))
                            .text_xs()
                            .child(format!("{}%", (row.gain * 100.0).round() as u32)),
                    )
            }))
    }
}
