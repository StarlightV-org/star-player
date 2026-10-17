use super::{audio_tracks::AudioTracks, seeker_bar::timestamp};
use crate::chapters::{self, Chapter, FileSettings, Import, Point};
use gpui_kit::{
    base::{Disableable as _, ElementExt as _},
    component::{
        ActiveTheme as _,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
    },
    prelude::FluentBuilder as _,
    *,
};
use std::{cell::Cell, path::PathBuf, rc::Rc};

pub enum FileSettingsEvent {
    Save(PathBuf, FileSettings),
    Seek(f64),
}

pub struct FileSettingsPanel {
    path: Option<PathBuf>,
    settings: FileSettings,
    chapters: Vec<Chapter>,
    position: f64,
    duration: f64,
    tab: usize,
    audio: Entity<AudioTracks>,
    paste: Entity<TextareaState>,
    title: Entity<InputState>,
    time: Entity<InputState>,
    preview: Option<Import>,
    editing: Option<(bool, u64)>,
    show_editor: bool,
    show_import: bool,
    body_height: Rc<Cell<Pixels>>,
    body_scroll: ScrollHandle,
    tab_bounds: Rc<Cell<Bounds<Pixels>>>,
    message: Option<String>,
    _paste_subscription: Subscription,
}

impl EventEmitter<FileSettingsEvent> for FileSettingsPanel {}

