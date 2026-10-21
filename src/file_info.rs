//! A readable media summary, not a dump of decoder internals.
use crate::{clip_store::display_path, playback::Snapshot};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{component::ActiveTheme as _, *};
use serde_json::Value;
use std::path::Path;

type Rows = Vec<(String, String)>;

fn add_text(rows: &mut Rows, label: &str, value: Option<&Value>) {
    if let Some(text) = value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        rows.push((label.into(), text.to_owned()));
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && *number > 0.)
}

fn duration(seconds: f64) -> String {
    let seconds = seconds.max(0.) as u64;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn add_bitrate(rows: &mut Rows, value: Option<&Value>) {
    if let Some(bits) = number(value) {
        rows.push((
            "Bitrate".into(),
            if bits >= 1_000_000. {
                format!("{:.2} Mbps", bits / 1_000_000.)
            } else {
                format!("{:.0} kbps", bits / 1_000.)
            },
        ));
    }
}

fn media_groups(snapshot: &Snapshot) -> Vec<(String, Rows)> {
    let metadata = &snapshot.metadata;
    let tracks = metadata
        .get("track-list")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let video_track = tracks
        .iter()
        .find(|track| track["type"] == "video" && track["albumart"] != true);
    let params = metadata.get("video-params");
    let mut video = vec![];
    add_text(
        &mut video,
        "Codec",
        video_track
            .and_then(|track| track.get("codec-desc").or_else(|| track.get("codec")))
            .or_else(|| metadata.get("video-format")),
    );
    let width = number(params.and_then(|params| params.get("w")))
        .or_else(|| number(video_track.and_then(|track| track.get("demux-w"))));
    let height = number(params.and_then(|params| params.get("h")))
        .or_else(|| number(video_track.and_then(|track| track.get("demux-h"))));
    if let (Some(width), Some(height)) = (width, height) {
        video.push(("Resolution".into(), format!("{width:.0} × {height:.0}")));
    }
    if let Some(fps) = number(metadata.get("container-fps"))
        .or_else(|| number(video_track.and_then(|track| track.get("demux-fps"))))
    {
        video.push(("Frame rate".into(), format!("{fps:.2} fps")));
    }
    add_bitrate(
        &mut video,
        video_track
            .and_then(|track| track.get("demux-bitrate"))
            .or_else(|| metadata.get("video-bitrate")),
    );
    if let Some(gamma) = params
        .and_then(|params| params.get("gamma"))
        .and_then(Value::as_str)
    {
        match gamma {
            "pq" => video.push(("Dynamic range".into(), "HDR / PQ".into())),
            "hlg" => video.push(("Dynamic range".into(), "HDR / HLG".into())),
            _ => {}
        }
    }
    let mut groups = vec![];
    if !video.is_empty() {
        groups.push(("Video".into(), video));
    }
    let mut audio_index = 0;
    let mut subtitle_index = 0;
    for track in tracks {
        let kind = track.get("type").and_then(Value::as_str);
        let title = match kind {
            Some("audio") => {
                audio_index += 1;
                format!("Audio track {audio_index}")
            }
            Some("sub") => {
                subtitle_index += 1;
                format!("Subtitle track {subtitle_index}")
            }
            _ => continue,
        };
        let mut rows = vec![];
        add_text(&mut rows, "Title", track.get("title"));
        add_text(&mut rows, "Language", track.get("lang"));
        add_text(
            &mut rows,
            "Codec",
            track.get("codec-desc").or_else(|| track.get("codec")),
        );
        if kind == Some("audio") {
            add_text(&mut rows, "Channels", track.get("demux-channels"));
            if !rows.iter().any(|(label, _)| label == "Channels") {
                if let Some(channels) = number(track.get("demux-channel-count")) {
                    rows.push(("Channels".into(), format!("{channels:.0}")));
                }
            }
            if let Some(rate) = number(track.get("demux-samplerate")) {
                rows.push(("Sample rate".into(), format!("{:.1} kHz", rate / 1000.)));
            }
            add_bitrate(&mut rows, track.get("demux-bitrate"));
        }
        if !rows.is_empty() {
            groups.push((title, rows));
        }
    }
    let mut tags = vec![];
    if let Some(fields) = metadata.get("metadata").and_then(Value::as_object) {
        for (key, value) in fields {
            let label = match key.to_ascii_lowercase().as_str() {
                "title" => "Title",
                "artist" => "Artist",
                "description" => "Description",
                "comment" => "Comment",
                "copyright" => "Copyright",
                _ => continue,
            };
            add_text(&mut tags, label, Some(value));
        }
    }
    if !tags.is_empty() {
        groups.push(("Tags".into(), tags));
    }
    if let Some(chapters) = metadata.get("chapter-list").and_then(Value::as_array) {
        let rows: Rows = chapters
            .iter()
            .enumerate()
            .map(|(index, chapter)| {
                (
                    chapter
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|title| !title.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Chapter {}", index + 1)),
                    duration(chapter.get("time").and_then(Value::as_f64).unwrap_or(0.)),
                )
            })
            .collect();
        if !rows.is_empty() {
            groups.push(("Chapters".into(), rows));
        }
    }
    groups
}

fn group(title: &str, rows: Vec<(String, String)>, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .mb_4()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .flex()
                .gap_3()
                .text_xs()
                .child(
                    div()
                        .w(px(160.))
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(value))
        }))
}

