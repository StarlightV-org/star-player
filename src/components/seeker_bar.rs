use crate::{preview::PreviewWorker, render_image};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Arc};

pub enum SeekerEvent {
    Seek(f64),
    Range(f64, f64),
    PreviewError(String),
}

pub struct SeekerBar {
    position: f64,
    duration: f64,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    path: Option<PathBuf>,
    previews: PreviewWorker,
    generation: u64,
    hover: Option<f64>,
    requested: Option<f64>,
    thumbnail: Option<Arc<RenderImage>>,
    thumbnail_time: f64,
    preview_error: Option<String>,
    chapters: Vec<crate::chapters::Chapter>,
    markers: Vec<crate::chapters::Point>,
    range: Option<(f64, f64)>,
    range_drag: Option<bool>,
}

impl EventEmitter<SeekerEvent> for SeekerBar {}

impl SeekerBar {
    pub fn new(_: f32, _: &mut Context<Self>) -> Self {
        Self {
            position: 0.0,
            duration: 0.0,
            bounds: Rc::new(Cell::new(Bounds::default())),
            path: None,
            previews: PreviewWorker::new(),
            generation: 0,
            hover: None,
            requested: None,
            thumbnail: None,
            thumbnail_time: 0.0,
            preview_error: None,
            chapters: Vec::new(),
            markers: Vec::new(),
            range: None,
            range_drag: None,
        }
    }

    pub fn set_media(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.previews.release();
        self.path = Some(path);
        self.generation += 1;
        self.thumbnail = None;
        self.preview_error = None;
        self.requested = None;
        self.hover = None;
        self.position = 0.0;
        self.duration = 0.0;
        self.chapters.clear();
        self.markers.clear();
        self.range = None;
        self.range_drag = None;
        cx.notify();
    }

    pub fn release_previews(&mut self) -> Arc<std::sync::atomic::AtomicBool> {
        self.requested = None;
        self.thumbnail = None;
        self.hover = None;
        self.previews.release()
    }

    pub fn suspend_previews(&mut self) -> Arc<std::sync::atomic::AtomicBool> {
        self.path = None;
        self.generation += 1;
        self.release_previews()
    }

    pub fn sync(&mut self, position: f64, duration: f64, cx: &mut Context<Self>) {
        self.position = position;
        self.duration = duration;
        if let Some(frame) = self.previews.take() {
            if frame.generation == self.generation {
                self.thumbnail_time = frame.seconds;
                match frame.image {
                    Ok(image) => {
                        self.thumbnail = Some(render_image(image));
                        self.preview_error = None;
                    }
                    Err(error) => {
                        self.thumbnail = None;
                        if self.preview_error.as_ref() != Some(&error) {
                            cx.emit(SeekerEvent::PreviewError(error.clone()));
                        }
                        self.preview_error = Some(error);
                    }
                }
            }
        }
        cx.notify();
    }

    pub fn set_range(&mut self, range: Option<(f64, f64)>, cx: &mut Context<Self>) {
        if self.range != range {
            self.range = range;
            if range.is_none() {
                self.range_drag = None;
            }
            cx.notify();
        }
    }

    pub fn set_annotations(
        &mut self,
        chapters: Vec<crate::chapters::Chapter>,
        markers: Vec<crate::chapters::Point>,
        cx: &mut Context<Self>,
    ) {
        if self.chapters != chapters || self.markers != markers {
            self.chapters = chapters;
            self.markers = markers;
            cx.notify();
        }
    }

    fn pointer_time(&self, x: Pixels) -> f64 {
        let bounds = self.bounds.get();
        hover_time(
            f32::from(x - bounds.origin.x),
            f32::from(bounds.size.width),
            self.duration,
        )
    }

    fn hover_at(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if self.duration <= 0.0 {
            return;
        }
        let seconds = self.pointer_time(x);
        self.hover = Some(seconds);
        // Half-second buckets avoid decoding on every pointer pixel.
        let bucket = (seconds * 2.0).floor() / 2.0;
        if self.requested != Some(bucket) {
            if let Some(path) = &self.path {
                self.previews.request(self.generation, path.clone(), bucket);
                self.requested = Some(bucket);
            }
        }
        cx.notify();
    }
}

fn hover_time(x: f32, width: f32, duration: f64) -> f64 {
    if width <= 0.0 || !duration.is_finite() {
        return 0.0;
    }
    (x / width).clamp(0.0, 1.0) as f64 * duration.max(0.0)
}

