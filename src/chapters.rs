//! Per-file annotations and pure chapter parsing / skip decisions.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub id: u64,
    pub time: f64,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChapterOptions {
    pub disabled: bool,
    pub auto_skip: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSettings {
    pub chapters: Vec<Point>,
    pub markers: Vec<Point>,
    pub embedded_disabled: bool,
    pub options: BTreeMap<String, ChapterOptions>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub key: String,
    pub time: f64,
    pub end: f64,
    pub title: String,
    pub custom_id: Option<u64>,
    pub disabled: bool,
    pub auto_skip: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Import {
    pub points: Vec<Point>,
    pub issues: Vec<String>,
}

pub fn parse_time(value: &str) -> Option<f64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let numbers = parts
        .iter()
        .map(|p| p.parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if numbers[1..].iter().any(|n| *n >= 60) {
        return None;
    }
    Some(
        numbers
            .iter()
            .fold(0.0, |time, n| time * 60.0 + f64::from(*n)),
    )
}

pub fn parse_line(line: &str) -> Option<(f64, String)> {
    let line = line.trim();
    let first = line.split_whitespace().next()?;
    if let Some(time) = parse_time(first) {
        let title = line[first.len()..]
            .trim()
            .trim_start_matches(['-', '|', '/'])
            .trim();
        return (!title.is_empty()).then(|| (time, title.to_owned()));
    }
    let last = line.split_whitespace().last()?;
    let time = parse_time(last)?;
    let title = line[..line.len() - last.len()]
        .trim()
        .trim_end_matches(['-', '|', '/'])
        .trim();
    (!title.is_empty()).then(|| (time, title.to_owned()))
}

pub fn import(text: &str, duration: f64, existing: &[Point]) -> Import {
    let mut result = Import::default();
    let mut next = existing
        .iter()
        .map(|p| p.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    for (index, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let Some((time, title)) = parse_line(line) else {
            result.issues.push(format!(
                "Line {}: expected a timestamp and title",
                index + 1
            ));
            continue;
        };
        if duration > 0.0 && time >= duration {
            result
                .issues
                .push(format!("Line {}: timestamp is beyond the video", index + 1));
            continue;
        }
        let all = || existing.iter().chain(result.points.iter());
        if all().any(|p| p.time == time && p.title == title) {
            continue;
        }
        if all().any(|p| p.time == time) {
            result.issues.push(format!(
                "Line {}: another chapter starts at this time; review before saving",
                index + 1
            ));
        }
        result.points.push(Point {
            id: next,
            time,
            title,
        });
        next = next.saturating_add(1);
    }
    result.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    result
}

impl FileSettings {
    pub fn validate(&self) -> Result<(), String> {
        for points in [&self.chapters, &self.markers] {
            let mut ids = HashSet::new();
            if points.iter().any(|p| {
                p.id == 0
                    || !ids.insert(p.id)
                    || !p.time.is_finite()
                    || p.time < 0.0
                    || p.title.trim().is_empty()
            }) {
                return Err("Invalid chapters or markers; media history left unchanged".into());
            }
        }
        Ok(())
    }

    pub fn chapters(
        &self,
        metadata: &BTreeMap<String, serde_json::Value>,
        duration: f64,
    ) -> Vec<Chapter> {
        let embedded = metadata.get("chapter-list").and_then(|v| v.as_array());
        let mut sources = vec![
            self.chapters
                .iter()
                .map(|p| {
                    (
                        format!("custom:{}", p.id),
                        p.time,
                        p.title.clone(),
                        Some(p.id),
                    )
                })
                .collect::<Vec<_>>(),
        ];
        sources.push(
            embedded
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, value)| {
                    let time = value.get("time")?.as_f64()?;
                    Some((
                        format!("embedded:{index}:{time}"),
                        time,
                        value
                            .get("title")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("Chapter {}", index + 1)),
                        None,
                    ))
                })
                .collect(),
        );
        let mut result = Vec::new();
        for mut source in sources {
            source.retain(|(_, t, _, _)| {
                t.is_finite() && *t >= 0.0 && (duration <= 0.0 || *t < duration)
            });
            source.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (key, time, title, custom_id) in &source {
                let end = source
                    .iter()
                    .find(|(_, t, _, _)| t > time)
                    .map(|p| p.1)
                    .unwrap_or(duration);
                let options = self.options.get(key).cloned().unwrap_or_default();
                result.push(Chapter {
                    key: key.clone(),
                    time: *time,
                    end,
                    title: title.clone(),
                    custom_id: *custom_id,
                    disabled: options.disabled || (custom_id.is_none() && self.embedded_disabled),
                    auto_skip: options.auto_skip,
                });
            }
        }
        result.sort_by(|a, b| a.time.total_cmp(&b.time).then_with(|| a.key.cmp(&b.key)));
        result
    }
}