pub fn panel(
    snapshot: &Snapshot,
    path: Option<&Path>,
    cx: &App,
    close: impl IntoElement,
) -> Stateful<Div> {
    let mut file = vec![];
    if let Some(path) = path {
        file.push((
            "Name".into(),
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ));
        file.push(("Path".into(), display_path(path)));
        if let Ok(metadata) = path.metadata() {
            file.push((
                "Size".into(),
                format!("{:.1} MiB", metadata.len() as f64 / 1_048_576.),
            ));
            for (label, time) in [
                ("Created", metadata.created()),
                ("Modified", metadata.modified()),
            ] {
                if let Ok(time) = time {
                    let time: chrono::DateTime<chrono::Local> = time.into();
                    file.push((label.into(), time.format("%Y-%m-%d %H:%M:%S").to_string()));
                }
            }
        }
    }
    if snapshot.duration > 0. {
        file.push(("Duration".into(), duration(snapshot.duration)));
    }
    add_text(&mut file, "Container", snapshot.metadata.get("file-format"));
    let mut groups = vec![("File".to_owned(), file)];
    groups.extend(media_groups(snapshot));
    div()
        .id("file-information")
        .absolute()
        .top(px(76.))
        .right_4()
        .w(px(560.))
        .max_w(relative(0.9))
        .max_h(relative(0.75))
        .overflow_y_scroll()
        .occlude()
        .p_4()
        .rounded_xl()
        .bg(cx.theme().popover.opacity(0.98))
        .border_1()
        .border_color(cx.theme().border)
        .shadow_lg()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .mb_4()
                .child(div().text_lg().child("File information"))
                .child(close),
        )
        .when_some(path.map(Path::to_owned), |panel, path| {
            panel.child(
                Button::new("reveal-media-file")
                    .ghost()
                    .label("Reveal in File Explorer")
                    .on_click(move |_, window, cx| {
                        crate::file_explorer::reveal(path.clone(), window, cx)
                    }),
            )
        })
        .children(
            groups
                .into_iter()
                .map(|(title, rows)| group(&title, rows, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::{duration, media_groups};
    use crate::playback::Snapshot;
    #[test]
    fn summary_keeps_useful_fields_without_decoder_internals_or_duplicates() {
        let mut snapshot = Snapshot::default();
        snapshot.metadata.insert(
            "video-params".into(),
            serde_json::json!({"w":1920,"h":1080,"rotate":0,"hw-pixelformat":"d3d11"}),
        );
        snapshot.metadata.insert("track-list".into(), serde_json::json!([
            {"type":"video","codec":"h264","id":1,"selected":true,"demux-fps":60},
            {"type":"audio","codec":"aac","id":2,"ff-index":7,"demux-channels":"stereo","demux-samplerate":48000}
        ]));
        snapshot.metadata.insert(
            "metadata".into(),
            serde_json::json!({"title":"Clip","encoder":"internal encoder"}),
        );
        snapshot
            .metadata
            .insert("mpv-version".into(), serde_json::json!("internal version"));
        let groups = media_groups(&snapshot);
        assert_eq!(
            groups
                .iter()
                .map(|(title, _)| title.as_str())
                .collect::<Vec<_>>(),
            ["Video", "Audio track 1", "Tags"]
        );
        assert!(
            groups[0]
                .1
                .contains(&("Resolution".into(), "1920 × 1080".into()))
        );
        assert!(
            groups[1]
                .1
                .contains(&("Sample rate".into(), "48.0 kHz".into()))
        );
        assert_eq!(groups[2].1, vec![("Title".into(), "Clip".into())]);
        assert!(
            !groups
                .iter()
                .flat_map(|(_, rows)| rows)
                .any(|(label, _)| ["id", "selected", "ff-index"].contains(&label.as_str()))
        );
    }

    #[test]
    fn unavailable_metadata_is_omitted_and_duration_is_readable() {
        assert!(media_groups(&Snapshot::default()).is_empty());
        assert_eq!(duration(3661.5), "1:01:01");
    }
}
