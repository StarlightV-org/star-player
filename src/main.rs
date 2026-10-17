#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use components::{
    audio_tracks::{AudioTracks, TrackGainEvent},
    clip_browser::{BrowserEvent, BrowserView, ClipBrowser},
    media_controls::{MediaControls, MediaControlsEvent},
    seeker_bar::{SeekerBar, SeekerEvent},
    volume_controls::{VolumeControls, VolumeEvent},
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, TITLE_BAR_HEIGHT, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use playback::{Command, Playback, Snapshot};
use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

mod app_assets;
mod app_icon;
mod clip_details;
mod clip_library;
mod clip_store;
mod components;
mod file_info;
mod game_icons;
mod keybinds;
mod logging;
mod media_keys;
mod native_backdrop;
mod native_video;
mod navigation;
mod notifications;
mod playback;
mod preview;
mod resume;
mod runtime;
mod steam_games;
mod theme;
mod thumbnail_cache;
#[cfg(all(test, windows))]
mod todo_tests;
mod updater;
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

struct NavigationView {
    path: Option<PathBuf>,
    queue: Vec<PathBuf>,
    folder: bool,
    browser: BrowserView,
}

/// Keep shortcut capture above Root's dialog layer as well as the main view.
struct ShortcutCapture;

impl Render for ShortcutCapture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl gpui_kit::base::RootPlugin for ShortcutCapture {
    fn decorate(
        &self,
        surface: AnyElement,
        root: &gpui_kit::base::Root,
        _window: &mut Window,
        _cx: &mut App,
    ) -> impl IntoElement {
        let player = root.view().clone().downcast::<PlayerShell>().ok();
        div()
            .size_full()
            .capture_key_down(move |event: &KeyDownEvent, window, cx| {
                if let Some(player) = &player {
                    let browser = player.read(cx).browser.clone();
                    if window.has_active_dialog(cx)
                        && browser.update(cx, |browser, cx| browser.record_keyboard_key(event, cx))
                    {
                        cx.stop_propagation();
                    } else {
                        player.update(cx, |player, cx| player.key(event, window, cx));
                    }
                }
            })
            .child(surface)
    }
}

