#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use components::{
    audio_tracks::{AudioTracks, TrackGainEvent},
    clip_browser::{BrowserEvent, ClipBrowser},
    media_controls::{MediaControls, MediaControlsEvent},
    seeker_bar::{SeekerBar, SeekerEvent},
    volume_controls::{VolumeControls, VolumeEvent},
};
use gpui_kit::base::Disableable as _;
use gpui_kit::component::{
    ActiveTheme as _, IconName, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use playback::{Command, Playback, Snapshot};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

mod app_assets;
mod app_icon;
mod clip_library;
mod clip_store;
mod components;
mod logging;
mod media_keys;
mod native_video;
mod playback;
mod preview;
mod runtime;
mod theme;
mod window_cursor;

/// GPUI's texture upload expects BGRA, while our decoders produce RGBA.
fn render_image(mut pixels: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
        pixels
    )]))
}

struct PlayerShell {
    browser: Entity<ClipBrowser>,
    navigation: Vec<PathBuf>,
    played_recorded: bool,
    hover_navigation: Option<bool>,
    playback: Option<Playback>,
    snapshot: Snapshot,
    native_video: Option<Rc<native_video::NativeVideo>>,
    path: Option<PathBuf>,
    error: Option<String>,
    focus: FocusHandle,
    media_controls: Entity<MediaControls>,
    volume_controls: Entity<VolumeControls>,
    seeker_bar: Entity<SeekerBar>,
    audio_tracks: Entity<AudioTracks>,
    selected_track: usize,
    show_help: bool,
    show_tracks: bool,
    controls_visible: bool,
    controls_dragging: bool,
    last_activity: Instant,
    video_click: Option<Task<()>>,
    media_keys: media_keys::MediaKeys,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl PlayerShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let browser = cx.new(|cx| ClipBrowser::new(std::env::args_os().nth(1).is_none(), cx));
        let media_controls = cx.new(|_| MediaControls::default());
        let volume_controls = cx.new(VolumeControls::new);
        let seeker_bar = cx.new(|cx| SeekerBar::new(0.0, cx));
        let audio_tracks = cx.new(AudioTracks::new);
        let subscriptions = vec![
            cx.subscribe_in(
                &browser,
                window,
                |this, _, event: &BrowserEvent, window, cx| match event {
                    BrowserEvent::Play(path, list) => {
                        this.navigation = list.clone();
                        this.load(path.clone(), cx);
                        window.focus(&this.focus, cx);
                    }
                    BrowserEvent::Delete(paths) => {
                        if this.path.as_ref().is_some_and(|path| paths.contains(path)) {
                            this.back_to_library(window, cx);
                        }
                        let released = this
                            .seeker_bar
                            .update(cx, |seeker, _| seeker.release_previews());
                        this.browser.read(cx).delete(paths.clone(), released);
                    }
                    BrowserEvent::Open => this.open(window, cx),
                },
            ),
            cx.subscribe_in(
                &media_controls,
                window,
                |this, _, event: &MediaControlsEvent, _, cx| {
                    this.command(
                        match event {
                            MediaControlsEvent::Toggle => Command::Toggle,
                            MediaControlsEvent::SeekBackward => Command::SeekRelative(-10.0),
                            MediaControlsEvent::SeekForward => Command::SeekRelative(10.0),
                            MediaControlsEvent::Stop => Command::Stop,
                        },
                        cx,
                    );
                },
            ),
            cx.subscribe_in(
                &volume_controls,
                window,
                |this, _, event: &VolumeEvent, _, cx| {
                    this.command(
                        match event {
                            VolumeEvent::Volume(v) => Command::SetVolume(*v),
                            VolumeEvent::Mute(m) => Command::SetMute(*m),
                        },
                        cx,
                    );
                },
            ),
            cx.subscribe_in(
                &seeker_bar,
                window,
                |this, _, event: &SeekerEvent, _, cx| {
                    let SeekerEvent::Seek(seconds) = event;
                    this.command(Command::SeekAbsolute(*seconds), cx);
                },
            ),
            cx.subscribe_in(
                &audio_tracks,
                window,
                |this, _, event: &TrackGainEvent, _, cx| {
                    this.command(
                        Command::SetTrackGain {
                            id: event.id,
                            gain: event.gain,
                        },
                        cx,
                    );
                },
            ),
        ];
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let (native_video, playback, error) = match native_video::NativeVideo::new(window) {
            Ok(host) => {
                let host = Rc::new(host);
                match Playback::native(host.hwnd()) {
                    Ok(player) => (Some(host), Some(player), None),
                    Err(error) => (Some(host), None, Some(error)),
                }
            }
            Err(error) => (None, None, Some(error)),
        };
        let shell_entity = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = shell_entity.update(cx, |shell, _| shell.shutdown());
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let shell = Self {
            browser,
            navigation: vec![],
            played_recorded: false,
            hover_navigation: None,
            playback,
            snapshot: Snapshot::default(),
            native_video,
            path: None,
            error,
            focus,
            media_controls,
            volume_controls,
            seeker_bar,
            audio_tracks,
            selected_track: 0,
            show_help: false,
            show_tracks: false,
            controls_visible: true,
            controls_dragging: false,
            last_activity: Instant::now(),
            video_click: None,
            media_keys: media_keys::MediaKeys::default(),
            _subscriptions: subscriptions,
            _poll: poll,
        };
        shell
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        self.video_click.take();
        self.reveal_controls(cx);
        if let Some(player) = &self.playback {
            if let Err(error) = player.send(command) {
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    fn reveal_controls(&mut self, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        if let Some(host) = &self.native_video {
            host.set_cursor_hidden(false);
        }
        if !self.controls_visible {
            self.controls_visible = true;
            cx.notify();
        }
    }

    fn shutdown(&mut self) {
        self.video_click.take();
        drop(self.playback.take());
        if let Some(host) = self.native_video.take() {
            host.destroy();
        }
    }

    fn click_video(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.reveal_controls(cx);
        self.video_click.take();
        if event.click_count == 2 {
            window.zoom_window();
        } else if event.click_count == 1 && self.path.is_some() {
            // Wait out the OS double-click interval. Maximizing must not also
            // toggle playback on the first click of the pair.
            self.video_click = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(double_click_interval())
                    .await;
                let _ = this.update_in(cx, |this, _, cx| this.command(Command::Toggle, cx));
            }));
        }
        cx.stop_propagation();
    }

    fn click_overlay(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A previous background click may still be waiting for the double-click
        // interval. Interacting with a panel cancels it as well as blocking any
        // new click from reaching the video sibling behind this panel.
        self.video_click.take();
        self.reveal_controls(cx);
        cx.stop_propagation();
    }

    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.played_recorded = false;
        self.hover_navigation = None;
        if self.playback.is_none() {
            let Some(host) = &self.native_video else {
                cx.notify();
                return;
            };
            match Playback::native(host.hwnd()) {
                Ok(player) => self.playback = Some(player),
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        }
        self.error = None;
        self.snapshot = Snapshot::default();
        self.selected_track = 0;
        self.show_tracks = false;
        self.path = Some(path.clone());
        self.seeker_bar
            .update(cx, |seeker, cx| seeker.set_media(path.clone(), cx));
        self.command(Command::Load(path), cx);
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open media".into()),
        });
        cx.spawn_in(window, async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.navigation.clear();
                        this.load(path, cx);
                        window.focus(&this.focus, cx);
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.error = Some(error.to_string());
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| {
            browser.poll(self.path.is_none(), window, cx)
        });
        if let Some(player) = &self.playback {
            self.snapshot = player.snapshot();
        }
        if !self.played_recorded
            && self.path.is_some()
            && self.snapshot.path == self.path
            && self.snapshot.playback_started
            && self.snapshot.error.is_none()
        {
            self.played_recorded = true;
            self.browser.update(cx, |browser, _| {
                browser.mark_played(self.path.clone().unwrap())
            });
        }
        if self.path.is_some() && !self.navigation.is_empty() {
            self.navigation.retain(|path| {
                Some(path) == self.path.as_ref() || self.browser.read(cx).exists(path)
            });
        }
        let snapshot = &self.snapshot;
        self.media_controls.update(cx, |controls, cx| {
            controls.set_playing(!snapshot.paused, cx)
        });
        self.volume_controls.update(cx, |controls, cx| {
            controls.sync(snapshot.volume, snapshot.muted, window, cx)
        });
        self.seeker_bar.update(cx, |seeker, cx| {
            seeker.sync(snapshot.position, snapshot.duration, cx)
        });
        self.selected_track = self
            .selected_track
            .min(snapshot.tracks.len().saturating_sub(1));
        self.audio_tracks.update(cx, |tracks, cx| {
            tracks.sync(&snapshot.tracks, self.selected_track, window, cx)
        });
        for action in self.media_keys.poll() {
            use media_keys::MediaAction;
            let command = match action {
                MediaAction::Toggle => Command::Toggle,
                MediaAction::Stop => Command::Stop,
                MediaAction::Backward => Command::SeekRelative(-10.0),
                MediaAction::Forward => Command::SeekRelative(10.0),
                MediaAction::Mute => Command::SetMute(!self.snapshot.muted),
                MediaAction::VolumeDown => {
                    Command::SetVolume((self.snapshot.volume - 5.0).max(0.0))
                }
                MediaAction::VolumeUp => {
                    Command::SetVolume((self.snapshot.volume + 5.0).min(100.0))
                }
            };
            self.command(command, cx);
        }
        self.controls_visible = controls_should_show(
            self.path.is_none()
                || self.snapshot.paused
                || self.hover_navigation.is_some()
                || self.error.is_some()
                || self.snapshot.error.is_some(),
            self.controls_dragging,
            self.last_activity.elapsed(),
        );
        if let Some(host) = &self.native_video {
            host.set_cursor_hidden(!self.controls_visible);
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal_controls(cx);
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control && key == "o" {
            self.open(window, cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control && modifiers.shift && key == "t" {
            theme::apply(cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        if self.path.is_none() {
            return;
        }
        let command = match key {
            "space" | "k" => Some(Command::Toggle),
            "p" => Some(Command::Play),
            "pause" => Some(Command::Pause),
            "s" => Some(Command::Stop),
            "left" | "j" => Some(Command::SeekRelative(if modifiers.shift {
                -60.0
            } else {
                -10.0
            })),
            "right" | "l" => Some(Command::SeekRelative(if modifiers.shift {
                60.0
            } else {
                10.0
            })),
            "home" => Some(Command::SeekAbsolute(0.0)),
            "end" => Some(Command::SeekAbsolute(self.snapshot.duration)),
            "," if !modifiers.shift && self.snapshot.paused => Some(Command::FrameBackStep),
            "." if !modifiers.shift && self.snapshot.paused => Some(Command::FrameStep),
            "m" => Some(Command::SetMute(!self.snapshot.muted)),
            "up" => Some(Command::SetVolume((self.snapshot.volume + 5.0).min(100.0))),
            "down" => Some(Command::SetVolume((self.snapshot.volume - 5.0).max(0.0))),
            ">" | "." if modifiers.shift => {
                Some(Command::SetSpeed((self.snapshot.speed + 0.25).min(4.0)))
            }
            "<" | "," if modifiers.shift => {
                Some(Command::SetSpeed((self.snapshot.speed - 0.25).max(0.25)))
            }
            "r" => Some(Command::SetSpeed(1.0)),
            "[" => {
                self.selected_track = self.selected_track.saturating_sub(1);
                None
            }
            "]" => {
                self.selected_track =
                    (self.selected_track + 1).min(self.snapshot.tracks.len().saturating_sub(1));
                None
            }
            "-" | "=" | "+" | "a" => self.snapshot.tracks.get(self.selected_track).map(|track| {
                let gain = match key {
                    "-" => (track.gain - 0.05).max(0.0),
                    "a" => {
                        if track.gain > 0.0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                    _ => (track.gain + 0.05).min(1.0),
                };
                Command::SetTrackGain { id: track.id, gain }
            }),
            "f" => {
                window.toggle_fullscreen();
                None
            }
            "escape" => {
                self.show_help = false;
                self.show_tracks = false;
                None
            }
            "h" | "?" => {
                self.show_help = !self.show_help;
                None
            }
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => key
                .parse::<u8>()
                .ok()
                .map(|n| Command::SeekAbsolute(self.snapshot.duration * n as f64 / 10.0)),
            _ => return,
        };
        if let Some(command) = command {
            self.command(command, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn back_to_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.video_click.take();
        drop(self.playback.take());
        self.seeker_bar.update(cx, |seeker, _| {
            seeker.release_previews();
        });
        if let Some(host) = &self.native_video {
            host.hide();
        }
        self.path = None;
        self.snapshot = Snapshot::default();
        self.hover_navigation = None;
        self.show_help = false;
        self.show_tracks = false;
        self.browser
            .update(cx, |browser, cx| browser.focus(window, cx));
        cx.notify();
    }

    fn adjacent(&self, next: bool) -> Option<PathBuf> {
        let index = self
            .navigation
            .iter()
            .position(|p| Some(p) == self.path.as_ref())?;
        let index = if next {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };
        self.navigation.get(index).cloned()
    }
}

impl Render for PlayerShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.error.clone().or_else(|| self.snapshot.error.clone());
        let title = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Spacer".into());
        let content = div()
            .id("player")
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.controls_dragging = event.pressed_button == Some(MouseButton::Left);
                this.reveal_controls(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.controls_dragging = false;
                    this.reveal_controls(cx);
                }),
            )
            // Keep the full-window native video visible beneath GPUI's
            // DirectComposition layer. Only floating controls paint backgrounds.
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::click_video))
                    .when_some(
                        self.native_video.clone().filter(|_| self.path.is_some()),
                        |area, host| {
                            area.child(
                                canvas(
                                    move |bounds, window, _| {
                                        host.layout(bounds, window.scale_factor())
                                    },
                                    |_, _, _, _| {},
                                )
                                .size_full(),
                            )
                        },
                    ),
            )
            .when(self.path.is_none(), |shell| {
                shell.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(cx.theme().background.opacity(1.0))
                        .child(self.browser.clone()),
                )
            })
            .when(self.controls_visible && self.path.is_some(), |shell| {
                shell
                    .child(
                        div()
                            .absolute()
                            .top_4()
                            .left_4()
                            .right_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::click_overlay),
                                    )
                                    .min_w_0()
                                    .max_w(relative(0.7))
                                    .px_4()
                                    .py_2()
                                    .rounded_lg()
                                    .bg(cx.theme().background.opacity(0.85))
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .text_sm()
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::click_overlay),
                                    )
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .p_1()
                                    .rounded_lg()
                                    .bg(cx.theme().background.opacity(0.85))
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        Button::new("back-library")
                                            .ghost()
                                            .icon(IconName::ChevronLeft)
                                            .label("Library")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.back_to_library(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("open-media")
                                            .ghost()
                                            .icon(IconName::FolderOpen)
                                            .tooltip("Open media (Ctrl+O)")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("help")
                                            .ghost()
                                            .icon(IconName::Info)
                                            .tooltip("Keyboard shortcuts (H)")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_help = !this.show_help;
                                                this.reveal_controls(cx);
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("transport-panel")
                            .occlude()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                            .absolute()
                            .bottom_4()
                            .left_4()
                            .right_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .px_4()
                            .py_3()
                            .rounded_xl()
                            .bg(cx.theme().background.opacity(0.94))
                            .border_1()
                            .border_color(cx.theme().border)
                            .shadow_lg()
                            .child(self.seeker_bar.clone())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(self.media_controls.clone())
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("{:.2}×", self.snapshot.speed)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(self.volume_controls.clone())
                                            .when(!self.snapshot.tracks.is_empty(), |controls| {
                                                controls.child(
                                                    Button::new("audio-mixer")
                                                        .ghost()
                                                        .icon(IconName::Settings2)
                                                        .tooltip("Audio track mixer")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.show_tracks = !this.show_tracks;
                                                            this.reveal_controls(cx);
                                                            cx.notify();
                                                        })),
                                                )
                                            })
                                            .child(
                                                Button::new("fullscreen")
                                                    .ghost()
                                                    .icon(IconName::Maximize)
                                                    .tooltip("Toggle fullscreen (F)")
                                                    .on_click(cx.listener(|_, _, window, _| {
                                                        window.toggle_fullscreen()
                                                    })),
                                            ),
                                    ),
                            ),
                    )
            })
            .when(self.path.is_some() && self.controls_visible, |shell| {
                let mut shell = shell;
                for next in [false, true] {
                    let target = self.adjacent(next);
                    let hovered = self.hover_navigation == Some(next);
                    let image = target.as_ref().and_then(|path| {
                        self.browser
                            .update(cx, |browser, _| browser.thumbnail(path))
                    });
                    shell = shell.child(
                        div()
                            .id(if next { "next-video" } else { "previous-video" })
                            .absolute()
                            .top(relative(0.45))
                            .when(next, |s| s.right_4())
                            .when(!next, |s| s.left_4())
                            .occlude()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                                this.hover_navigation = if *hovering { Some(next) } else { None };
                                this.reveal_controls(cx);
                                cx.notify();
                            }))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                Button::new(if next { "next-clip" } else { "previous-clip" })
                                    .ghost()
                                    .icon(if next {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronLeft
                                    })
                                    .disabled(target.is_none())
                                    .label(if next { "Next" } else { "Previous" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(path) = this.adjacent(next) {
                                            this.load(path, cx);
                                        }
                                    })),
                            )
                            .when(hovered, |s| {
                                s.when_some(target, |s, path| {
                                    s.child(
                                        div()
                                            .w(px(180.))
                                            .p_2()
                                            .rounded_md()
                                            .bg(cx.theme().popover)
                                            .when_some(image, |s, image| {
                                                s.child(
                                                    img(image)
                                                        .w_full()
                                                        .h(px(101.))
                                                        .object_fit(ObjectFit::Contain),
                                                )
                                            })
                                            .child(
                                                div().text_xs().truncate().child(
                                                    path.file_name()
                                                        .unwrap_or_default()
                                                        .to_string_lossy()
                                                        .into_owned(),
                                                ),
                                            ),
                                    )
                                })
                            }),
                    );
                }
                shell
            })
            .when(self.controls_visible && self.show_tracks, |shell| {
                shell.child(
                    div()
                        .absolute()
                        .bottom(px(128.0))
                        .right_4()
                        .w(px(380.0))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                        .p_4()
                        .rounded_xl()
                        .bg(cx.theme().popover.opacity(0.96))
                        .border_1()
                        .border_color(cx.theme().border)
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .mb_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Audio mixer"),
                                )
                                .child(
                                    Button::new("close-mixer")
                                        .ghost()
                                        .icon(IconName::Close)
                                        .tooltip("Close mixer")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_tracks = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(self.audio_tracks.clone()),
                )
            })
            .when(self.controls_visible && self.show_help, |shell| {
                shell.child(
                    div()
                        .absolute()
                        .top(px(76.0))
                        .right_4()
                        .w(px(360.0))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                        .p_4()
                        .rounded_xl()
                        .bg(cx.theme().popover.opacity(0.96))
                        .border_1()
                        .border_color(cx.theme().border)
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .mb_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Keyboard shortcuts"),
                                )
                                .child(
                                    Button::new("close-help")
                                        .ghost()
                                        .icon(IconName::Close)
                                        .tooltip("Close shortcuts")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_help = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .children(
                            [
                                ("Open media", "Ctrl+O"),
                                ("Play / pause", "Space / K"),
                                ("Seek 10 seconds", "← / →"),
                                ("Seek 60 seconds", "Shift + ← / →"),
                                ("Previous / next frame", ", / ."),
                                ("Volume", "↑ / ↓"),
                                ("Mute", "M"),
                                ("Playback speed", "Shift + , / ."),
                                ("Select audio track", "[ / ]"),
                                ("Track volume", "− / +"),
                                ("Fullscreen", "F"),
                                ("Reload theme", "Ctrl+Shift+T"),
                                ("Dismiss panels", "Esc"),
                            ]
                            .into_iter()
                            .map(|(action, shortcut)| {
                                div()
                                    .flex()
                                    .justify_between()
                                    .py_1()
                                    .gap_4()
                                    .text_xs()
                                    .child(
                                        div().text_color(cx.theme().muted_foreground).child(action),
                                    )
                                    .child(shortcut)
                            }),
                        ),
                )
            })
            .when_some(error, |shell, error| {
                shell.child(
                    div()
                        .absolute()
                        .top(px(76.0))
                        .left_4()
                        .max_w(px(480.0))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                        .p_4()
                        .rounded_lg()
                        .bg(cx.theme().background.opacity(0.96))
                        .border_1()
                        .border_color(cx.theme().danger.opacity(0.4))
                        .text_sm()
                        .child(error),
                )
            });
        div()
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.reveal_controls(cx)))
            .when(!window.is_fullscreen(), |shell| {
                shell.child(
                    TitleBar::new().bg(cx.theme().background).child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(img("spacer/icon.png").w(px(20.)).h(px(20.)))
                            .child("Spacer"),
                    ),
                )
            })
            .child(div().relative().flex_1().min_h_0().child(content))
    }
}