pub fn timestamp(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn chapter_segments(chapters: &[crate::chapters::Chapter], duration: f64) -> Vec<(f64, f64)> {
    if !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let mut boundaries = vec![0.0, duration];
    boundaries.extend(
        chapters
            .iter()
            .filter(|c| !c.disabled && c.time.is_finite() && c.time > 0.0 && c.time < duration)
            .map(|c| c.time),
    );
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();
    boundaries
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

impl Render for SeekerBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fraction = if self.duration > 0.0 {
            (self.position / self.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let bounds = self.bounds.clone();
        let width = f32::from(self.bounds.get().size.width);
        let hover = self.hover;
        let segments = chapter_segments(&self.chapters, self.duration);
        let segmented = segments.len() > 1;
        let range = self.range;
        let owner = cx.weak_entity();
        // The transparent GPUI composition layer hosts the preview above the
        // native video. No permanently reserved thumbnail strip is needed.
        div()
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .h(px(24.0))
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(timestamp(self.position)),
            )
            .child(
                div()
                    .id("seek-track")
                    .relative()
                    .flex_1()
                    .h(px(24.0))
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.hover_at(event.position.x, cx);
                        if this.range_drag.is_none()
                            && event.pressed_button == Some(MouseButton::Left)
                            && this.duration > 0.0
                        {
                            cx.emit(SeekerEvent::Seek(this.pointer_time(event.position.x)));
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            if this.duration > 0.0 {
                                cx.emit(SeekerEvent::Seek(this.pointer_time(event.position.x)));
                            }
                        }),
                    )
                    .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                        if !hovering {
                            this.hover = None;
                            cx.notify();
                        }
                    }))
                    .child(
                        canvas(
                            move |area, _, _| {
                                bounds.set(area);
                            },
                            move |_, _, window, _| {
                                let moving = owner.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, _, cx| {
                                        if phase != DispatchPhase::Capture {
                                            return;
                                        }
                                        let _ = moving.update(cx, |this, cx| {
                                            if let (Some(start_handle), Some((start, end))) =
                                                (this.range_drag, this.range)
                                            {
                                                if event.pressed_button != Some(MouseButton::Left) {
                                                    this.range_drag = None;
                                                    return;
                                                }
                                                let seconds = this.pointer_time(event.position.x);
                                                let range = if start_handle {
                                                    (seconds.min((end - 0.001).max(0.)), end)
                                                } else {
                                                    (
                                                        start,
                                                        seconds.max(
                                                            (start + 0.001).min(this.duration),
                                                        ),
                                                    )
                                                };
                                                this.range = Some(range);
                                                cx.emit(SeekerEvent::Range(range.0, range.1));
                                                cx.stop_propagation();
                                                cx.notify();
                                            }
                                        });
                                    },
                                );
                                let releasing = owner.clone();
                                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                    if phase == DispatchPhase::Capture
                                        && event.button == MouseButton::Left
                                    {
                                        let _ = releasing.update(cx, |this, cx| {
                                            if this.range_drag.take().is_some() {
                                                cx.stop_propagation();
                                                cx.notify();
                                            }
                                        });
                                    }
                                });
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when(segments.is_empty(), |bar| {
                        bar.child(
                            div()
                                .absolute()
                                .top(px(10.0))
                                .h(px(3.0))
                                .w_full()
                                .bg(cx.theme().muted),
                        )
                    })
                    .children(segments.iter().map(|(start, end)| {
                        let progress =
                            ((self.position - start) / (end - start)).clamp(0.0, 1.0) as f32;
                        div()
                            .absolute()
                            .top(px(9.5))
                            .h(px(4.0))
                            .left(relative((start / self.duration) as f32))
                            .w(relative(((end - start) / self.duration) as f32))
                            .pl(px(if segmented && *start > 0.0 { 1.5 } else { 0.0 }))
                            .pr(px(if segmented && *end < self.duration {
                                1.5
                            } else {
                                0.0
                            }))
                            .child(
                                div()
                                    .relative()
                                    .w_full()
                                    .h_full()
                                    .rounded_sm()
                                    .overflow_hidden()
                                    .bg(cx.theme().muted)
                                    .child(
                                        div()
                                            .absolute()
                                            .h_full()
                                            .w(relative(progress))
                                            .bg(cx.theme().primary),
                                    ),
                            )
                    }))
                    .when_some(range, |bar, (start, end)| {
                        bar.child(
                            div()
                                .absolute()
                                .top(px(3.))
                                .h(px(18.))
                                .left(relative((start / self.duration.max(0.001)) as f32))
                                .w(relative(((end - start) / self.duration.max(0.001)) as f32))
                                .bg(cx.theme().primary.opacity(0.25)),
                        )
                    })
                    .when(self.duration > 0.0, |bar| {
                        bar.child(
                            div()
                                .absolute()
                                .top(px(5.5))
                                .left(relative(fraction))
                                .ml(px(-6.0))
                                .size(px(12.0))
                                .rounded_full()
                                .bg(cx.theme().primary),
                        )
                    })
                    .when_some(hover, |bar, seconds| {
                        let left = ((seconds / self.duration.max(1.0)) as f32 * width - 120.0)
                            .clamp(0.0, (width - 240.0).max(0.0));
                        bar.child(
                            deferred(
                                div()
                                    .absolute()
                                    .bottom(px(38.0))
                                    .left(px(left))
                                    .w(px(240.0))
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                                    )
                                    .rounded_lg()
                                    .overflow_hidden()
                                    .bg(cx.theme().popover)
                                    .shadow_lg()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .when_some(self.thumbnail.clone(), |popup, image| {
                                        popup.child(
                                            img(image)
                                                .w_full()
                                                .h(px(135.0))
                                                .object_fit(ObjectFit::Contain),
                                        )
                                    })
                                    .when(
                                        self.thumbnail.is_none() && self.preview_error.is_none(),
                                        |popup| {
                                            popup.child(
                                                div()
                                                    .h(px(80.0))
                                                    .p_3()
                                                    .text_xs()
                                                    .child("Loading preview..."),
                                            )
                                        },
                                    )
                                    .children(
                                        self.chapters
                                            .iter()
                                            .filter(|c| {
                                                !c.disabled && c.time <= seconds && seconds < c.end
                                            })
                                            .map(|c| div().px_2().text_xs().child(c.title.clone())),
                                    )
                                    .children(
                                        self.markers
                                            .iter()
                                            .filter(|m| {
                                                (m.time - seconds).abs()
                                                    <= self.duration / f64::from(width.max(1.0))
                                                        * 8.0
                                            })
                                            .map(|m| {
                                                div()
                                                    .px_2()
                                                    .text_xs()
                                                    .child(format!("Marker: {}", m.title))
                                            }),
                                    )
                                    .child(div().p_2().text_xs().child(timestamp(
                                        if self.thumbnail.is_some() {
                                            self.thumbnail_time
                                        } else {
                                            seconds
                                        },
                                    ))),
                            )
                            .with_priority(10),
                        )
                    })
                    .when_some(range, |bar, (start, end)| {
                        bar.children([(true, start), (false, end)].into_iter().enumerate().map(
                            |(i, (start_handle, time))| {
                                div()
                                    .id(("clip-range-handle", i))
                                    .absolute()
                                    .top_0()
                                    .h(px(24.))
                                    .w(px(10.))
                                    .ml(px(-5.))
                                    .left(relative((time / self.duration.max(0.001)) as f32))
                                    .rounded_sm()
                                    .bg(cx.theme().primary)
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.range_drag = Some(start_handle);
                                            this.hover = None;
                                            cx.stop_propagation();
                                        }),
                                    )
                            },
                        ))
                    })
                    .children(
                        self.markers
                            .iter()
                            .filter(|m| self.duration > 0.0 && m.time < self.duration)
                            .map(|marker| {
                                let time = marker.time;
                                div()
                                    .id(("timeline-marker", marker.id))
                                    .absolute()
                                    .left(relative((time / self.duration).clamp(0.0, 1.0) as f32))
                                    .top(px(0.0))
                                    .w(px(8.0))
                                    .h(px(7.0))
                                    .rounded_sm()
                                    .bg(cx.theme().primary)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |_, _, _, cx| {
                                            cx.stop_propagation();
                                            cx.emit(SeekerEvent::Seek(time));
                                        }),
                                    )
                            }),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(timestamp(self.duration)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{chapter_segments, hover_time, timestamp};

    #[test]
    fn chapter_segments_cover_the_video_and_ignore_hidden_duplicates() {
        let mut settings = crate::chapters::FileSettings::default();
        settings.chapters = crate::chapters::import("0:10 One\n0:30 Two", 60.0, &[]).points;
        let metadata = std::collections::BTreeMap::from([(
            "chapter-list".into(),
            serde_json::json!([{"time":10},{"time":20}]),
        )]);
        assert_eq!(
            chapter_segments(&settings.chapters(&metadata, 60.0), 60.0),
            vec![(0.0, 10.0), (10.0, 20.0), (20.0, 30.0), (30.0, 60.0)]
        );
        settings.embedded_disabled = true;
        settings
            .options
            .entry("custom:1".into())
            .or_default()
            .disabled = true;
        assert_eq!(
            chapter_segments(&settings.chapters(&metadata, 60.0), 60.0),
            vec![(0.0, 30.0), (30.0, 60.0)]
        );
        assert_eq!(chapter_segments(&[], 60.0), vec![(0.0, 60.0)]);
        assert!(chapter_segments(&[], 0.0).is_empty());
        assert!(chapter_segments(&[], f64::NAN).is_empty());
    }
    #[test]
    fn formats_timestamps() {
        assert_eq!(timestamp(65.0), "01:05");
        assert_eq!(timestamp(3661.0), "1:01:01");
        assert_eq!(timestamp(-1.0), "00:00");
    }
    #[test]
    fn maps_pointer_to_clamped_time() {
        assert_eq!(hover_time(50.0, 100.0, 300.0), 150.0);
        assert_eq!(hover_time(-10.0, 100.0, 300.0), 0.0);
        assert_eq!(hover_time(200.0, 100.0, 300.0), 300.0);
        assert_eq!(hover_time(10.0, 0.0, 300.0), 0.0);
    }
}