fn player_root(
    player: Entity<PlayerShell>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<gpui_kit::base::Root> {
    gpui_kit::base::Root::register_plugin(cx, |_, _| ShortcutCapture);
    cx.new(|cx| gpui_kit::base::Root::new(player, window, cx).bg(rgba(0x00000000)))
}

struct PlayerShell {
    browser: Entity<ClipBrowser>,
    navigation: Vec<PathBuf>,
    folder_navigation: bool,
    navigation_scan: Option<Task<()>>,
    history: navigation::History<NavigationView>,
    played_recorded: bool,
    pending_load: Option<PathBuf>,
    audio_initialized: bool,
    master_volume: f64,
    master_muted: bool,
    pending_master: Option<(f64, bool)>,
    pending_audio: Option<(PathBuf, std::collections::BTreeMap<i64, f64>)>,
    audio_changed: Instant,
    resume_candidate: Option<f64>,
    resume_prompt: Option<resume::Prompt>,
    resume_focus: FocusHandle,
    resume_saved_at: Instant,
    resume_stopped: bool,
    rename_resume: Option<(PathBuf, f64, bool)>,
    rename_busy: bool,
    hover_navigation: Option<bool>,
    playback: Option<Playback>,
    snapshot: Snapshot,
    native_video: Option<Rc<native_video::NativeVideo>>,
    path: Option<PathBuf>,
    error: Option<String>,
    notified_playback_error: Option<(Option<PathBuf>, String)>,
    focus: FocusHandle,
    media_controls: Entity<MediaControls>,
    volume_controls: Entity<VolumeControls>,
    seeker_bar: Entity<SeekerBar>,
    audio_tracks: Entity<AudioTracks>,
    selected_track: usize,
    show_help: bool,
    show_info: bool,
    show_settings: bool,
    title_hovered: bool,
    title_focus: FocusHandle,
    show_tracks: bool,
    controls_visible: bool,
    focus_mode: bool,
    controls_opacity: f32,
    controls_frame: Instant,
    controls_dragging: bool,
    last_activity: Instant,
    video_click: Option<Task<()>>,
    media_keys: media_keys::MediaKeys,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl PlayerShell {
    fn render_title_bar(
        &self,
        title: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let update_visible = self.browser.read(cx).updater.state.visible();
        let title_margin = if update_visible {
            px(380.)
        } else {
            window.viewport_size().width * 0.2
        };
        // The kit's content area excludes the window controls. Center against the
        // whole bar instead, so the filename stays at the actual window midpoint.
        div()
            .relative()
            .h(TITLE_BAR_HEIGHT)
            .flex_shrink_0()
            .child(
                TitleBar::new().bg(cx.theme().title_bar).child(
                    div()
                        .w_full()
                        .h_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(img("star-player/icon.png").w(px(20.)).h(px(20.)))
                        .child(
                            div()
                                .h_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .line_height(px(20.))
                                .whitespace_nowrap()
                                .child("StarPlayer")
                                .when(cfg!(debug_assertions), |label| {
                                    label.child(div().text_color(cx.theme().primary).child("DEV"))
                                }),
                        )
                        .child(div().flex_1())
                        .when(update_visible, |bar| {
                            let state = &self.browser.read(cx).updater.state;
                            bar.child(
                                div()
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::click_overlay),
                                    )
                                    .child(
                                        Button::new("title-bar-update")
                                            .ghost()
                                            .label(match state {
                                                updater::State::Available(_) => {
                                                    "Download update".into()
                                                }
                                                updater::State::Failed(_) => "Retry update".into(),
                                                _ => state.label(),
                                            })
                                            .tooltip("Updates are managed in Settings > About")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.browser.update(cx, |browser, cx| {
                                                    browser.update_action(cx)
                                                });
                                            })),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                                .child(
                                    Button::new("settings")
                                        .ghost()
                                        .icon(IconName::Settings)
                                        .tooltip("Settings")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.set_settings(!this.show_settings, window, cx);
                                        })),
                                ),
                        ),
                ),
            )
            .when(
                self.path.is_some()
                    && !self.show_settings
                    && window.viewport_size().width > title_margin * 2. + px(80.),
                |bar| {
                    bar.child(
                        div()
                            .absolute()
                            .top_0()
                            .left(title_margin)
                            .right(title_margin)
                            .h(TITLE_BAR_HEIGHT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(self.render_media_title(title, false, cx)),
                    )
                },
            )
    }

    fn set_settings(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = visible;
        self.title_hovered = false;
        self.browser.update(cx, |browser, cx| {
            browser.settings = visible;
            cx.notify();
        });
        if visible {
            self.video_click.take();
            self.hover_navigation = None;
            if let Some(host) = &self.native_video {
                host.hide();
            }
        } else if self.path.is_some() {
            window.focus(&self.focus, cx);
        } else {
            self.browser
                .update(cx, |browser, cx| browser.focus(window, cx));
        }
        self.reveal_controls(cx);
        cx.notify();
    }

    fn rename_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rename_busy
            && let Some(path) = self.path.clone()
        {
            self.browser
                .update(cx, |browser, cx| browser.prompt_rename(path, window, cx));
        }
    }

    fn render_media_title(
        &self,
        title: &str,
        fullscreen: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("editable-media-title")
            // Exclude interactive content from the kit's native Windows drag hitbox.
            .occlude()
            .track_focus(&self.title_focus)
            .tab_index(0)
            .min_w_0()
            .max_w_full()
            .flex()
            .items_center()
            .gap_2()
            .when(fullscreen, |s| {
                s.px_3()
                    .py_2()
                    .rounded_full()
                    .bg(cx.theme().background.opacity(0.9))
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.title_focus, cx);
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.rename_current(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.rename_current(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.title_hovered = *hovered;
                this.reveal_controls(cx);
                cx.notify();
            }))
            // Balance the pencil slot so the filename itself is centered.
            .when(!fullscreen, |s| {
                s.child(div().flex_shrink_0().w(px(16.)).h(px(16.)))
            })
            .child(div().min_w_0().truncate().text_sm().child(title.to_owned()))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(16.))
                    .h(px(16.))
                    .when(self.title_hovered, |s| {
                        s.child(Icon::new(gpui_kit::assets::IconName::Pencil).size(px(16.)))
                    }),
            )
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let browser = cx.new(|cx| ClipBrowser::new(std::env::args_os().nth(1).is_none(), cx));
        Self::with_browser(browser, window, cx)
    }

    fn with_browser(
        browser: Entity<ClipBrowser>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if !cx.has_global::<keybinds::Keymap>() {
            cx.set_global(keybinds::Keymap::open(
                clip_store::directory()
                    .unwrap_or_default()
                    .join("keybinds.json"),
            ));
        }
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
                        if this.rename_busy {
                            return;
                        }
                        this.remember_view(Some(path), cx);
                        this.navigation_scan.take();
                        this.folder_navigation = false;
                        this.navigation = list.clone();
                        this.load_media(path.clone(), cx);
                        window.focus(&this.focus, cx);
                    }
                    BrowserEvent::Delete(paths) => {
                        if this.rename_busy {
                            return;
                        }
                        if this.path.as_ref().is_some_and(|path| paths.contains(path)) {
                            this.back_to_library(window, cx);
                        }
                        let released = this
                            .seeker_bar
                            .update(cx, |seeker, _| seeker.release_previews());
                        this.browser.read(cx).delete(paths.clone(), released);
                    }
                    BrowserEvent::Open => this.open(window, cx),
                    BrowserEvent::CloseSettings => this.set_settings(false, window, cx),
                    BrowserEvent::RestartUpdate => this.restart_update(window, cx),
                    BrowserEvent::Navigate(previous) => {
                        let mut view = this.current_view(cx);
                        view.browser = previous.clone();
                        this.history.visit(view);
                    }
                    BrowserEvent::Rename(path, stem) => {
                        if this.rename_busy
                            || matches!(
                                this.browser.read(cx).updater.state,
                                updater::State::PreparingRestart
                            )
                        {
                            return;
                        }
                        this.rename_busy = true;
                        this.navigation_scan.take();
                        this.flush_audio(cx);
                        this.save_resume(cx);
                        if this.path.as_ref() == Some(path) {
                            this.rename_resume =
                                Some((path.clone(), this.snapshot.position, this.snapshot.paused));
                            this.video_click.take();
                            drop(this.playback.take());
                        }
                        let released = this
                            .seeker_bar
                            .update(cx, |seeker, _| seeker.suspend_previews());
                        this.browser
                            .read(cx)
                            .rename(path.clone(), stem.clone(), released);
                    }
                    BrowserEvent::Renamed(old, new) => {
                        this.rename_busy = false;
                        for path in &mut this.navigation {
                            if path == old {
                                *path = new.clone();
                            }
                        }
                        this.history.update(|view| {
                            if view.path.as_ref() == Some(old) {
                                view.path = Some(new.clone());
                            }
                            for path in &mut view.queue {
                                if path == old {
                                    *path = new.clone();
                                }
                            }
                            view.browser.rename(old, new);
                        });
                        if let Some((_, position, paused)) = this.rename_resume.take() {
                            this.load_media_at(new.clone(), position, paused, cx);
                        } else if let Some(path) = this.path.clone() {
                            this.seeker_bar
                                .update(cx, |seeker, cx| seeker.set_media(path, cx));
                        }
                        if this.folder_navigation
                            && let Some(path) = this.path.clone()
                        {
                            this.scan_navigation(path, cx);
                        }
                    }
                    BrowserEvent::RenameFailed(path, error) => {
                        this.rename_busy = false;
                        if let Some((_, position, paused)) = this.rename_resume.take() {
                            this.load_media_at(path.clone(), position, paused, cx);
                        } else if let Some(path) = this.path.clone() {
                            this.seeker_bar
                                .update(cx, |seeker, cx| seeker.set_media(path, cx));
                        }
                        if this.folder_navigation
                            && let Some(path) = this.path.clone()
                        {
                            this.scan_navigation(path, cx);
                        }
                        notifications::error(window, cx, "Rename failed", error);
                        cx.notify();
                    }
                    BrowserEvent::Error(error) => {
                        notifications::error(window, cx, "Library", error);
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &media_controls,
                window,
                |this, _, event: &MediaControlsEvent, _, cx| {
                    this.command(
                        match event {
                            MediaControlsEvent::Toggle => Command::Toggle,
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
                |this, _, event: &SeekerEvent, window, cx| match event {
                    SeekerEvent::Seek(seconds) => this.command(Command::SeekAbsolute(*seconds), cx),
                    SeekerEvent::PreviewError(error) => {
                        notifications::error(window, cx, "Preview unavailable", error)
                    }
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
            let _ = shell_entity.update(cx, |shell, cx| {
                shell.flush_audio(cx);
                shell.save_resume(cx);
                shell.shutdown();
            });
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let shell = Self {
            browser,
            navigation: vec![],
            folder_navigation: false,
            navigation_scan: None,
            history: navigation::History::default(),
            played_recorded: false,
            pending_load: None,
            audio_initialized: false,
            master_volume: 100.0,
            master_muted: false,
            pending_master: None,
            pending_audio: None,
            audio_changed: Instant::now(),
            resume_candidate: None,
            resume_prompt: None,
            resume_focus: cx.focus_handle(),
            resume_saved_at: Instant::now(),
            resume_stopped: false,
            rename_resume: None,
            rename_busy: false,
            hover_navigation: None,
            playback,
            snapshot: Snapshot::default(),
            native_video,
            path: None,
            error,
            notified_playback_error: None,
            focus,
            media_controls,
            volume_controls,
            seeker_bar,
            audio_tracks,
            selected_track: 0,
            show_help: false,
            show_info: false,
            show_settings: false,
            title_hovered: false,
            title_focus: cx.focus_handle(),
            show_tracks: false,
            controls_visible: true,
            focus_mode: false,
            controls_opacity: 1.0,
            controls_frame: Instant::now(),
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
        if self.rename_busy {
            return;
        }
        match &command {
            Command::Pause | Command::Toggle if !self.snapshot.paused => self.save_resume(cx),
            Command::Play | Command::Toggle => self.resume_stopped = false,
            Command::Stop => {
                self.resume_stopped = true;
                self.resume_prompt = None;
                self.resume_candidate = None;
                if let Some(path) = self.path.clone() {
                    self.browser
                        .update(cx, |browser, _| browser.save_resume(path, None));
                }
            }
            Command::SeekAbsolute(_)
            | Command::SeekRelative(_)
            | Command::FrameStep
            | Command::FrameBackStep => {
                self.resume_stopped = false;
            }
            Command::SetVolume(volume) => {
                self.master_volume = *volume;
                self.pending_master = Some((*volume, self.master_muted));
                self.audio_changed = Instant::now();
            }
            Command::SetMute(muted) => {
                self.master_muted = *muted;
                self.pending_master = Some((self.master_volume, *muted));
                self.audio_changed = Instant::now();
            }
            Command::SetTrackGain { id, gain } => {
                if let Some(path) = &self.path {
                    let (_, gains) = self.pending_audio.get_or_insert_with(|| {
                        (
                            path.clone(),
                            self.snapshot
                                .tracks
                                .iter()
                                .map(|t| (t.id, t.gain))
                                .collect(),
                        )
                    });
                    gains.insert(*id, *gain);
                    self.audio_changed = Instant::now();
                }
            }
            _ => {}
        }
        self.video_click.take();
        if command_reveals_controls(&command) {
            self.reveal_controls(cx);
        }
        if let Some(player) = &self.playback {
            if let Err(error) = player.send(command) {
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    fn restart_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_busy {
            return;
        }
        let updater::State::Ready(stage, _) = &self.browser.read(cx).updater.state else {
            return;
        };
        let stage = stage.clone();
        let reopen = self.path.clone();
        self.browser.update(cx, |browser, cx| {
            browser.updater.state = updater::State::PreparingRestart;
            cx.notify();
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { updater::prepare_restart(&stage, reopen.as_deref()) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.flush_audio(cx);
                    this.save_resume(cx);
                    this.shutdown();
                    cx.quit();
                }
                Err(error) => {
                    this.browser.update(cx, |browser, cx| {
                        browser.updater.state = updater::State::Failed(error.clone());
                        cx.notify();
                    });
                    crate::notifications::error(window, cx, "Update failed", &error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn flush_audio(&mut self, cx: &mut Context<Self>) {
        if let Some((volume, muted)) = self.pending_master.take() {
            self.browser
                .update(cx, |browser, _| browser.save_master(volume, muted));
        }
        if let Some((path, gains)) = self.pending_audio.take() {
            self.browser
                .update(cx, |browser, _| browser.save_audio(path, gains));
        }
    }

    fn save_resume(&mut self, cx: &mut Context<Self>) {
        self.resume_saved_at = Instant::now();
        // Read the worker before teardown rather than persisting an older UI poll.
        let Some(player) = &self.playback else {
            return;
        };
        let snapshot = player.snapshot();
        if snapshot.path != self.path
            || !self
                .browser
                .read(cx)
                .config()
                .player
                .remember_playback
                .includes(snapshot.duration)
            || snapshot.error.is_some()
        {
            return;
        }
        let position = if self.resume_stopped || snapshot.ended {
            None
        } else {
            resume::position(
                self.browser.read(cx).config().player.remember_playback,
                snapshot.duration,
                snapshot.position,
            )
        };
        if let Some(path) = self.path.clone()
            && self.browser.read(cx).media_state(&path).resume_position != position
        {
            self.browser
                .update(cx, |browser, _| browser.save_resume(path, position));
        }
    }

    fn reveal_controls(&mut self, cx: &mut Context<Self>) {
        if self.player_focus_mode() {
            return;
        }
        self.last_activity = Instant::now();
        if let Some(host) = &self.native_video {
            host.set_cursor_hidden(false);
        }
        if !self.controls_visible {
            self.controls_visible = true;
            self.controls_frame = Instant::now();
            cx.notify();
        }
    }

    fn player_focus_mode(&self) -> bool {
        self.focus_mode && self.path.is_some() && !self.show_settings
    }

    fn shutdown(&mut self) {
        self.navigation_scan.take();
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
        if self.rename_busy {
            return;
        }
        self.remember_view(Some(&path), cx);
        self.load_media(path, cx);
    }

    fn load_media(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load_media_at(path.clone(), 0.0, false, cx);
        self.resume_candidate = self.browser.read(cx).media_state(&path).resume_position;
    }

    fn load_media_at(
        &mut self,
        path: PathBuf,
        position: f64,
        paused: bool,
        cx: &mut Context<Self>,
    ) {
        self.save_resume(cx);
        self.resume_prompt = None;
        self.resume_candidate = None;
        self.resume_stopped = false;
        self.resume_saved_at = Instant::now();
        self.flush_audio(cx);
        if !self.browser.read(cx).audio_ready() {
            self.path = Some(path.clone());
            self.pending_load = Some(path);
            cx.notify();
            return;
        }
        self.initialize_audio(cx);
        self.pending_load = None;
        self.played_recorded = false;
        self.title_hovered = false;
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
        self.notified_playback_error = None;
        self.selected_track = 0;
        self.show_tracks = false;
        self.path = Some(path.clone());
        self.seeker_bar
            .update(cx, |seeker, cx| seeker.set_media(path.clone(), cx));
        let gains = self.browser.read(cx).media_state(&path).track_gains;
        self.command(
            Command::LoadSaved(
                path,
                playback::LoadState {
                    volume: self.master_volume,
                    muted: self.master_muted,
                    gains,
                    position,
                    paused,
                },
            ),
            cx,
        );
    }

    fn initialize_audio(&mut self, cx: &Context<Self>) {
        if !self.audio_initialized && self.browser.read(cx).audio_ready() {
            self.master_volume = self.browser.read(cx).config().volume;
            self.master_muted = self.browser.read(cx).config().muted;
            self.audio_initialized = true;
        }
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.rename_busy {
            return;
        }
        self.remember_view(Some(&path), cx);
        self.open_file_view(path, cx);
    }

    fn open_file_view(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.navigation_scan.take();
        self.folder_navigation = true;
        self.navigation.clear();
        let path = if path.is_relative() {
            std::env::current_dir()
                .map(|dir| dir.join(&path))
                .unwrap_or(path)
        } else {
            path
        };
        self.load_media(path.clone(), cx);
        self.scan_navigation(path, cx);
    }

    fn scan_navigation(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let scan = cx
            .background_executor()
            .spawn(async move { navigation::siblings(&path) });
        self.navigation_scan = Some(cx.spawn(async move |this, cx| {
            let queue = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.navigation = queue;
                cx.notify();
            });
        }));
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_busy {
            return;
        }
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
                        this.open_file(path, cx);
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
        if theme::poll(cx) {
            cx.notify();
        }
        if cx.has_global::<keybinds::Keymap>() {
            let scope = self.key_scope(window, cx);
            if let Some(action) = cx.global_mut::<keybinds::Keymap>().expire(&scope) {
                self.run_action(&action, window, cx);
            }
        }
        self.browser.update(cx, |browser, cx| {
            browser.poll(self.path.is_none(), window, cx)
        });
        self.initialize_audio(cx);
        if self.browser.read(cx).audio_ready()
            && let Some(path) = self.pending_load.take()
        {
            self.load_media(path, cx);
        }
        if self.audio_changed.elapsed() >= Duration::from_millis(350) {
            self.flush_audio(cx);
        }
        if let Some(player) = &self.playback {
            let snapshot = player.snapshot();
            let paused_now = snapshot.paused && !self.snapshot.paused;
            let ended_now = snapshot.ended && !self.snapshot.ended;
            self.snapshot = snapshot;
            if self.snapshot.path == self.path
                && self.snapshot.duration > 0.0
                && let Some(position) = self.resume_candidate.take()
                && let Some(position) = resume::position(
                    self.browser.read(cx).config().player.remember_playback,
                    self.snapshot.duration,
                    position,
                )
            {
                self.resume_prompt = Some(resume::Prompt::new(position, Instant::now()));
            }
            if paused_now || ended_now || self.resume_saved_at.elapsed() >= Duration::from_secs(5) {
                self.save_resume(cx);
            }
        }
        if !self
            .browser
            .read(cx)
            .config()
            .player
            .remember_playback
            .includes(self.snapshot.duration)
        {
            self.resume_prompt = None;
        }
        if let Some(prompt) = &mut self.resume_prompt
            && prompt.expired(
                Instant::now(),
                self.resume_focus.contains_focused(window, cx),
            )
        {
            self.resume_prompt = None;
        }
        if let Some(error) = self.error.take() {
            notifications::error(window, cx, "Player", &error);
        }
        let playback_error = self
            .snapshot
            .error
            .as_ref()
            .map(|error| (self.snapshot.path.clone(), error.clone()));
        if playback_error != self.notified_playback_error {
            if let Some((_, error)) = &playback_error {
                notifications::error(window, cx, "Playback failed", error);
            }
            self.notified_playback_error = playback_error;
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
        if !self.folder_navigation && self.path.is_some() && !self.navigation.is_empty() {
            self.navigation.retain(|path| {
                Some(path) == self.path.as_ref() || self.browser.read(cx).exists(path)
            });
        }
        self.update_navigation_hover(window, cx);
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
            let key = match action {
                media_keys::MediaAction::Toggle => "media-play-pause",
                media_keys::MediaAction::Stop => "media-stop",
                media_keys::MediaAction::Backward => "media-previous",
                media_keys::MediaAction::Forward => "media-next",
                _ => continue,
            };
            if window.has_active_dialog(cx)
                && self
                    .browser
                    .update(cx, |browser, cx| browser.record_media_key(key, cx))
            {
                continue;
            }
            let scope = self.key_scope(window, cx);
            if let Some(action) = cx.global_mut::<keybinds::Keymap>().feed(key, scope).0 {
                self.run_action(&action, window, cx);
            }
        }
        let controls_visible = controls_should_show(
            self.player_focus_mode(),
            self.path.is_none()
                || self.show_settings
                || self.show_info
                || self.show_tracks
                || self.show_help
                || self.title_hovered
                || self.title_focus.is_focused(window)
                || self.rename_busy
                || self.volume_controls.read(cx).interacting(window, cx)
                || self.hover_navigation.is_some()
                || self.snapshot.error.is_some(),
            self.controls_dragging,
            self.last_activity.elapsed(),
        );
        if self.controls_visible != controls_visible {
            self.controls_visible = controls_visible;
            self.controls_frame = Instant::now();
        }
        if let Some(host) = &self.native_video {
            host.set_cursor_hidden(!self.controls_visible);
        }
        cx.notify();
    }

    fn key_scope(&self, window: &mut Window, cx: &mut App) -> keybinds::Scope {
        let mut scope = keybinds::Scope::new([
            "Application",
            if self.show_settings {
                "Settings"
            } else if self.path.is_some() {
                "Player"
            } else {
                "ClipLibrary"
            },
        ]);
        if self.path.is_some() && !self.show_settings {
            scope.values.push(if self.snapshot.paused {
                "paused"
            } else {
                "playing"
            });
        }
        if window.is_fullscreen() {
            scope.values.push("fullscreen");
        }
        if self.player_focus_mode() {
            scope.values.push("focus_mode");
        }
        if window.has_active_dialog(cx) {
            scope.values.push("Dialog");
        }
        if window.has_focused_input(cx) {
            scope.values.push("TextInput");
        }
        self.browser.read(cx).extend_scope(&mut scope);
        scope
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<keybinds::Keymap>() {
            return;
        }
        let scope = self.key_scope(window, cx);
        if self.rename_busy {
            return;
        }
        let (action, consumed) = cx
            .global_mut::<keybinds::Keymap>()
            .feed(&keybinds::stroke(&event.keystroke), scope);
        if let Some(action) = action {
            self.run_action(&action, window, cx);
        }
        if consumed {
            cx.stop_propagation();
        }
    }

    fn run_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(action, "frame_back" | "frame_forward" | "focus_mode") {
            self.reveal_controls(cx);
        }
        let command = match action {
            "focus_mode" => {
                self.focus_mode = !self.focus_mode;
                if self.focus_mode {
                    self.controls_visible = false;
                    self.controls_opacity = 0.0;
                    self.controls_frame = Instant::now();
                    self.show_help = false;
                    self.show_info = false;
                    self.show_tracks = false;
                    self.title_hovered = false;
                    window.focus(&self.focus, cx);
                    if let Some(host) = &self.native_video {
                        host.set_cursor_hidden(true);
                    }
                } else {
                    self.reveal_controls(cx);
                }
                None
            }
            "open" => {
                self.open(window, cx);
                None
            }
            "settings" => {
                self.set_settings(true, window, cx);
                None
            }
            "close_settings" => {
                self.set_settings(false, window, cx);
                None
            }
            "reload_theme" => {
                theme::apply(cx);
                None
            }
            "toggle" => Some(Command::Toggle),
            "play" => Some(Command::Play),
            "pause" => Some(Command::Pause),
            "stop" => Some(Command::Stop),
            "seek_back" => Some(Command::SeekRelative(-10.0)),
            "seek_forward" => Some(Command::SeekRelative(10.0)),
            "seek_back_long" => Some(Command::SeekRelative(-60.0)),
            "seek_forward_long" => Some(Command::SeekRelative(60.0)),
            "start" => Some(Command::SeekAbsolute(0.0)),
            "end" => Some(Command::SeekAbsolute(self.snapshot.duration)),
            "frame_back" => Some(Command::FrameBackStep),
            "frame_forward" => Some(Command::FrameStep),
            "mute" => Some(Command::SetMute(!self.snapshot.muted)),
            "volume_up" => Some(Command::SetVolume((self.snapshot.volume + 5.0).min(100.0))),
            "volume_down" => Some(Command::SetVolume((self.snapshot.volume - 5.0).max(0.0))),
            "speed_up" => Some(Command::SetSpeed((self.snapshot.speed + 0.25).min(4.0))),
            "speed_down" => Some(Command::SetSpeed((self.snapshot.speed - 0.25).max(0.25))),
            "speed_reset" => Some(Command::SetSpeed(1.0)),
            "track_previous" => {
                self.selected_track = self.selected_track.saturating_sub(1);
                None
            }
            "track_next" => {
                self.selected_track =
                    (self.selected_track + 1).min(self.snapshot.tracks.len().saturating_sub(1));
                None
            }
            "track_down" | "track_up" | "track_toggle" => {
                self.snapshot.tracks.get(self.selected_track).map(|track| {
                    let gain = match action {
                        "track_down" => (track.gain - 0.05).max(0.0),
                        "track_up" => (track.gain + 0.05).min(1.0),
                        _ => {
                            if track.gain > 0.0 {
                                0.0
                            } else {
                                1.0
                            }
                        }
                    };
                    Command::SetTrackGain { id: track.id, gain }
                })
            }
            "fullscreen" => {
                window.toggle_fullscreen();
                None
            }
            "dismiss" => {
                self.show_help = false;
                self.show_info = false;
                self.show_tracks = false;
                None
            }
            "help" => {
                self.show_help = !self.show_help;
                self.show_info = false;
                None
            }
            "info" => {
                self.show_info = !self.show_info;
                None
            }
            "tracks" => {
                self.show_tracks = !self.show_tracks;
                None
            }
            "library" => {
                self.back_to_library(window, cx);
                None
            }
            "rename" if self.path.is_some() && !self.show_settings => {
                self.rename_current(window, cx);
                None
            }
            "history_back" | "history_forward" => {
                self.navigate_history(action == "history_forward", window, cx);
                None
            }
            "previous_clip" | "next_clip" => {
                if let Some(path) = self.adjacent(action == "next_clip") {
                    self.open_file_view(path, cx);
                }
                None
            }
            id if id.starts_with("seek_") => id[5..]
                .parse::<u8>()
                .ok()
                .map(|n| Command::SeekAbsolute(self.snapshot.duration * n as f64 / 10.0)),
            id => {
                self.browser
                    .update(cx, |browser, cx| browser.run_library_action(id, window, cx));
                None
            }
        };
        if let Some(command) = command {
            self.command(command, cx);
        }
        cx.notify();
    }

    fn back_to_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_busy {
            return;
        }
        self.remember_view(None, cx);
        self.show_library(window, cx);
    }

    fn show_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_resume(cx);
        self.resume_prompt = None;
        self.resume_candidate = None;
        self.flush_audio(cx);
        self.pending_load = None;
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.navigation_scan.take();
        self.navigation.clear();
        self.video_click.take();
        drop(self.playback.take());
        self.seeker_bar.update(cx, |seeker, _| {
            seeker.release_previews();
        });
        if let Some(host) = &self.native_video {
            host.hide();
        }
        self.path = None;
        self.focus_mode = false;
        self.snapshot = Snapshot::default();
        self.notified_playback_error = None;
        self.hover_navigation = None;
        self.show_help = false;
        self.show_info = false;
        self.title_hovered = false;
        self.show_tracks = false;
        self.browser
            .update(cx, |browser, cx| browser.focus(window, cx));
        cx.notify();
    }

    fn adjacent(&self, next: bool) -> Option<PathBuf> {
        navigation::adjacent(&self.navigation, self.path.as_ref()?, next)
    }

    fn update_navigation_hover(&mut self, window: &Window, cx: &mut Context<Self>) {
        let position = window.mouse_position();
        let size = window.viewport_size();
        // Use pointer coordinates rather than hitbox hover. The button and preview
        // occlude the video, but must not make their own proximity region flicker.
        let hovered = navigation::near_edge(
            position.x.into(),
            position.y.into(),
            size.width.into(),
            size.height.into(),
        )
        .filter(|next| {
            !self.show_settings && window.is_window_hovered() && self.adjacent(*next).is_some()
        });
        if self.hover_navigation != hovered {
            self.hover_navigation = hovered;
            cx.notify();
        }
    }

    fn current_view(&self, cx: &Context<Self>) -> NavigationView {
        NavigationView {
            path: self.path.clone(),
            queue: self.navigation.clone(),
            folder: self.folder_navigation,
            browser: self.browser.read(cx).view(),
        }
    }

    fn remember_view(&mut self, target: Option<&PathBuf>, cx: &Context<Self>) {
        if target != self.path.as_ref() {
            self.history.visit(self.current_view(cx));
        }
    }

    fn navigate_history(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            self.set_settings(false, window, cx);
            return;
        }
        if self.rename_busy {
            return;
        }
        let current = self.current_view(cx);
        let target = if forward {
            self.history.forward(current)
        } else {
            self.history.back(current)
        };
        if let Some(target) = target {
            self.browser
                .update(cx, |browser, cx| browser.restore(target.browser, cx));
            self.navigation_scan.take();
            self.video_click.take();
            if let Some(path) = target.path {
                if target.folder {
                    self.open_file_view(path, cx);
                } else {
                    self.folder_navigation = false;
                    self.navigation = target.queue;
                    self.load_media(path, cx);
                }
                window.focus(&self.focus, cx);
            } else {
                self.show_library(window, cx);
            }
        }
    }
}

impl Render for PlayerShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.player_focus_mode() {
            self.controls_visible = false;
            self.controls_opacity = 0.0;
        }
        let now = Instant::now();
        self.controls_opacity = advance_controls_opacity(
            self.controls_opacity,
            self.controls_visible,
            now.saturating_duration_since(self.controls_frame),
            reduced_motion(),
        );
        self.controls_frame = now;
        let target = if self.controls_visible { 1.0 } else { 0.0 };
        if self.controls_opacity != target {
            window.request_animation_frame();
        }
        let opacity =
            self.controls_opacity * self.controls_opacity * (3.0 - 2.0 * self.controls_opacity);
        let controls_offset = 8.0 - (1.0 - opacity) * 8.0;
        let info_trigger_bounds = Rc::new(Cell::new(Bounds::default()));
        let mixer_trigger_bounds = Rc::new(Cell::new(Bounds::default()));
        let shell = cx.weak_entity();
        // History buttons work over occluding panels and the library as well as
        // the video. Root hitbox handlers alone cannot see all of those clicks.
        let history_events = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let shell = shell.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture
                        && let MouseButton::Navigate(direction) = event.button
                    {
                        let _ = shell.update(cx, |this, cx| {
                            this.navigate_history(
                                direction == NavigationDirection::Forward,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full();
        let title = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "StarPlayer".into());
        let content = div()
            .id("player")
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.controls_dragging = event.pressed_button == Some(MouseButton::Left);
                this.update_navigation_hover(window, cx);
                this.reveal_controls(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.controls_dragging = false;
                    this.reveal_controls(cx);
                }),
            )
            // Native backdrop -> native video -> GPUI controls/library. Painting
            // an opaque GPUI root would cover the video rather than sit behind it.
            .text_color(cx.theme().foreground)
            .child(history_events)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::click_video))
                    .when_some(
                        self.native_video
                            .clone()
                            .filter(|_| self.path.is_some() && !self.show_settings),
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
            .when(self.path.is_none() || self.show_settings, |shell| {
                shell.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(cx.theme().background.opacity(1.0))
                        .child(self.browser.clone()),
                )
            })
            .when(self.resume_prompt.is_some() && self.path.is_some() && !self.show_settings && !self.player_focus_mode(), |shell| {
                let position = self.resume_prompt.as_ref().unwrap().position;
                let seconds = position.floor() as u64;
                let timestamp = if seconds >= 3600 {
                    format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
                } else {
                    format!("{}:{:02}", seconds / 60, seconds % 60)
                };
                shell.child(
                    div().absolute().left_4().right_4()
                        .bottom(px(if self.controls_visible { 140.0 } else { 24.0 }))
                        .flex().justify_center()
                        .child(
                            div().id("resume-toast").track_focus(&self.resume_focus)
                                .occlude().flex().items_center().gap_3().px_3().py_2()
                                .rounded_lg().bg(cx.theme().background.opacity(0.96))
                                .border_1().border_color(cx.theme().border).shadow_lg()
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                                    if let Some(prompt) = &mut this.resume_prompt {
                                        prompt.expired(Instant::now(), this.resume_focus.contains_focused(window, cx));
                                        prompt.hovered = *hovered;
                                    }
                                    this.reveal_controls(cx);
                                }))
                                .child(format!("Resume from {timestamp}?"))
                                .child(Button::new("resume-playback").label("Resume")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.resume_prompt = None;
                                        this.command(Command::SeekAbsolute(position), cx);
                                        window.focus(&this.focus, cx);
                                    })))
                                .child(Button::new("dismiss-resume").ghost().label("×")
                                    .tooltip("Dismiss resume prompt")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.resume_prompt = None;
                                        window.focus(&this.focus, cx);
                                        cx.notify();
                                    }))),
                        ),
                )
            })
            .when(
                self.controls_opacity > 0.0 && self.path.is_some() && !self.show_settings,
                |shell| {
                    shell
                        .child(
                            div()
                                .absolute()
                                .top(px(controls_offset + 8.0))
                                .opacity(opacity)
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
                                        ),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .relative()
                                        .occlude()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(Self::click_overlay),
                                        )
                                        .child(
                                            Button::new("file-info")
                                                .ghost()
                                                .icon(IconName::Info)
                                                .tooltip("File information")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.show_info = !this.show_info;
                                                    cx.stop_propagation();
                                                    this.video_click.take();
                                                    this.show_help = false;
                                                    this.reveal_controls(cx);
                                                    cx.notify();
                                                })),
                                        )
                                        .child(measure_bounds(info_trigger_bounds.clone())),
                                )
                                .when(window.is_fullscreen(), |s| {
                                    s.child(
                                        div()
                                            .occlude()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(Self::click_overlay),
                                            )
                                            .child(
                                                Button::new("fullscreen-settings")
                                                    .ghost()
                                                    .icon(IconName::Settings)
                                                    .tooltip("Settings")
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.set_settings(true, window, cx)
                                                        },
                                                    )),
                                            ),
                                    )
                                }),
                        )
                        .when(window.is_fullscreen(), |s| {
                            s.child(
                                div()
                                    .absolute()
                                    .top_4()
                                    .left(relative(0.25))
                                    .right(relative(0.25))
                                    .flex()
                                    .justify_center()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .max_w_full()
                                            .child(self.render_media_title(&title, true, cx)),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .id("transport-panel")
                                .occlude()
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                                .absolute()
                                .bottom(px(controls_offset))
                                .opacity(opacity)
                                .left_2()
                                .right_2()
                                .flex()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_xl()
                                .bg(cx.theme().background.opacity(0.70))
                                .border_1()
                                .border_color(cx.theme().border)
                                .shadow_lg()
                                .child(
                                    div()
                                        .flex()
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .child(self.media_controls.clone())
                                                .child(self.volume_controls.clone())
                                                .flex_shrink_0(),
                                        )
                                        .child(div().flex_1().min_w_0().child(self.seeker_bar.clone()))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .flex_shrink_0()
                                                .when(
                                                    !self.snapshot.tracks.is_empty(),
                                                    |controls| {
                                                        controls.child(
                                                            div().relative().child(Button::new("audio-mixer")
                                                                .ghost()
                                                                .icon(IconName::Settings2)
                                                                .tooltip("Audio track mixer")
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        this.show_tracks =
                                                                            !this.show_tracks;
                                                                        this.reveal_controls(cx);
                                                                        cx.notify();
                                                                    },
                                                                )))
                                                                .child(measure_bounds(mixer_trigger_bounds.clone())),
                                                        )
                                                    },
                                                )
                                                .child(
                                                    Button::new("fullscreen")
                                                        .ghost()
                                                        .icon(IconName::Maximize)
                                                        .tooltip("Toggle fullscreen (F)")
                                                        .on_click(cx.listener(
                                                            |_, _, window, _| {
                                                                window.toggle_fullscreen()
                                                            },
                                                        )),
                                                ),
                                        ),
                                ),
                        )
                },
            )
            .when(
                self.path.is_some() && self.controls_visible && !self.show_settings,
                |shell| {
                    let mut shell = shell;
                    for next in [false, true] {
                        let Some(target) = self.adjacent(next) else {
                            continue;
                        };
                        let hovered = self.hover_navigation == Some(next);
                        let image = hovered
                            .then(|| {
                                self.browser
                                    .update(cx, |browser, _| browser.thumbnail(&target))
                            })
                            .flatten();
                        shell = shell.child(
                            div()
                                .id(if next { "next-video" } else { "previous-video" })
                                .absolute()
                                .top(relative(0.5))
                                .mt(px(-120.0))
                                .w(px(220.0))
                                .h(px(240.0))
                                .when(next, |s| s.right_4())
                                .when(!next, |s| s.left_4())
                                .flex()
                                .flex_col()
                                .justify_center()
                                .when(next, |s| s.items_end())
                                .when(!next, |s| s.items_start())
                                .gap_2()
                                .when(hovered, |s| {
                                    s.child(
                                        div()
                                            .occlude()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(Self::click_overlay),
                                            )
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(
                                                Button::new(if next {
                                                    "next-clip"
                                                } else {
                                                    "previous-clip"
                                                })
                                                .ghost()
                                                .icon(if next {
                                                    IconName::ChevronRight
                                                } else {
                                                    IconName::ChevronLeft
                                                })
                                                .label(if next { "Next" } else { "Previous" })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if let Some(path) = this.adjacent(next) {
                                                        this.load(path, cx);
                                                    }
                                                })),
                                            )
                                            .child(
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
                                                            target
                                                                .file_name()
                                                                .unwrap_or_default()
                                                                .to_string_lossy()
                                                                .into_owned(),
                                                        ),
                                                    ),
                                            ),
                                    )
                                }),
                        );
                    }
                    shell
                },
            )
            .when(
                self.controls_visible && self.show_tracks && !self.show_settings,
                |shell| {
                    let trigger_bounds = mixer_trigger_bounds.clone();
                    shell.child(
                        div()
                            .id("audio-mixer-panel")
                            .absolute()
                            .bottom(px(128.0))
                            .right_4()
                            .w(px(380.0))
                            .occlude()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                if !trigger_bounds.get().contains(&event.position) {
                                    this.show_tracks = false;
                                    cx.notify();
                                }
                            }))
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
                },
            )
            .when(
                self.controls_visible && self.show_info && !self.show_settings,
                |shell| {
                    let trigger_bounds = info_trigger_bounds.clone();
                    shell.child(
                        file_info::panel(
                            &self.snapshot,
                            self.path.as_deref(),
                            cx,
                            Button::new("close-file-info")
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Close file information")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_info = false;
                                    cx.notify();
                                })),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::click_overlay))
                        .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            if !trigger_bounds.get().contains(&event.position) {
                                this.show_info = false;
                                cx.notify();
                            }
                        })),
                    )
                },
            )
            .when(
                self.controls_visible && self.show_help && !self.show_settings,
                |shell| {
                    shell.child(
                        div()
                            .id("keyboard-help")
                            .absolute()
                            .top(px(76.0))
                            .right_4()
                            .w(px(360.0))
                            .max_h(window.viewport_size().height - px(100.))
                            .overflow_y_scroll()
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
                            .children(cx.global::<keybinds::Keymap>().hints().into_iter().map(
                                |(action, shortcut)| {
                                    div()
                                        .flex()
                                        .justify_between()
                                        .py_1()
                                        .gap_4()
                                        .text_xs()
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(action),
                                        )
                                        .child(shortcut)
                                },
                            )),
                    )
                },
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.reveal_controls(cx)))
            .when(!window.is_fullscreen(), |shell| {
                shell.child(self.render_title_bar(&title, window, cx))
            })
            .child(div().relative().flex_1().min_h_0().child(content))
    }
}