impl FileSettingsPanel {
    pub fn new(audio: Entity<AudioTracks>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paste = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 6)
                .placeholder("0:50 Chapter title\nAnother chapter 5:00")
        });
        let paste_subscription = cx.subscribe(&paste, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.preview = Some(chapters::import(
                    &input.read(cx).value(),
                    this.duration,
                    &this.settings.chapters,
                ));
                cx.notify();
            }
        });
        Self {
            path: None,
            settings: FileSettings::default(),
            chapters: Vec::new(),
            position: 0.0,
            duration: 0.0,
            tab: 0,
            audio,
            paste,
            title: cx.new(|cx| InputState::new(window, cx).placeholder("Title")),
            time: cx.new(|cx| InputState::new(window, cx).placeholder("0:00")),
            preview: None,
            editing: None,
            show_editor: false,
            show_import: false,
            body_height: Rc::new(Cell::new(px(0.0))),
            body_scroll: ScrollHandle::new(),
            tab_bounds: Rc::new(Cell::new(Bounds::default())),
            message: None,
            _paste_subscription: paste_subscription,
        }
    }

    pub fn sync(
        &mut self,
        path: Option<PathBuf>,
        settings: FileSettings,
        chapters: Vec<Chapter>,
        position: f64,
        duration: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.path != path || self.settings != settings || self.chapters != chapters;
        let refresh_preview = self.settings != settings || self.duration != duration;
        if self.path != path {
            self.preview = None;
            self.editing = None;
            self.show_editor = false;
            self.show_import = false;
            self.message = None;
            self.paste.update(cx, |s, cx| s.set_value("", window, cx));
            self.title.update(cx, |s, cx| s.set_value("", window, cx));
            self.time.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.path = path;
        self.settings = settings;
        self.chapters = chapters;
        self.position = position;
        self.duration = duration;
        if refresh_preview && self.preview.is_some() {
            self.preview = Some(chapters::import(
                &self.paste.read(cx).value(),
                duration,
                &self.settings.chapters,
            ));
        }
        if changed {
            cx.notify();
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.path {
            cx.emit(FileSettingsEvent::Save(path.clone(), self.settings.clone()));
        }
        cx.notify();
    }

    fn edit(&mut self, marker: bool, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let points = if marker {
            &self.settings.markers
        } else {
            &self.settings.chapters
        };
        if let Some(point) = points.iter().find(|p| p.id == id) {
            let title = point.title.clone();
            let time = timestamp(point.time);
            self.title
                .update(cx, |s, cx| s.set_value(title, window, cx));
            self.time.update(cx, |s, cx| s.set_value(time, window, cx));
            self.editing = Some((marker, id));
            self.show_editor = true;
            self.show_import = false;
            self.message = None;
            cx.notify();
        }
    }

    fn save_point(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().trim().to_owned();
        let value = self.time.read(cx).value();
        let original = self.editing.and_then(|(marker, id)| {
            let points = if marker {
                &self.settings.markers
            } else {
                &self.settings.chapters
            };
            points.iter().find(|p| p.id == id).map(|p| p.time)
        });
        let time = if value.trim().is_empty() {
            Some(self.position)
        } else if original.is_some_and(|time| timestamp(time) == value.trim()) {
            original
        } else {
            chapters::parse_time(value.trim())
        };
        let Some(time) = time.filter(|t| self.duration > 0.0 && *t < self.duration) else {
            self.message = Some("Enter a valid timestamp within the video".into());
            cx.notify();
            return;
        };
        if title.is_empty() {
            self.message = Some("Enter a title".into());
            cx.notify();
            return;
        }
        let marker = self.editing.map(|p| p.0).unwrap_or(self.tab == 2);
        let points = if marker {
            &mut self.settings.markers
        } else {
            &mut self.settings.chapters
        };
        if let Some((_, id)) = self.editing {
            if let Some(p) = points.iter_mut().find(|p| p.id == id) {
                p.time = time;
                p.title = title;
            }
        } else {
            let id = points
                .iter()
                .map(|p| p.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            points.push(Point { id, time, title });
        }
        points.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.editing = None;
        self.show_editor = false;
        self.preview = None;
        self.message = None;
        self.title.update(cx, |s, cx| s.set_value("", window, cx));
        self.time.update(cx, |s, cx| s.set_value("", window, cx));
        self.save(cx);
    }
}

impl FileSettingsPanel {
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut content = div()
            .id("file-settings-body-content")
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_3();
        if self.tab == 0 {
            return content.child(self.audio.clone());
        }
        let list_limit = (window.viewport_size().height
            - px(if self.show_editor { 400.0 } else { 280.0 }))
        .max(px(40.0))
        .min(px(if self.show_editor { 120.0 } else { 260.0 }));
        let row_height = if self.tab == 1 { 56.0 } else { 40.0 };
        let row_count = if self.tab == 1 {
            self.chapters.len()
        } else {
            self.settings.markers.len()
        };
        let mut list = div()
            .id("annotation-list")
            .flex()
            .flex_col()
            .min_w_0()
            .max_h(list_limit)
            .when(px(row_count as f32 * row_height) > list_limit, |list| {
                list.overflow_y_scroll()
            });
        content = content.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.tab == 1 {
                            format!("{} chapters", self.chapters.len())
                        } else {
                            format!("{} markers", self.settings.markers.len())
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .when(self.tab == 1, |row| {
                            row.child(
                                Button::new("toggle-import")
                                    .ghost()
                                    .label(if self.show_import {
                                        "Close import"
                                    } else {
                                        "Import"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_import = !this.show_import;
                                        this.show_editor = false;
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(
                            Button::new("new-point")
                                .ghost()
                                .label(if self.tab == 1 {
                                    "+ Chapter"
                                } else {
                                    "+ Marker"
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editing = None;
                                    this.show_editor = true;
                                    this.show_import = false;
                                    this.message = None;
                                    this.title.update(cx, |s, cx| s.set_value("", window, cx));
                                    this.time.update(cx, |s, cx| s.set_value("", window, cx));
                                    cx.notify();
                                })),
                        ),
                ),
        );
        if self.tab == 1 {
            content = content.child(
                Checkbox::new("embedded-enabled")
                    .label("Show embedded chapters")
                    .checked(!self.settings.embedded_disabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.settings.embedded_disabled = !*checked;
                        this.save(cx);
                    })),
            );
            if self.show_import {
                content = content
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Paste timestamps before or after each chapter title."),
                    )
                    .child(Textarea::new(&self.paste));
            }
            if let Some(preview) = &self.preview
                && self.show_import
            {
                content = content
                    .child(
                        div()
                            .text_xs()
                            .child(format!("{} new chapters", preview.points.len())),
                    )
                    .children(preview.issues.iter().map(|issue| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(issue.clone())
                    }))
                    .child(
                        div()
                            .id("chapter-import-preview")
                            .flex()
                            .flex_col()
                            .gap_1()
                            .max_h(px(160.0))
                            .when(preview.points.len() > 6, |list| list.overflow_y_scroll())
                            .children(preview.points.iter().map(|p| {
                                div().flex_shrink_0().h(px(20.0)).text_xs().child(format!(
                                    "{}  {}",
                                    timestamp(p.time),
                                    p.title
                                ))
                            })),
                    );
                if !preview.points.is_empty() {
                    content = content.child(
                        Button::new("confirm-import")
                            .primary()
                            .disabled(self.duration <= 0.0)
                            .label("Confirm import")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(preview) = this.preview.take() {
                                    this.settings.chapters.extend(preview.points);
                                }
                                this.paste.update(cx, |s, cx| s.set_value("", window, cx));
                                this.show_import = false;
                                this.save(cx);
                            })),
                    );
                }
            }
            for chapter in self.chapters.clone() {
                let key = chapter.key.clone();
                let skip_key = key.clone();
                let id = chapter.custom_id;
                let time = chapter.time;
                let hidden = self
                    .settings
                    .options
                    .get(&key)
                    .is_some_and(|options| options.disabled);
                let owner = cx.entity();
                let row =
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .h(px(56.0))
                        .min_w_0()
                        .gap_1()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .child(
                            Button::new(SharedString::from(format!("seek-{key}")))
                                .ghost()
                                .flex_1()
                                .min_w_0()
                                .h_auto()
                                .py_2()
                                .disabled(chapter.disabled)
                                .child(
                                    div()
                                        .flex()
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(52.0))
                                                .flex_shrink_0()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(timestamp(time)),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .min_w_0()
                                                .flex_1()
                                                .items_start()
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .text_sm()
                                                        .truncate()
                                                        .child(chapter.title),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!(
                                                            "{}{}{}",
                                                            if id.is_some() {
                                                                "Custom"
                                                            } else {
                                                                "Embedded"
                                                            },
                                                            if chapter.disabled {
                                                                " · Hidden"
                                                            } else {
                                                                ""
                                                            },
                                                            if chapter.auto_skip {
                                                                " · Auto-skip"
                                                            } else {
                                                                ""
                                                            }
                                                        )),
                                                ),
                                        ),
                                )
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(FileSettingsEvent::Seek(time))
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("chapter-menu-{key}")))
                                .ghost()
                                .label("...")
                                .tooltip("Chapter actions")
                                .dropdown_menu(move |menu, _, _| {
                                    let enabled_owner = owner.clone();
                                    let enabled_key = key.clone();
                                    let skip_owner = owner.clone();
                                    let skip_key = skip_key.clone();
                                    let mut menu = menu
                                        .item(
                                            PopupMenuItem::new(if hidden {
                                                "Show chapter"
                                            } else {
                                                "Hide chapter"
                                            })
                                            .on_click(move |_, _, cx| {
                                                enabled_owner.update(cx, |this, cx| {
                                                    let options = this
                                                        .settings
                                                        .options
                                                        .entry(enabled_key.clone())
                                                        .or_default();
                                                    options.disabled = !options.disabled;
                                                    this.save(cx);
                                                })
                                            }),
                                        )
                                        .item(
                                            PopupMenuItem::new(if chapter.auto_skip {
                                                "Turn off auto-skip"
                                            } else {
                                                "Auto-skip chapter"
                                            })
                                            .on_click(move |_, _, cx| {
                                                skip_owner.update(cx, |this, cx| {
                                                    let options = this
                                                        .settings
                                                        .options
                                                        .entry(skip_key.clone())
                                                        .or_default();
                                                    options.auto_skip = !options.auto_skip;
                                                    this.save(cx);
                                                })
                                            }),
                                        );
                                    if let Some(id) = id {
                                        let edit_owner = owner.clone();
                                        let delete_owner = owner.clone();
                                        menu = menu
                                            .separator()
                                            .item(PopupMenuItem::new("Edit chapter").on_click(
                                                move |_, window, cx| {
                                                    edit_owner.update(cx, |this, cx| {
                                                        this.edit(false, id, window, cx)
                                                    })
                                                },
                                            ))
                                            .item(PopupMenuItem::new("Delete chapter").on_click(
                                                move |_, _, cx| {
                                                    delete_owner.update(cx, |this, cx| {
                                                        this.settings
                                                            .chapters
                                                            .retain(|p| p.id != id);
                                                        this.settings
                                                            .options
                                                            .remove(&format!("custom:{id}"));
                                                        this.preview = None;
                                                        this.editing = None;
                                                        this.show_editor = false;
                                                        this.save(cx);
                                                    })
                                                },
                                            ));
                                    }
                                    menu
                                }),
                        );
                list = list.child(row);
            }
        } else {
            for point in self.settings.markers.clone() {
                let id = point.id;
                let time = point.time;
                let owner = cx.entity();
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .h(px(40.0))
                        .min_w_0()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .gap_1()
                        .child(
                            Button::new(("seek-marker", id))
                                .ghost()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(
                                    div()
                                        .flex()
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(52.0))
                                                .flex_shrink_0()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(timestamp(time)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_sm()
                                                .truncate()
                                                .child(point.title),
                                        ),
                                )
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(FileSettingsEvent::Seek(time))
                                })),
                        )
                        .child(
                            Button::new(("marker-menu", id))
                                .ghost()
                                .label("...")
                                .tooltip("Marker actions")
                                .dropdown_menu(move |menu, _, _| {
                                    let edit_owner = owner.clone();
                                    let delete_owner = owner.clone();
                                    menu.item(PopupMenuItem::new("Edit marker").on_click(
                                        move |_, window, cx| {
                                            edit_owner.update(cx, |this, cx| {
                                                this.edit(true, id, window, cx)
                                            })
                                        },
                                    ))
                                    .item(
                                        PopupMenuItem::new("Delete marker").on_click(
                                            move |_, _, cx| {
                                                delete_owner.update(cx, |this, cx| {
                                                    this.settings.markers.retain(|p| p.id != id);
                                                    this.editing = None;
                                                    this.show_editor = false;
                                                    this.save(cx);
                                                })
                                            },
                                        ),
                                    )
                                }),
                        ),
                );
            }
        }
        if self.show_import {
            return content;
        }
        if (self.tab == 1 && self.chapters.is_empty())
            || (self.tab == 2 && self.settings.markers.is_empty())
        {
            content = content.child(
                div()
                    .py_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.tab == 1 {
                        "No chapters yet. Import a list or add one at the playhead."
                    } else {
                        "No markers yet. Bookmark a moment with + Marker."
                    }),
            );
        } else {
            content = content.child(list);
        }
        if !self.show_editor {
            return content;
        }
        content
            .child(div().text_xs().child(if self.editing.is_some() {
                "Edit selected item"
            } else {
                "Add at playhead, or enter a timestamp"
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.0))
                            .flex_shrink_0()
                            .child(Input::new(&self.time)),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&self.title))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("save-point")
                            .primary()
                            .label(if self.editing.is_some() {
                                "Save changes"
                            } else if self.tab == 2 {
                                "Add marker"
                            } else {
                                "Add chapter"
                            })
                            .on_click(cx.listener(|this, _, w, cx| this.save_point(w, cx))),
                    )
                    .child(Button::new("cancel-edit").ghost().label("Cancel").on_click(
                        cx.listener(|this, _, window, cx| {
                            this.editing = None;
                            this.show_editor = false;
                            this.message = None;
                            this.title.update(cx, |s, cx| s.set_value("", window, cx));
                            this.time.update(cx, |s, cx| s.set_value("", window, cx));
                            cx.notify();
                        }),
                    )),
            )
            .when_some(self.message.clone(), |view, message| {
                view.child(div().text_xs().child(message))
            })
    }
}