/// Follow overlapping and consecutive skip ranges in one seek, never backwards.
pub fn skip_target(chapters: &[Chapter], position: f64, duration: f64) -> Option<f64> {
    if !position.is_finite() || !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    let mut target = position;
    loop {
        let next = chapters
            .iter()
            .filter(|c| !c.disabled && c.auto_skip && c.time <= target && target < c.end)
            .map(|c| c.end.min(duration))
            .fold(target, f64::max);
        if next <= target {
            break;
        }
        target = next;
    }
    (target > position).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_the_complete_concert_sample() {
        let sample = "0:50 Nightmare (Milan, Italy)
5:00 I'm Not A Woman, I'm A God (Milan, Italy)
8:54 Castle (Rock For People)
15:12 Easier Than Lying (Milan, Italy)
19:30 Dog Years (Milan, Italy)
23:37 You Asked For This (Milan, Italy)
28:16 Experiment On Me (Rock For People)
32:54 Closer (Rockwerchter)
38:17 Honey (Milan, Italy)
41:23 Lighthouse (Milan, Italy)
46:10 Colors (Rock For People)
51:47 Without Me (Rock For People)
56:23 Hurricane (Milan, Italy)
1:01:05 Lie (Rock For People)
1:04:06 Gasoline (Milan, Italy)
1:09:03 Lonely Is The Muse (Rock For People)";
        let result = import(sample, 4500.0, &[]);
        assert_eq!(result.points.len(), 16);
        assert!(result.issues.is_empty());
        assert_eq!(result.points.last().unwrap().time, 4143.0);
        let reversed = result
            .points
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    p.title,
                    crate::components::seeker_bar::timestamp(p.time)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(import(&reversed, 4500.0, &[]).points, result.points);
    }
    #[test]
    fn detects_both_orders_and_preserves_titles() {
        let result = import(
            "0:50 Nightmare (Milan, Italy)\nLie (Rock For People) 1:01:05",
            5000.0,
            &[],
        );
        assert!(result.issues.is_empty());
        assert_eq!(result.points[0].time, 50.0);
        assert_eq!(result.points[0].title, "Nightmare (Milan, Italy)");
        assert_eq!(result.points[1].time, 3665.0);
        assert!(parse_time("1:60").is_none());
        assert_eq!(
            parse_line("  0:50 / Song title  "),
            Some((50.0, "Song title".into()))
        );
        assert_eq!(
            parse_line("Song title / 0:50"),
            Some((50.0, "Song title".into()))
        );
        assert!(parse_time("1:60:00").is_none());
        assert!(parse_line("0:50").is_none());
    }
    #[test]
    fn imports_append_deduplicate_and_flag_conflicts() {
        let first = import("0:50 One", 100.0, &[]);
        let next = import(
            "0:50 One\nOther 0:50\n2:00 Too late\nbad",
            100.0,
            &first.points,
        );
        assert_eq!(next.points.len(), 1);
        assert_eq!(next.issues.len(), 3);
        assert_eq!(next.points[0].id, 2);
    }
    #[test]
    fn ranges_stay_with_their_source_and_skip_consecutive_chapters() {
        let mut settings = FileSettings::default();
        settings.chapters = import("0:10 A\n0:20 B\n0:30 C", 60.0, &[]).points;
        for id in [1, 2] {
            settings.options.insert(
                format!("custom:{id}"),
                ChapterOptions {
                    auto_skip: true,
                    disabled: false,
                },
            );
        }
        let metadata = BTreeMap::from([(
            "chapter-list".into(),
            serde_json::json!([{"time":15,"title":"Embedded"}]),
        )]);
        let chapters = settings.chapters(&metadata, 60.0);
        assert_eq!(chapters[0].end, 20.0);
        assert_eq!(skip_target(&chapters, 12.0, 60.0), Some(30.0));
        settings.options.get_mut("custom:1").unwrap().disabled = true;
        assert_eq!(
            skip_target(&settings.chapters(&metadata, 60.0), 12.0, 60.0),
            None
        );
    }
    #[test]
    fn embedded_toggle_preserves_custom_and_skip_options() {
        let mut settings = FileSettings::default();
        settings.chapters = import("0:20 Custom", 60.0, &[]).points;
        let metadata = BTreeMap::from([(
            "chapter-list".into(),
            serde_json::json!([{"time":10,"title":"Intro"},{"time":30,"title":"End"}]),
        )]);
        let embedded_key = settings.chapters(&metadata, 60.0)[0].key.clone();
        settings.options.insert(
            embedded_key,
            ChapterOptions {
                disabled: false,
                auto_skip: true,
            },
        );
        assert_eq!(
            skip_target(&settings.chapters(&metadata, 60.0), 15.0, 60.0),
            Some(30.0)
        );
        settings.embedded_disabled = true;
        let chapters = settings.chapters(&metadata, 60.0);
        assert_eq!(skip_target(&chapters, 15.0, 60.0), None);
        assert!(
            chapters
                .iter()
                .find(|c| c.custom_id.is_some())
                .is_some_and(|c| !c.disabled)
        );
        assert!(chapters[0].auto_skip);
    }
    #[test]
    fn final_chapter_skips_to_end_and_invalid_points_are_rejected() {
        let mut settings = FileSettings::default();
        settings.chapters = import("0:10 End", 60.0, &[]).points;
        settings.options.insert(
            "custom:1".into(),
            ChapterOptions {
                disabled: false,
                auto_skip: true,
            },
        );
        let chapters = settings.chapters(&BTreeMap::new(), 60.0);
        assert_eq!(skip_target(&chapters, 12.0, 60.0), Some(60.0));
        assert_eq!(skip_target(&chapters, 60.0, 60.0), None);
        assert_eq!(skip_target(&chapters, f64::NAN, 60.0), None);
        settings.chapters[0].time = f64::NAN;
        assert!(settings.validate().is_err());
    }
}