// Measure the toggle buttons separately so outside-click dismissal cannot
// close a panel on mouse-down and accidentally reopen it on the button's click.
fn measure_bounds(bounds: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |layout, _, _| bounds.set(layout), |_, _, _, _| {})
        .absolute()
        .inset_0()
        .size_full()
}

fn advance_controls_opacity(current: f32, visible: bool, elapsed: Duration, reduced: bool) -> f32 {
    let target = if visible { 1.0 } else { 0.0 };
    if reduced {
        return target;
    }
    let step = elapsed.as_secs_f32() / 0.18;
    if visible {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

fn reduced_motion() -> bool {
    #[cfg(windows)]
    {
        // SPI_GETCLIENTAREAANIMATION reflects Windows' animation preference.
        let mut enabled: i32 = 1;
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
                0x1042,
                0,
                (&mut enabled as *mut i32).cast(),
                0,
            );
        }
        enabled == 0
    }
    #[cfg(not(windows))]
    false
}

fn command_reveals_controls(command: &Command) -> bool {
    !matches!(command, Command::FrameStep | Command::FrameBackStep)
}

fn controls_should_show(
    focus_mode: bool,
    persistent: bool,
    interacting: bool,
    idle: Duration,
) -> bool {
    !focus_mode && (persistent || interacting || idle < Duration::from_millis(1500))
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
    use super::{
        Command, advance_controls_opacity, command_reveals_controls, controls_should_show,
    };

    #[test]
    fn controls_animation_reverses_without_jumping_and_respects_reduced_motion() {
        let half = Duration::from_millis(90);
        let hidden = advance_controls_opacity(1.0, false, half, false);
        assert!((hidden - 0.5).abs() < 0.001);
        assert_eq!(advance_controls_opacity(hidden, true, half, false), 1.0);
        assert_eq!(
            advance_controls_opacity(1.0, false, Duration::from_secs(1), false),
            0.0
        );
        assert_eq!(
            advance_controls_opacity(0.4, true, Duration::ZERO, true),
            1.0
        );
        assert_eq!(
            advance_controls_opacity(0.4, false, Duration::ZERO, true),
            0.0
        );
    }
    use std::time::Duration;

    #[test]
    fn controls_hide_after_short_idle_regardless_of_playback_state() {
        assert!(controls_should_show(
            false,
            false,
            false,
            Duration::from_millis(1499)
        ));
        assert!(!controls_should_show(
            false,
            false,
            false,
            Duration::from_millis(1500)
        ));
        assert!(controls_should_show(
            false,
            true,
            false,
            Duration::from_secs(10)
        ));
        assert!(controls_should_show(
            false,
            false,
            true,
            Duration::from_secs(10)
        ));
    }

    #[test]
    fn focus_mode_overrides_activity_and_open_panels() {
        for persistent in [false, true] {
            for interacting in [false, true] {
                assert!(!controls_should_show(
                    true,
                    persistent,
                    interacting,
                    Duration::ZERO
                ));
            }
        }
    }

    #[test]
    fn frame_steps_do_not_reveal_controls() {
        assert!(!command_reveals_controls(&Command::FrameStep));
        assert!(!command_reveals_controls(&Command::FrameBackStep));
        assert!(command_reveals_controls(&Command::Toggle));
        assert!(command_reveals_controls(&Command::SeekRelative(10.0)));
    }
}

