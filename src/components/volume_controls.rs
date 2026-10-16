use gpui_kit::component::{
    ActiveTheme as _, Icon,
    button::{Button, ButtonVariants},
    slider::{Slider, SliderEvent, SliderState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

const POPUP_HEIGHT: f32 = 160.0;
const CLOSE_DELAY: Duration = Duration::from_millis(500);

pub enum VolumeEvent {
    Volume(f64),
    Mute(bool),
}

pub struct VolumeControls {
    state: Entity<SliderState>,
    muted: bool,
    syncing: bool,
    hovered: bool,
    dragging: bool,
    close_task: Option<Task<()>>,
    focus: FocusHandle,
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
            hovered: false,
            dragging: false,
            close_task: None,
            focus: cx.focus_handle(),
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

    pub fn interacting(&self, window: &Window, cx: &App) -> bool {
        self.hovered
            || self.dragging
            || self.close_task.is_some()
            || self.focus.contains_focused(window, cx)
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.hovered == hovered {
            return;
        }
        self.hovered = hovered;
        self.close_task.take();
        if !hovered {
            self.close_task = Some(cx.spawn(async |this, cx| {
                cx.background_executor().timer(CLOSE_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    this.close_task = None;
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }
}

impl Render for VolumeControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let volume = self.state.read(cx).value().start();
        let open = self.interacting(window, cx);
        let entity = cx.weak_entity();
        div()
            .id("volume-control")
            .track_focus(&self.focus)
            .tab_index(0)
            .relative()
            .flex()
            .items_center()
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let hover_entity = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                let hovered = volume_hover_contains(bounds, event.position, open);
                                let _ = hover_entity
                                    .update(cx, |this, cx| this.set_hovered(hovered, cx));
                            }
                        });
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                let _ = entity.update(cx, |this, cx| {
                                    if this.dragging {
                                        this.dragging = false;
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let current = this.state.read(cx).value().start();
                let value = match event.keystroke.key.as_str() {
                    "up" | "right" => (current + 5.0).min(100.0),
                    "down" | "left" => (current - 5.0).max(0.0),
                    "home" => 0.0,
                    "end" => 100.0,
                    _ => return,
                };
                this.state
                    .update(cx, |state, cx| state.set_value(value, window, cx));
                cx.stop_propagation();
            }))
            .child(
                Button::new("mute")
                    .ghost()
                    .icon(Icon::new(volume_icon(self.muted, volume)))
                    // .tooltip("Mute / unmute (M)")
                    .on_click(
                        cx.listener(|this, _, _, cx| cx.emit(VolumeEvent::Mute(!this.muted))),
                    ),
            )
            .when(open, |control| {
                control.child(
                    deferred(
                        div()
                            .absolute()
                            .bottom_full()
                            .left(px(-4.0))
                            .w(px(44.0))
                            .pb_2()
                            .child(
                                div()
                                    .occlude()
                                    .w_full()
                                    .h(px(152.0))
                                    .py_4()
                                    .flex()
                                    .justify_center()
                                    .rounded_lg()
                                    .bg(cx.theme().popover)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .shadow_lg()
                                    .capture_any_mouse_down(cx.listener(
                                        |this, event: &MouseDownEvent, _, cx| {
                                            if event.button == MouseButton::Left {
                                                this.dragging = true;
                                                cx.notify();
                                            }
                                        },
                                    ))
                                    .child(Slider::new(&self.state).vertical().h(px(120.0))),
                            ),
                    )
                    .with_priority(20),
                )
            })
    }
}

fn volume_hover_contains(bounds: Bounds<Pixels>, cursor: Point<Pixels>, open: bool) -> bool {
    if !open {
        return bounds.contains(&cursor);
    }
    let expanded = Bounds {
        origin: point(
            bounds.origin.x - px(4.0),
            bounds.origin.y - px(POPUP_HEIGHT),
        ),
        size: size(
            px(44.0).max(bounds.size.width + px(4.0)),
            bounds.size.height + px(POPUP_HEIGHT),
        ),
    };
    expanded.contains(&cursor)
}

fn volume_icon(muted: bool, volume: f32) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName;
    if muted {
        IconName::VolumeX
    } else if volume < 75.0 {
        IconName::Volume1
    } else {
        IconName::Volume2
    }
}

#[cfg(test)]
mod tests {
    use super::{volume_hover_contains, volume_icon};
    use gpui_kit::assets::IconName;
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn closed_volume_only_opens_over_the_icon() {
        let bounds = Bounds::new(point(px(100.0), px(200.0)), size(px(32.0), px(32.0)));
        assert!(volume_hover_contains(
            bounds,
            point(px(116.0), px(216.0)),
            false
        ));
        assert!(!volume_hover_contains(
            bounds,
            point(px(116.0), px(100.0)),
            false
        ));
        assert!(!volume_hover_contains(
            bounds,
            point(px(98.0), px(216.0)),
            false
        ));
    }

    #[test]
    fn open_volume_hover_covers_slider_bridge_and_icon() {
        let bounds = Bounds::new(point(px(100.0), px(200.0)), size(px(32.0), px(32.0)));
        for (x, y) in [
            (116.0, 216.0),
            (116.0, 196.0),
            (116.0, 50.0),
            (97.0, 100.0),
            (139.0, 100.0),
        ] {
            assert!(volume_hover_contains(bounds, point(px(x), px(y)), true));
        }
        for (x, y) in [(95.0, 100.0), (141.0, 100.0), (116.0, 39.0), (116.0, 233.0)] {
            assert!(!volume_hover_contains(bounds, point(px(x), px(y)), true));
        }
    }

    #[test]
    fn volume_icons_respect_mute_and_the_75_percent_boundary() {
        assert_eq!(volume_icon(true, 100.0), IconName::VolumeX);
        assert_eq!(volume_icon(true, 0.0), IconName::VolumeX);
        assert_eq!(volume_icon(false, 0.0), IconName::Volume1);
        assert_eq!(volume_icon(false, 74.9), IconName::Volume1);
        assert_eq!(volume_icon(false, 75.0), IconName::Volume2);
        assert_eq!(volume_icon(false, 100.0), IconName::Volume2);
    }
}
