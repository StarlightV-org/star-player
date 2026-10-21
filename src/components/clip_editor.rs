use crate::{
    clip_export::{self, AudioMode, AudioSelection, Export, Format, Job, Media, Quality},
    playback::Snapshot,
};
use gpui_kit::{
    base::{Disableable as _, Selectable as _},
    component::{
        ActiveTheme as _, Icon, IndexPath, Sizable as _,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState},
        select::{Select, SelectEvent, SelectItem, SelectState},
        slider::{Slider, SliderEvent, SliderState},
    },
    prelude::FluentBuilder as _,
    *,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub enum EditorEvent {
    Changed,
    Preview(f64, f64),
    Closed,
    Play(PathBuf),
    Finished(Result<PathBuf, String>),
}

struct Track {
    index: u32,
    title: String,
    selected: bool,
    gain: Entity<SliderState>,
}

impl SelectItem for Format {
    type Value = Self;
    fn title(&self) -> SharedString {
        self.label().into()
    }
    fn value(&self) -> &Self {
        self
    }
}

pub struct ClipEditor {
    pub visible: bool,
    source: Option<PathBuf>,
    duration: f64,
    position: f64,
    current: bool,
    start: Entity<InputState>,
    end: Entity<InputState>,
    format: Format,
    format_select: Entity<SelectState<Vec<Format>>>,
    quality: Quality,
    audio_mode: AudioMode,
    tracks: Vec<Track>,
    selected_track: usize,
    track_subscriptions: Vec<Subscription>,
    gains: BTreeMap<u32, f64>,
    media: Option<Media>,
    job: Option<Job>,
    ffmpeg: Option<PathBuf>,
    checking_tools: bool,
    downloading_tools: bool,
    download_progress: Option<(u64, u64)>,
    exporting: bool,
    picking: bool,
    cancelling: bool,
    progress: f64,
    error: Option<String>,
    output: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<EditorEvent> for ClipEditor {}

impl ClipEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("0:00:00.000"));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("0:00:00.000"));
        let format_select = cx.new(|cx| {
            SelectState::new(
                Format::OPTIONS.to_vec(),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let mut subscriptions = [&start, &end]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.emit(EditorEvent::Changed);
                        cx.notify();
                    }
                })
            })
            .collect::<Vec<_>>();
        subscriptions.push(cx.subscribe(&format_select, |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(format)) = event {
                if this.busy() {
                    return;
                }
                this.format = *format;
                if format.audio_only() {
                    this.audio_mode = AudioMode::Merge;
                }
                this.error = None;
                cx.notify();
            }
        }));
        Self {
            visible: false,
            source: None,
            duration: 0.,
            position: 0.,
            current: false,
            start,
            end,
            format: Format::Mp4,
            format_select,
            quality: Quality::Balanced,
            audio_mode: AudioMode::Separate,
            tracks: vec![],
            selected_track: 0,
            track_subscriptions: vec![],
            gains: BTreeMap::new(),
            media: None,
            job: None,
            ffmpeg: None,
            checking_tools: false,
            downloading_tools: false,
            download_progress: None,
            exporting: false,
            picking: false,
            cancelling: false,
            progress: 0.,
            error: None,
            output: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn busy(&self) -> bool {
        self.exporting || self.picking || self.downloading_tools
    }

    pub fn open(
        &mut self,
        source: PathBuf,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.visible = true;
        if self.busy() {
            cx.notify();
            return;
        }
        self.source = Some(source.clone());
        self.duration = snapshot.duration;
        self.position = snapshot.position;
        self.current = true;
        self.media = None;
        self.ffmpeg = None;
        self.checking_tools = false;
        self.download_progress = None;
        self.tracks.clear();
        self.track_subscriptions.clear();
        self.selected_track = 0;
        self.error = None;
        self.output = None;
        self.progress = 0.;
        self.cancelling = false;
        // Use mpv's explicit FFmpeg index, never its track ID or row position.
        self.gains = snapshot
            .metadata
            .get("track-list")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "audio")
            .filter_map(|v| {
                let index = u32::try_from(v["ff-index"].as_u64()?).ok()?;
                let id = v["id"].as_i64()?;
                Some((
                    index,
                    snapshot
                        .tracks
                        .iter()
                        .find(|t| t.id == id)
                        .map_or(1., |t| t.gain),
                ))
            })
            .collect();
        self.set_range(0., self.duration, window, cx);
        self.job = Some(Job::probe(source));
        cx.notify();
    }

    pub fn change_media(&mut self, cx: &mut Context<Self>) {
        self.current = false;
        if !self.busy() {
            self.visible = false;
            self.job.take();
        }
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn shutdown(&mut self) {
        self.job.take();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.visible = false;
        if let Some(job) = &self.job {
            job.cancel();
        }
        cx.emit(EditorEvent::Closed);
        cx.notify();
    }

    pub fn range(&self, path: Option<&Path>, cx: &App) -> Option<(f64, f64)> {
        if self.busy() {
            return None;
        }
        self.playback_range(path, cx)
    }

    pub fn playback_range(&self, path: Option<&Path>, cx: &App) -> Option<(f64, f64)> {
        if !self.visible || self.source.as_deref() != path {
            return None;
        }
        self.valid_range(cx)
    }

    fn valid_range(&self, cx: &App) -> Option<(f64, f64)> {
        let start = clip_export::parse_timestamp(&self.start.read(cx).value())?;
        let end = clip_export::parse_timestamp(&self.end.read(cx).value())?;
        (start < end && end <= self.duration + 0.001).then_some((start, end))
    }

    pub fn set_range(&mut self, start: f64, end: f64, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.start.update(cx, |s, cx| {
            s.set_value(clip_export::timestamp(start), window, cx)
        });
        self.end.update(cx, |s, cx| {
            s.set_value(clip_export::timestamp(end), window, cx)
        });
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn poll(
        &mut self,
        path: Option<&Path>,
        position: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.current = self.source.as_deref() == path && path.is_some();
        if self.current {
            self.position = position;
        }
        let events = self
            .job
            .as_ref()
            .map(|j| j.events().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            match event {
                clip_export::Event::Probed(result) => {
                    self.job.take();
                    match result {
                        Ok(media) => {
                            self.duration = media.duration;
                            self.set_range(0., media.duration, window, cx);
                            self.tracks = media
                                .audio
                                .iter()
                                .enumerate()
                                .map(|(i, stream)| {
                                    let gain = self.gains.get(&stream.index).copied().unwrap_or(1.);
                                    let slider = cx.new(|_| {
                                        SliderState::new()
                                            .min(0.)
                                            .max((gain * 100.).max(100.) as f32)
                                            .step(1.)
                                            .default_value((gain * 100.) as f32)
                                    });
                                    self.track_subscriptions.push(cx.subscribe(
                                        &slider,
                                        move |this, _, event: &SliderEvent, cx| {
                                            if matches!(event, SliderEvent::Change(_)) {
                                                this.selected_track = i;
                                                cx.notify();
                                            }
                                        },
                                    ));
                                    Track {
                                        index: stream.index,
                                        title: stream.title.clone(),
                                        selected: true,
                                        gain: slider,
                                    }
                                })
                                .collect();
                            self.media = Some(media);
                            self.checking_tools = true;
                            self.job = Some(Job::find_tools());
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                clip_export::Event::Tools(result) => {
                    self.job.take();
                    self.checking_tools = false;
                    self.downloading_tools = false;
                    self.cancelling = false;
                    match result {
                        Ok(path) => {
                            self.ffmpeg = path;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                clip_export::Event::DownloadProgress(done, total) => {
                    self.download_progress = Some((done, total))
                }
                clip_export::Event::Progress(progress) => self.progress = progress,
                clip_export::Event::Finished(result) => {
                    self.job.take();
                    self.exporting = false;
                    self.cancelling = false;
                    match &result {
                        Ok(path) => {
                            self.progress = 1.;
                            self.output = Some(path.clone());
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.clone()),
                    }
                    cx.emit(EditorEvent::Finished(result));
                }
            }
            cx.notify();
        }
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.checking_tools || self.ffmpeg.is_none() || !self.ready() {
            return;
        }
        let Some((start, end)) = self.valid_range(cx) else {
            return;
        };
        let Some(source) = self.source.clone() else {
            return;
        };
        let audio = self
            .tracks
            .iter()
            .filter(|t| t.selected && self.audio_mode != AudioMode::None)
            .map(|t| {
                let gain = t.gain.read(cx).value().start() as f64 / 100.;
                AudioSelection {
                    index: t.index,
                    gain,
                }
            })
            .collect();
        let mut spec = Export {
            source: source.clone(),
            destination: PathBuf::new(),
            start,
            end,
            format: self.format,
            quality: self.quality,
            audio_mode: self.audio_mode,
            audio,
        };
        let suggested = format!(
            "{}-clip.{}",
            source.file_stem().unwrap_or_default().to_string_lossy(),
            self.format.extension()
        );
        let selection =
            cx.prompt_for_new_path(source.parent().unwrap_or(Path::new(".")), Some(&suggested));
        self.picking = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = selection.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.picking = false;
                match result {
                    Ok(Ok(Some(mut path))) => {
                        if path.extension().is_none() {
                            path.set_extension(spec.format.extension());
                        }
                        spec.destination = path;
                        this.exporting = true;
                        this.cancelling = false;
                        this.output = None;
                        this.progress = 0.;
                        this.job = Some(Job::export(spec));
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    _ => {}
                }
                cx.emit(EditorEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    fn ready(&self) -> bool {
        self.media.as_ref().is_some_and(|m| {
            if self.format.audio_only() {
                self.audio_mode == AudioMode::Merge && self.tracks.iter().any(|t| t.selected)
            } else {
                m.video.is_some() && !m.hdr
            }
        })
    }

    fn choose_ffmpeg(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.checking_tools {
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose ffmpeg.exe".into()),
        });
        self.picking = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = selection.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.picking = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            this.checking_tools = true;
                            this.error = None;
                            this.job = Some(Job::select_tools(path.clone()));
                        }
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ClipEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let ready = self.ready();
        let range = self.valid_range(cx);
        let start_action = Button::new("clip-start-here")
            .ghost()
            .xsmall()
            .icon(Icon::new(gpui_kit::assets::IconName::ArrowLeftToLine))
            .tooltip("Set start at playhead")
            .disabled(busy || !self.current)
            .on_click(cx.listener(|this, _, window, cx| {
                let end = clip_export::parse_timestamp(&this.end.read(cx).value())
                    .unwrap_or(this.duration);
                this.set_range(this.position, end, window, cx);
            }));
        let end_action = Button::new("clip-end-here")
            .ghost()
            .xsmall()
            .icon(Icon::new(gpui_kit::assets::IconName::ArrowRightToLine))
            .tooltip("Set end at playhead")
            .disabled(busy || !self.current)
            .on_click(cx.listener(|this, _, window, cx| {
                let start =
                    clip_export::parse_timestamp(&this.start.read(cx).value()).unwrap_or(0.);
                this.set_range(start, this.position, window, cx);
            }));
        let range_actions = Button::new("clip-preview")
            .ghost()
            .label("Preview range")
            .disabled(busy || !self.current || range.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some((start, end)) = this.valid_range(cx) {
                    cx.emit(EditorEvent::Preview(start, end));
                }
            }));
        let qualities = [Quality::High, Quality::Balanced, Quality::Small]
            .into_iter()
            .enumerate()
            .map(|(i, quality)| {
                Button::new(("clip-quality", i))
                    .ghost()
                    .label(quality.label())
                    .selected(self.quality == quality)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.quality = quality;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        let modes = [
            (AudioMode::Separate, "Separate tracks"),
            (AudioMode::Merge, "Merge to stereo"),
            (AudioMode::None, "No audio"),
        ]
        .into_iter()
        .filter(|(mode, _)| !self.format.audio_only() || *mode == AudioMode::Merge)
        .enumerate()
        .map(|(i, (mode, label))| {
            Button::new(("clip-audio-mode", i))
                .ghost()
                .label(label)
                .selected(self.audio_mode == mode)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.audio_mode = mode;
                    cx.notify();
                }))
        })
        .collect::<Vec<_>>();
        let tracks = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, track)| {
                div()
                    .id(("clip-audio-track", i))
                    .flex()
                    .flex_shrink_0()
                    .h(px(40.))
                    .items_center()
                    .gap_3()
                    .py_1()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_track = i;
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(112.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Checkbox::new(("clip-audio-select", i))
                                    .checked(track.selected)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                        this.tracks[i].selected = *checked;
                                        this.selected_track = i;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .truncate()
                                    .text_color(if i == self.selected_track {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(track.title.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Slider::new(&track.gain).disabled(busy || !track.selected)),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .text_xs()
                            .child(format!("{:.0}%", track.gain.read(cx).value().start())),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .id("clip-editor")
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_lg().child("Create a new clip"))
                    .child(
                        Button::new("close-clip-editor")
                            .ghost()
                            .label("Close")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .when_some(self.source.clone(), |s, path| {
                s.child(
                    div().text_sm().truncate().child(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                )
            })
            .when(self.media.is_none() && self.error.is_none(), |s| {
                s.child("Reading media information…")
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().child("Start"))
                            .child(Input::new(&self.start).disabled(busy).suffix(start_action)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().child("End"))
                            .child(Input::new(&self.end).disabled(busy).suffix(end_action)),
                    ),
            )
            .when(range.is_none(), |s| {
                s.child(
                    div()
                        .text_xs()
                        .child("Enter a start before the end, within the video duration."),
                )
            })
            .when_some(range, |s, (start, end)| {
                s.child(div().text_xs().child(format!(
                    "Selected duration: {}",
                    clip_export::timestamp(end - start)
                )))
            })
            .child(range_actions)
            .child(div().text_sm().child("Format"))
            .child(Select::new(&self.format_select).disabled(busy))
            .child(div().flex().flex_wrap().gap_2().children(qualities))
            .child(div().text_sm().child("Audio"))
            .child(div().flex().flex_wrap().gap_2().children(modes))
            .when(self.audio_mode != AudioMode::None, |s| s.children(tracks))
            .when(
                self.media.as_ref().is_some_and(|m| m.hdr) && !self.format.audio_only(),
                |s| {
                    s.child(div().text_sm().child(
                        "HDR video export is not supported. Audio-only export is available.",
                    ))
                },
            )
            .when(
                self.format.audio_only()
                    && self.media.is_some()
                    && !self.tracks.iter().any(|t| t.selected),
                |s| s.child(div().text_sm().child("Select at least one audio track.")),
            )
            .when_some(self.error.clone(), |s, error| {
                s.child(div().text_sm().child(error))
            })
            .when(self.checking_tools, |s| s.child(div().text_sm().child("Checking local FFmpeg capabilities…")))
            .when(self.media.is_some() && self.ffmpeg.is_none() && !busy && !self.checking_tools, |s| {
                let package = crate::export_tools::package();
                let description = match &package {
                    Ok(package) => format!("Export requires FFmpeg. Download the package tested with StarPlayer {} from its GitHub release? {:.1} MiB download, {:.1} MiB disk space. No system installation or PATH changes.",
                        env!("CARGO_PKG_VERSION"), package.size as f64 / 1_048_576., package.unpacked_size() as f64 / 1_048_576.),
                    Err(error) => format!("Export requires FFmpeg. {error}"),
                };
                s.child(div().text_sm().child(description))
                    .child(div().flex().flex_wrap().gap_2()
                        .child(Button::new("download-export-ffmpeg").label("Download FFmpeg")
                            .disabled(package.is_err())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.downloading_tools = true;
                                this.download_progress = None;
                                this.cancelling = false;
                                this.error = None;
                                this.job = Some(Job::download_tools());
                                cx.notify();
                            })))
                        .child(Button::new("choose-export-ffmpeg").ghost().label("Choose ffmpeg.exe…")
                            .on_click(cx.listener(|this, _, window, cx| this.choose_ffmpeg(window, cx)))))
            })
            .when_some(self.ffmpeg.clone(), |s, path| {
                s.child(div().text_xs().child(format!("FFmpeg: {}", crate::clip_store::display_path(&path))))
                    .child(Button::new("change-export-ffmpeg").ghost().label("Choose another FFmpeg…")
                        .disabled(busy || self.checking_tools)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_ffmpeg(window, cx))))
            })
            .when(busy, |s| {
                let status = if self.picking {
                    "Choose where to save…".to_owned()
                } else if self.cancelling {
                    "Cancelling…".to_owned()
                } else if self.downloading_tools {
                    match self.download_progress {
                        Some((done, total)) if done < total => format!("Downloading FFmpeg… {:.0}%", done as f64 * 100. / total.max(1) as f64),
                        _ => "Preparing and verifying FFmpeg…".to_owned(),
                    }
                } else {
                    format!("Exporting… {:.0}%", self.progress * 100.)
                };
                s.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(status)
                        .child(
                            Button::new("cancel-clip-export")
                                .ghost()
                                .label(if self.downloading_tools { "Cancel download" } else { "Cancel export" })
                                .disabled(self.picking || self.cancelling)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(job) = &this.job {
                                        job.cancel();
                                        this.cancelling = true;
                                        cx.notify();
                                    }
                                })),
                        ),
                )
            })
            .when(!busy, |s| {
                s.child(
                    Button::new("save-new-clip")
                        .label(if self.format.audio_only() {
                            "Save audio…"
                        } else {
                            "Save new clip…"
                        })
                        .disabled(!ready || range.is_none() || self.ffmpeg.is_none() || self.checking_tools)
                        .on_click(cx.listener(|this, _, window, cx| this.save_as(window, cx))),
                )
            })
            .when_some(self.output.clone(), |s, path| {
                let play = path.clone();
                s.child(div().text_sm().child("New clip saved. Original unchanged."))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("play-exported-clip")
                                    .ghost()
                                    .label("Play exported clip")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(EditorEvent::Play(play.clone()))
                                    })),
                            )
                            .child(
                                Button::new("reveal-exported-clip")
                                    .ghost()
                                    .label("Reveal in File Explorer")
                                    .on_click(move |_, window, cx| {
                                        crate::file_explorer::reveal(path.clone(), window, cx)
                                    }),
                            ),
                    )
            })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use crate::{PlayerShell, clip_store, components::clip_browser::ClipBrowser};
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[test]
    #[ignore = "interactive GPUI/native clip editor preview and navigation"]
    fn editor_preview_and_navigation_preserve_original() {
        use super::*;
        use crate::{Command, components::media_controls::MediaControlsEvent};
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("store");
        let source = temp.path().join("original.avi");
        let other = temp.path().join("other.avi");
        crate::playback::test_fixtures::avi_timed(&source, 2, true, 64, 48, 10, 30);
        crate::playback::test_fixtures::avi(&other, 0, false);
        let original = std::fs::read(&source).unwrap();
        let before = std::fs::metadata(&source).unwrap();
        let original_permissions = before.permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&source, permissions).unwrap();
        clip_store::save_config(
            &store,
            &clip_store::Config {
                clip_discovery: clip_store::Discovery::Declined,
                steam_discovery: clip_store::Discovery::Declined,
                updates: clip_store::UpdateConfig {
                    automatic_check: false,
                    automatic_download: false,
                },
                ..Default::default()
            },
        )
        .unwrap();
        let outcome = Arc::new(Mutex::new(None));
        let report = outcome.clone();
        let clip = source.clone();
        crate::application()
            .with_assets(crate::app_assets::AppAssets)
            .run(move |cx| {
                crate::init(cx);
                let mut entity = None;
                let window = cx
                    .open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(100.), px(100.)),
                                size(px(1100.), px(850.)),
                            ))),
                            window_background: WindowBackgroundAppearance::Transparent,
                            ..Default::default()
                        },
                        |window, cx| {
                            let browser = cx.new(|cx| ClipBrowser::for_test(store, cx));
                            let player =
                                cx.new(|cx| PlayerShell::with_browser(browser, window, cx));
                            entity = Some(player.clone());
                            crate::player_root(player, window, cx)
                        },
                    )
                    .unwrap();
                let player = entity.unwrap();
                cx.spawn(async move |cx| {
                    let mut stage = 0;
                    let mut result = Err("Clip editor lifecycle timed out".to_owned());
                    for _ in 0..400 {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        let update = window.update(cx, |_, window, cx| {
                            player.update(cx, |shell, cx| -> Result<bool, String> {
                                match stage {
                                    0 if shell.browser.read(cx).audio_ready() => {
                                        shell.load_media_at(clip.clone(), 0., true, cx);
                                        stage = 1;
                                    }
                                    1 if shell.snapshot.path.as_ref() == Some(&clip)
                                        && shell.snapshot.duration > 0. =>
                                    {
                                        shell.edit_current(window, cx);
                                        stage = 2;
                                    }
                                    2 if shell.clip_editor.read(cx).media.is_some() => {
                                        shell.clip_editor.update(cx, |editor, cx| {
                                            editor.set_range(0.35, 1.35, window, cx);
                                            editor.tracks[0].gain.update(cx, |gain, cx| {
                                                gain.set_value(25., window, cx)
                                            });
                                            cx.emit(EditorEvent::Preview(0.35, 1.35));
                                        });
                                        if shell.clip_editor.read(cx).range(Some(&clip), cx)
                                            != Some((0.35, 1.35))
                                        {
                                            return Err("Fractional range was not retained".into());
                                        }
                                        stage = 3;
                                    }
                                    3 if shell.range_preview.is_none()
                                        && shell.snapshot.paused
                                        && shell.snapshot.position >= 1.25 =>
                                    {
                                        if shell.snapshot.tracks.iter().any(|t| t.gain != 1.) {
                                            return Err(
                                                "Export gains changed playback gains".into()
                                            );
                                        }
                                        shell.media_controls.update(cx, |_, cx| cx.emit(MediaControlsEvent::Toggle));
                                        stage = 4;
                                    }
                                    4 if !shell.snapshot.paused && shell.snapshot.position >= 0.3 && shell.snapshot.position < 0.9 => {
                                        // The regular play button restarted from the selected start.
                                        stage = 5;
                                    }
                                    5 if shell.segment_ended && shell.snapshot.paused => {
                                        shell.run_action("toggle", window, cx);
                                        stage = 6;
                                    }
                                    6 if !shell.snapshot.paused && shell.snapshot.position >= 0.3 && shell.snapshot.position < 0.9 => {
                                        shell.command(Command::Pause, cx);
                                        shell.run_action("seek_0", window, cx);
                                        stage = 7;
                                    }
                                    7 if shell.snapshot.paused && (shell.snapshot.position - 0.35).abs() < 0.11 => {
                                        shell.clip_editor.update(cx, |editor, cx| {
                                            editor.audio_mode = AudioMode::None;
                                            editor.format_select.update(cx, |select, cx| {
                                                select.set_selected_value(&Format::Mp3, window, cx);
                                                cx.emit(SelectEvent::Confirm(Some(Format::Mp3)));
                                            });
                                        });
                                        stage = 8;
                                    }
                                    8 if shell.clip_editor.read(cx).format == Format::Mp3 => {
                                        let editor = shell.clip_editor.read(cx);
                                        if editor.audio_mode != AudioMode::Merge || !editor.ready() {
                                            return Err("Audio-only dropdown selection did not enable audio export".into());
                                        }
                                        shell.load_media_at(other.clone(), 0., true, cx);
                                        if shell.clip_editor.read(cx).visible {
                                            return Err(
                                                "Changing media retained the old editing range"
                                                    .into(),
                                            );
                                        }
                                        return Ok(true);
                                    }
                                    _ => {}
                                }
                                Ok(false)
                            })
                        });
                        match update {
                            Ok(Ok(true)) => {
                                result = Ok(());
                                break;
                            }
                            Ok(Ok(false)) => {}
                            Ok(Err(error)) => {
                                result = Err(error);
                                break;
                            }
                            Err(error) => {
                                result = Err(error.to_string());
                                break;
                            }
                        }
                    }
                    *report.lock().unwrap() = Some(result);
                    let _ = window.update(cx, |_, _, cx| {
                        player.update(cx, |shell, cx| {
                            shell.clip_editor.update(cx, |editor, _| editor.shutdown());
                            shell.shutdown();
                        });
                        cx.quit();
                    });
                })
                .detach();
            });
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&source).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert!(std::fs::metadata(&source).unwrap().permissions().readonly());
        std::fs::set_permissions(&source, original_permissions).unwrap();
        outcome.lock().unwrap().take().unwrap().unwrap();
    }
}
