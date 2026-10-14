use components::{
    audio_tracks::{AudioTracks, TrackGainEvent},
    media_controls::{MediaControls, MediaControlsEvent},
    seeker_bar::{SeekerBar, SeekerEvent},
    volume_controls::{VolumeControls, VolumeEvent},
};
use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use playback::{Command, Playback, Snapshot};
use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};

mod components;
mod logging;
mod media_keys;
mod native_video;
mod playback;
mod preview;
mod runtime;
mod theme;

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
    media_keys: media_keys::MediaKeys,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl PlayerShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let media_controls = cx.new(|_| MediaControls::default());
        let volume_controls = cx.new(VolumeControls::new);
        let seeker_bar = cx.new(|cx| SeekerBar::new(0.0, cx));
        let audio_tracks = cx.new(AudioTracks::new);
        let subscriptions = vec![
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
        let mut shell = Self {
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
            media_keys: media_keys::MediaKeys::default(),
            _subscriptions: subscriptions,
            _poll: poll,
        };
        if let Some(path) = std::env::args_os().nth(1) {
            shell.load(PathBuf::from(path), cx);
        }
        shell
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Some(player) = &self.playback {
            if let Err(error) = player.send(command) {
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    fn shutdown(&mut self) {
        drop(self.playback.take());
        if let Some(host) = self.native_video.take() {
            host.destroy();
        }
    }

    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        if let Some(player) = &self.playback {
            self.snapshot = player.snapshot();
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
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control && key == "o" {
            self.open(window, cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform {
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
}

impl Render for PlayerShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.error.clone().or_else(|| self.snapshot.error.clone());
        let title = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Spacer".into());
        div().size_full().flex().flex_col().track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            // GPUI's DirectComposition content can sit above child HWNDs.
            // Leave the video rectangle alpha-transparent, paint chrome only.
            .text_color(cx.theme().foreground)
            .child(div().flex().items_center().justify_between().px_6().py_3().gap_3()
                .bg(cx.theme().background)
                .child(div().flex_1().text_sm().child(title))
                .child(Button::new("open-media").ghost().label("Open media").tooltip("Open media (Ctrl+O)")
                    .on_click(cx.listener(|this, _, window, cx| this.open(window, cx))))
                .child(Button::new("help").ghost().label("Shortcuts")
                    .on_click(cx.listener(|this, _, _, cx| { this.show_help = !this.show_help; cx.notify(); }))))
            .child(div().flex().flex_1().min_h_0()
                .child(div().w(px(16.0)).flex_shrink_0().bg(cx.theme().background))
                .child(div().relative().flex_1().min_h_0().min_w_0().overflow_hidden()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
                .when_some(self.native_video.clone(), |area, host| area.child(
                    canvas(move |bounds, window, _| host.layout(bounds, window.scale_factor()), |_, _, _, _| {}).size_full())))
                .child(div().w(px(16.0)).flex_shrink_0().bg(cx.theme().background)))
            .child(div().h(px(16.0)).flex_shrink_0().bg(cx.theme().background))
                .when(self.show_help, |area| area.child(div().p_4()
                    .bg(cx.theme().background).text_sm()
                    .child("Ctrl+O open | Space/K play-pause | P play | Pause key pause | S stop\nLeft/J and Right/L seek 10s, Shift for 60s | Home/End start/end | 0-9 seek by 10%\nPaused: , previous frame | . next frame\nUp/Down master volume | M mute | Shift+,/. speed | R reset speed\n[/] select audio track | -/+ track volume | A toggle selected track\nF fullscreen | H/? shortcuts | Esc hide shortcuts\nMedia play/pause, stop and previous/next work while Spacer is focused.\nSystem volume keys keep their normal Windows behavior.")))
            .when_some(error, |shell, error| shell.child(div().bg(cx.theme().background).px_6().py_2().text_sm().child(error)))
            .child(div().flex().flex_col().flex_shrink_0().gap_3().px_6().pt_3().pb_5()
                .bg(cx.theme().background)
                .border_t_1().border_color(cx.theme().border)
                .child(self.seeker_bar.clone())
                .child(div().flex().items_center().justify_between().gap_4()
                    .child(self.media_controls.clone())
                    .child(div().text_xs().child(format!("{:.2}x", self.snapshot.speed)))
                    .child(self.volume_controls.clone()))
                .when(!self.snapshot.tracks.is_empty(), |controls| controls.child(self.audio_tracks.clone())))
    }
}

fn main() {
    logging::Print(logging::Level::Info, "Starting Spacer");
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        theme::apply(cx);
        let options = WindowOptions {
            window_background: WindowBackgroundAppearance::Transparent,
            titlebar: Some(TitlebarOptions {
                title: Some("Spacer".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(960.0), px(700.0)),
                cx,
            ))),
            window_min_size: Some(size(px(680.0), px(480.0))),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let player = cx.new(|cx| PlayerShell::new(window, cx));
            cx.new(|cx| gpui_kit::base::Root::new(player, window, cx).bg(rgba(0x00000000)))
        })
        .expect("Failed to open player window");
        cx.activate(true);
    });
}

impl Drop for PlayerShell {
    fn drop(&mut self) {
        self.shutdown();
    }
}