impl Render for FileSettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let limit = (window.viewport_size().height - px(260.0)).max(px(64.0));
        let needs_scroll = self.body_height.get() > limit;
        if !needs_scroll {
            self.body_scroll.set_offset(gpui_kit::Point::default());
        }
        let height = self.body_height.clone();
        let owner = cx.weak_entity();
        let body = self
            .render_body(window, cx)
            .on_prepaint(move |bounds, _, cx| {
                if height.replace(bounds.size.height) != bounds.size.height {
                    let _ = owner.update(cx, |_, cx| cx.notify());
                }
            });
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_3()
            .child(
                div()
                    .flex()
                    .w_full()
                    .flex_shrink_0()
                    .gap_1()
                    .p_1()
                    .rounded_lg()
                    .bg(cx.theme().muted.opacity(0.35))
                    .on_prepaint({
                        let bounds = self.tab_bounds.clone();
                        move |area, _, _| bounds.set(area)
                    })
                    .children(
                        ["Audio", "Chapters", "Markers"]
                            .into_iter()
                            .enumerate()
                            .map(|(index, title)| {
                                Button::new(("file-tab", index))
                                    .ghost()
                                    .flex_1()
                                    .when(index == self.tab, |button| button.primary())
                                    .label(title)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.tab = index;
                                        this.editing = None;
                                        this.show_editor = false;
                                        this.show_import = false;
                                        this.message = None;
                                        this.body_scroll.set_offset(gpui_kit::Point::default());
                                        this.title.update(cx, |s, cx| s.set_value("", window, cx));
                                        this.time.update(cx, |s, cx| s.set_value("", window, cx));
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .child(
                div()
                    .id(("file-settings-body", self.tab))
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .max_h(limit)
                    .overflow_hidden()
                    .when(needs_scroll, |body| body.overflow_y_scroll())
                    .track_scroll(&self.body_scroll)
                    .child(body),
            )
    }
}

#[cfg(test)]
impl FileSettingsPanel {
    pub fn scroll_offset(&self) -> gpui_kit::Point<Pixels> {
        self.body_scroll.offset()
    }
    pub fn active_tab(&self) -> usize {
        self.tab
    }
    pub fn tab_bounds(&self, index: usize) -> Bounds<Pixels> {
        let bounds = self.tab_bounds.get();
        let width = bounds.size.width / 3.0;
        Bounds::new(
            gpui_kit::point(bounds.origin.x + width * index as f32, bounds.origin.y),
            size(width, bounds.size.height),
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{AudioTracks, FileSettings, FileSettingsPanel, InputEvent, PathBuf};
    use gpui_kit::{AppContext as _, WindowOptions, application, init};

    #[test]
    #[ignore = "interactive GPUI file settings render and editing smoke test"]
    fn file_settings_render_edit_and_switch_files() {
        application()
            .with_assets(crate::app_assets::AppAssets)
            .run(|cx| {
                init(cx);
                crate::theme::apply(cx);
                let mut panel = None;
                let handle = cx
                    .open_window(WindowOptions::default(), |window, cx| {
                        let audio = cx.new(AudioTracks::new);
                        let view = cx.new(|cx| FileSettingsPanel::new(audio, window, cx));
                        panel = Some(view.clone());
                        cx.new(|cx| gpui_kit::base::Root::new(view, window, cx))
                    })
                    .unwrap();
                let panel = panel.unwrap();
                cx.spawn(async move |cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| {
                                let settings = FileSettings {
                                    chapters: crate::chapters::import(
                                        "0:20 Existing custom chapter",
                                        100.0,
                                        &[],
                                    )
                                    .points,
                                    ..Default::default()
                                };
                                let metadata = std::collections::BTreeMap::from([(
                                    "chapter-list".into(),
                                    serde_json::json!([{"time":0,"title":"Embedded chapter"}]),
                                )]);
                                let chapters = settings.chapters(&metadata, 100.0);
                                panel.sync(
                                    Some(PathBuf::from("one.mp4")),
                                    settings,
                                    chapters,
                                    12.75,
                                    100.0,
                                    window,
                                    cx,
                                );
                                panel.tab = 2;
                                panel
                                    .title
                                    .update(cx, |s, cx| s.set_value("Bookmark", window, cx));
                                panel.save_point(window, cx);
                                assert_eq!(panel.settings.markers[0].time, 12.75);
                                let id = panel.settings.markers[0].id;
                                panel.edit(true, id, window, cx);
                                panel
                                    .title
                                    .update(cx, |s, cx| s.set_value("Renamed", window, cx));
                                panel.save_point(window, cx);
                                assert_eq!(panel.settings.markers[0].time, 12.75);
                                assert_eq!(panel.settings.markers[0].title, "Renamed");
                                panel.edit(true, id, window, cx);
                                cx.notify();
                            })
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, _, cx| {
                            panel.update(cx, |panel, cx| {
                                panel.tab = 1;
                                panel.show_editor = false;
                                cx.notify();
                            })
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| {
                                panel.show_import = true;
                                panel.paste.update(cx, |s, cx| {
                                    s.set_value("0:10 First\nLast 1:00", window, cx);
                                    cx.emit(InputEvent::Change);
                                });
                                cx.notify();
                            })
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| {
                                assert_eq!(panel.preview.as_ref().unwrap().points.len(), 2);
                                panel.paste.update(cx, |s, cx| {
                                    s.set_value("invalid chapter\n".repeat(100), window, cx);
                                    cx.emit(InputEvent::Change);
                                });
                            })
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| {
                                assert!(
                                    panel.body_height.get()
                                        > (window.viewport_size().height - gpui_kit::px(260.0))
                                            .max(gpui_kit::px(64.0)),
                                    "Long import warnings must remain scrollable"
                                );
                                panel.sync(
                                    Some(PathBuf::from("two.mp4")),
                                    FileSettings::default(),
                                    Vec::new(),
                                    0.0,
                                    60.0,
                                    window,
                                    cx,
                                );
                                assert!(panel.preview.is_none());
                                assert!(panel.settings.markers.is_empty());
                                assert!(panel.paste.read(cx).value().is_empty());
                                panel.tab = 2;
                                cx.notify();
                            })
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
                cx.activate(true);
            });
    }
}