fn controls_should_show(persistent: bool, interacting: bool, idle: Duration) -> bool {
    persistent || interacting || idle < Duration::from_secs(3)
}

fn double_click_interval() -> Duration {
    #[cfg(windows)]
    return Duration::from_millis(u64::from(unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime()
    }));
    #[cfg(not(windows))]
    Duration::from_millis(500)
}

#[cfg(test)]
mod styling_tests {
    use super::controls_should_show;
    use std::time::Duration;

    #[test]
    fn controls_hide_only_during_idle_playback() {
        assert!(controls_should_show(false, false, Duration::from_secs(2)));
        assert!(!controls_should_show(false, false, Duration::from_secs(3)));
        assert!(controls_should_show(true, false, Duration::from_secs(10)));
        assert!(controls_should_show(false, true, Duration::from_secs(10)));
    }
}

fn main() {
    logging::attach_parent_terminal();
    logging::Print(logging::Level::Info, "Starting Spacer");
    application().with_assets(app_assets::AppAssets).run(|cx| {
        init(cx);
        theme::apply(cx);
        let options = WindowOptions {
            window_background: WindowBackgroundAppearance::Transparent,
            titlebar: Some(TitlebarOptions {
                title: Some("Spacer".into()),
                ..TitleBar::title_bar_options()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(960.0), px(700.0)),
                cx,
            ))),
            window_min_size: Some(size(px(680.0), px(480.0))),
            ..TitleBar::window_options()
        };
        cx.open_window(options, |window, cx| {
            app_icon::apply(window);
            let player = cx.new(|cx| {
                let mut player = PlayerShell::new(window, cx);
                if let Some(path) = std::env::args_os().nth(1) {
                    player.load(PathBuf::from(path), cx);
                }
                player
            });
            cx.new(|cx| gpui_kit::base::Root::new(player, window, cx).bg(rgba(0x00000000)))
        })
        .expect("Failed to open player window");
        cx.activate(true);
    });
}