fn main() {
    if updater::run_helper() {
        return;
    }
    logging::attach_parent_terminal();
    logging::Print(logging::Level::Info, "Starting StarPlayer");
    if let Err(error) = clip_store::directory() {
        logging::Print(logging::Level::Error, &error);
        #[cfg(windows)]
        rfd::MessageDialog::new()
            .set_title("StarPlayer could not open its data")
            .set_description(&error)
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
    application().with_assets(app_assets::AppAssets).run(|cx| {
        init(cx);
        theme::apply(cx);
        cx.set_global(keybinds::Keymap::open(
            clip_store::directory()
                .unwrap_or_default()
                .join("keybinds.json"),
        ));
        let options = WindowOptions {
            window_background: WindowBackgroundAppearance::Transparent,
            titlebar: Some(TitlebarOptions {
                title: Some("StarPlayer".into()),
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
                    player.open_file(PathBuf::from(path), cx);
                }
                player
            });
            player_root(player, window, cx)
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
    #[ignore = "interactive GPUI overlay hit-testing with STAR_PLAYER_AUDIO_PROBE media"]
    fn mixer_clicks_do_not_control_video() {
        use gpui_kit::{
            MouseButton, MouseDownEvent, MouseUpEvent, WindowBackgroundAppearance, WindowOptions,
            application, init, point, px, rgba,
        };
        let path = PathBuf::from(
            std::env::var_os("STAR_PLAYER_AUDIO_PROBE")
                .expect("Set STAR_PLAYER_AUDIO_PROBE to the test media"),
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
#[cfg(all(test, windows))]
mod layer_tests {
    use super::{Command, app_assets};
    #[test]
    #[ignore = "interactive GPUI/native background composition and transition pixels"]
    fn opaque_background_survives_video_transitions() {
        use gpui_kit::*;
        use std::{
            rc::Rc,
            sync::{Arc, Mutex},
            time::Duration,
        };
        use windows_sys::Win32::{
            Foundation::{POINT, RECT},
            Graphics::Gdi::{ClientToScreen, GetDC, GetPixel, ReleaseDC},
            UI::WindowsAndMessaging::*,
        };

        struct Layers {
            playback: crate::playback::Playback,
            host: Rc<crate::native_video::NativeVideo>,
            video: bool,
        }
        impl Render for Layers {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let host = self.host.clone();
                let video = self.video;
                div()
                    .relative()
                    .size_full()
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                if video {
                                    host.layout(bounds, window.scale_factor());
                                } else {
                                    host.hide();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(80.))
                            .h(px(40.))
                            .bg(rgb(0xff0000)),
                    )
            }
        }

        // A white sentinel behind the app makes any transparent hole detectable.
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let sentinel = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | WS_VISIBLE | 0x00000006,
                0,
                0,
                1400,
                1000,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!sentinel.is_null());
        struct Sentinel(windows_sys::Win32::Foundation::HWND);
        impl Drop for Sentinel {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let _sentinel = Sentinel(sentinel);
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first.avi");
        let second = fixture.path().join("second.avi");
        crate::playback::test_fixtures::avi(&first, 0, true);
        crate::playback::test_fixtures::avi(&second, 0, true);
        let report = Arc::new(Mutex::new(Vec::new()));
        let samples = report.clone();
        application()
            .with_assets(app_assets::AppAssets)
            .run(move |cx| {
                init(cx);
                let mut layers = None;
                let handle = cx
                    .open_window(
                        WindowOptions {
                            window_background: WindowBackgroundAppearance::Transparent,
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(100.), px(100.)),
                                size(px(640.), px(480.)),
                            ))),
                            ..Default::default()
                        },
                        |window, cx| {
                            let host =
                                Rc::new(crate::native_video::NativeVideo::new(window).unwrap());
                            let playback = crate::playback::Playback::native(host.hwnd()).unwrap();
                            let view = cx.new(|_| Layers {
                                host,
                                playback,
                                video: false,
                            });
                            layers = Some(view.clone());
                            cx.new(|cx| {
                                gpui_kit::base::Root::new(view, window, cx).bg(rgba(0x00000000))
                            })
                        },
                    )
                    .unwrap();
                let layers = layers.unwrap();
                cx.spawn(async move |cx| {
                    let sample = |window: &Window, control: bool| -> u32 {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        let RawWindowHandle::Win32(raw) =
                            HasWindowHandle::window_handle(window).unwrap().as_raw()
                        else {
                            unreachable!()
                        };
                        unsafe {
                            let hwnd = raw.hwnd.get() as _;
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            // Keep screen-pixel assertions independent of other apps
                            // stealing focus while this opt-in desktop test runs.
                            assert_ne!(
                                SetWindowPos(
                                    hwnd,
                                    HWND_TOPMOST,
                                    0,
                                    0,
                                    0,
                                    0,
                                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE
                                ),
                                0
                            );
                            assert_ne!(IsWindowVisible(hwnd), 0);
                            let mut rect: RECT = std::mem::zeroed();
                            GetClientRect(hwnd, &mut rect);
                            let mut pos = if control {
                                POINT { x: 10, y: 10 }
                            } else {
                                POINT {
                                    x: rect.right / 2,
                                    y: rect.bottom / 2,
                                }
                            };
                            ClientToScreen(hwnd, &mut pos);
                            let dc = GetDC(std::ptr::null_mut());
                            let color = GetPixel(dc, pos.x, pos.y);
                            ReleaseDC(std::ptr::null_mut(), dc);
                            color
                        }
                    };
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    handle
                        .update(cx, |_, window, _| {
                            samples
                                .lock()
                                .unwrap()
                                .push(("background", sample(window, false)));
                            samples
                                .lock()
                                .unwrap()
                                .push(("controls", sample(window, true)));
                        })
                        .unwrap();
                    handle
                        .update(cx, |_, _, cx| {
                            layers.update(cx, |view, cx| {
                                view.video = true;
                                view.playback.send(Command::Load(first)).unwrap();
                                cx.notify();
                            });
                        })
                        .unwrap();
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    handle
                        .update(cx, |_, window, cx| {
                            samples
                                .lock()
                                .unwrap()
                                .push(("video", sample(window, false)));
                            layers.update(cx, |view, _| {
                                view.playback.send(Command::Load(second)).unwrap()
                            });
                        })
                        .unwrap();
                    for _ in 0..20 {
                        cx.background_executor()
                            .timer(Duration::from_millis(20))
                            .await;
                        handle
                            .update(cx, |_, window, _| {
                                samples
                                    .lock()
                                    .unwrap()
                                    .push(("transition", sample(window, false)));
                            })
                            .unwrap();
                    }
                    handle
                        .update(cx, |_, _, cx| {
                            layers.update(cx, |view, cx| {
                                view.video = false;
                                view.host.hide();
                                cx.notify();
                            });
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            samples
                                .lock()
                                .unwrap()
                                .push(("hidden video", sample(window, false)));
                            layers.update(cx, |view, _| {
                                view.playback.send(Command::Shutdown).unwrap();
                            });
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    handle
                        .update(cx, |_, window, _| {
                            samples
                                .lock()
                                .unwrap()
                                .push(("decoder shutdown", sample(window, false)));
                            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                            let RawWindowHandle::Win32(raw) =
                                HasWindowHandle::window_handle(window).unwrap().as_raw()
                            else {
                                unreachable!()
                            };
                            unsafe {
                                SetWindowPos(
                                    raw.hwnd.get() as _,
                                    std::ptr::null_mut(),
                                    0,
                                    0,
                                    1400,
                                    800,
                                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                                );
                            }
                        })
                        .unwrap();
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    handle
                        .update(cx, |_, window, cx| {
                            samples
                                .lock()
                                .unwrap()
                                .push(("resized background", sample(window, false)));
                            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                            let RawWindowHandle::Win32(raw) =
                                HasWindowHandle::window_handle(window).unwrap().as_raw()
                            else {
                                unreachable!()
                            };
                            let mut rect: RECT = unsafe { std::mem::zeroed() };
                            unsafe {
                                GetClientRect(raw.hwnd.get() as _, &mut rect);
                            }
                            assert_eq!(
                                layers.read(cx).host.backdrop_size(),
                                [rect.right, rect.bottom],
                                "Background must cover the entire resized client area"
                            );
                        })
                        .unwrap();
                    handle.update(cx, |_, _, cx| cx.quit()).unwrap();
                })
                .detach();
                cx.activate(true);
            });
        let samples = report.lock().unwrap();
        assert_eq!(samples.len(), 26);
        for (stage, color) in samples.iter() {
            match *stage {
                "controls" => assert_eq!(
                    *color, 0x000000ff,
                    "GPUI controls must remain above background"
                ),
                "video" => assert!(
                    (*color >> 16) >= 128 && (*color & 0xffff) == 0,
                    "Native video must remain above background: {color:#x}"
                ),
                "transition" => assert!(
                    *color == 0 || ((*color >> 16) >= 128 && (*color & 0xffff) == 0),
                    "Transparent transition pixel: {color:#x}"
                ),
                _ => assert_eq!(
                    *color, 0,
                    "Background disappeared during {stage}: {samples:?}"
                ),
            }
        }
    }
}