#[cfg(all(test, windows))]
mod overlay_tests {
    use super::{Command, PlayerShell, app_assets, double_click_interval, theme};
    use gpui_kit::{AppContext as _, InputEvent as _, Styled as _};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[test]
    #[ignore = "interactive GPUI overlay hit-testing with SPACER_AUDIO_PROBE media"]
    fn mixer_clicks_do_not_control_video() {
        use gpui_kit::{
            MouseButton, MouseDownEvent, MouseUpEvent, WindowBackgroundAppearance, WindowOptions,
            application, init, point, px, rgba,
        };
        let path = PathBuf::from(
            std::env::var_os("SPACER_AUDIO_PROBE")
                .expect("Set SPACER_AUDIO_PROBE to the test media"),
        );
        let result = Arc::new(Mutex::new(None));
        let report = result.clone();
        application()
            .with_assets(app_assets::AppAssets)
            .run(move |cx| {
                init(cx);
                theme::apply(cx);
                let mut player = None;
                let handle = cx
                    .open_window(
                        WindowOptions {
                            window_background: WindowBackgroundAppearance::Transparent,
                            ..Default::default()
                        },
                        |window, cx| {
                            let entity = cx.new(|cx| {
                                let mut shell = PlayerShell::new(window, cx);
                                shell.command(Command::SetMute(true), cx);
                                shell.load(path, cx);
                                shell
                            });
                            player = Some(entity.clone());
                            cx.new(|cx| {
                                gpui_kit::base::Root::new(entity, window, cx).bg(rgba(0x00000000))
                            })
                        },
                    )
                    .unwrap();
                let player = player.unwrap();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    handle
                        .update(cx, |_, _, cx| {
                            player.update(cx, |shell, cx| {
                                shell.command(Command::Pause, cx);
                                shell.show_tracks = true;
                                shell.controls_visible = true;
                                cx.notify();
                            });
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let (before, zoom_before) = handle
                        .update(cx, |_, window, cx| {
                            let state = player.read(cx).snapshot.clone();
                            let hwnd = player.read(cx).native_video.as_ref().unwrap().hwnd();
                            let root = unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                                    hwnd as _,
                                    windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT,
                                )
                            };
                            let zoom = unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed(root)
                            };
                            let size = window.viewport_size();
                            // Inside the mixer's bottom/right padding, not a slider or button.
                            let position = point(size.width - px(24.0), size.height - px(136.0));
                            for click_count in [1, 2] {
                                window.dispatch_event(
                                    MouseDownEvent {
                                        position,
                                        button: MouseButton::Left,
                                        click_count,
                                        ..Default::default()
                                    }
                                    .to_platform_input(),
                                    cx,
                                );
                                window.dispatch_event(
                                    MouseUpEvent {
                                        position,
                                        button: MouseButton::Left,
                                        ..Default::default()
                                    }
                                    .to_platform_input(),
                                    cx,
                                );
                            }
                            (state, zoom)
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(double_click_interval() + Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, _, cx| {
                            let shell = player.read(cx);
                            let hwnd = shell.native_video.as_ref().unwrap().hwnd();
                            let root = unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                                    hwnd as _,
                                    windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT,
                                )
                            };
                            let zoom = unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed(root)
                            };
                            *report.lock().unwrap() = Some((
                                before.paused,
                                shell.snapshot.paused,
                                (shell.snapshot.position - before.position).abs(),
                                shell.show_tracks,
                                shell.video_click.is_none(),
                                zoom == zoom_before,
                                shell.snapshot.error.clone(),
                            ));
                            player.update(cx, |shell, _| shell.shutdown());
                            cx.quit();
                        })
                        .unwrap();
                })
                .detach();
                cx.activate(true);
            });
        let (before, after, drift, mixer_open, no_pending_click, same_zoom, error) =
            result.lock().unwrap().take().expect("UI check completed");
        assert!(error.is_none(), "{error:?}");
        assert!(before && after, "Mixer clicks changed pause state");
        assert!(drift < 0.01, "Mixer clicks moved playback by {drift}");
        assert!(
            mixer_open && no_pending_click && same_zoom,
            "Mixer clicks escaped into video gestures"
        );
    }
}

impl Drop for PlayerShell {
    fn drop(&mut self) {
        self.shutdown();
    }
}
