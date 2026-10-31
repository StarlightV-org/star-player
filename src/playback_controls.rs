//! Pending control values keep older worker snapshots from moving sliders back.
use super::{Command, Snapshot, finite_range};
use std::collections::BTreeMap;

pub(super) struct QueuedCommand {
    pub revision: u64,
    pub command: Command,
}

#[derive(Default)]
pub(super) struct AudioIntent {
    pub revision: u64,
    volume: Option<(u64, f64)>,
    muted: Option<(u64, bool)>,
    gains: BTreeMap<i64, (u64, f64)>,
    path: Option<std::path::PathBuf>,
}

impl AudioIntent {
    pub fn record(&mut self, command: &Command) {
        match command {
            Command::Load(path) | Command::LoadSaved(path, _) => {
                self.path = Some(path.clone());
                self.gains.clear();
            }
            Command::SetVolume(value) => self.volume = Some((self.revision, *value)),
            Command::SetMute(value) => self.muted = Some((self.revision, *value)),
            Command::SetTrackGain { id, gain } => {
                self.gains.insert(*id, (self.revision, *gain));
            }
            _ => {}
        }
    }

    pub fn acknowledge(&mut self, revision: u64) {
        if self.volume.is_some_and(|(r, _)| r <= revision) {
            self.volume = None;
        }
        if self.muted.is_some_and(|(r, _)| r <= revision) {
            self.muted = None;
        }
        self.gains.retain(|_, (r, _)| *r > revision);
    }

    pub fn overlay(&self, snapshot: &mut Snapshot) {
        if let Some((_, volume)) = self.volume {
            snapshot.volume = volume;
        }
        if let Some((_, muted)) = self.muted {
            snapshot.muted = muted;
        }
        if snapshot.path == self.path {
            for track in &mut snapshot.tracks {
                if let Some((_, gain)) = self.gains.get(&track.id) {
                    track.gain = *gain;
                }
            }
        }
    }
}

pub(super) fn validate_audio(command: &Command) -> Result<(), String> {
    match command {
        Command::SetVolume(value) => finite_range(*value, 0.0, 100.0, "Volume"),
        Command::SetTrackGain { gain, .. } => finite_range(*gain, 0.0, 10.0, "Track gain"),
        _ => Ok(()),
    }
}

// Only absolute audio settings commute. Never coalesce across load, seek, play,
// or other commands. Removing the older item preserves revision order for acks.
pub(super) fn coalesce(commands: Vec<QueuedCommand>) -> Vec<QueuedCommand> {
    let mut result: Vec<QueuedCommand> = Vec::with_capacity(commands.len());
    for next in commands {
        if is_audio_setting(&next.command) {
            if let Some(index) = result
                .iter()
                .enumerate()
                .rev()
                .take_while(|(_, previous)| is_audio_setting(&previous.command))
                .find_map(|(i, previous)| {
                    same_setting(&previous.command, &next.command).then_some(i)
                })
            {
                result.remove(index);
            }
        }
        result.push(next);
    }
    result
}

fn is_audio_setting(command: &Command) -> bool {
    matches!(
        command,
        Command::SetVolume(_) | Command::SetMute(_) | Command::SetTrackGain { .. }
    )
}

fn same_setting(first: &Command, second: &Command) -> bool {
    match (first, second) {
        (Command::SetVolume(_), Command::SetVolume(_))
        | (Command::SetMute(_), Command::SetMute(_)) => true,
        (Command::SetTrackGain { id: a, .. }, Command::SetTrackGain { id: b, .. }) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::AudioTrack;

    #[test]
    fn newer_intent_survives_old_worker_snapshots_and_acknowledgments() {
        let mut intent = AudioIntent::default();
        let path = std::path::PathBuf::from("clip.avi");
        intent.record(&Command::Load(path.clone()));
        intent.revision = 1;
        intent.record(&Command::SetTrackGain { id: 1, gain: 0.25 });
        intent.revision = 2;
        intent.record(&Command::SetTrackGain { id: 1, gain: 0.75 });
        intent.revision = 3;
        intent.record(&Command::SetVolume(37.0));
        let mut snapshot = Snapshot {
            path: Some(path),
            tracks: vec![AudioTrack {
                id: 1,
                title: "Audio".into(),
                gain: 0.25,
            }],
            ..Snapshot::default()
        };
        intent.acknowledge(1);
        intent.overlay(&mut snapshot);
        assert_eq!(snapshot.tracks[0].gain, 0.75);
        assert_eq!(snapshot.volume, 37.0);
        intent.acknowledge(2);
        snapshot.tracks[0].gain = 0.5;
        intent.overlay(&mut snapshot);
        assert_eq!(snapshot.tracks[0].gain, 0.5);
        assert_eq!(snapshot.volume, 37.0);
        intent.acknowledge(3);
        snapshot.volume = 100.0;
        intent.overlay(&mut snapshot);
        assert_eq!(snapshot.volume, 100.0);
    }

    #[test]
    fn media_changes_discard_old_track_intent() {
        let mut intent = AudioIntent::default();
        intent.record(&Command::Load("old.avi".into()));
        intent.record(&Command::SetTrackGain { id: 1, gain: 0.0 });
        intent.record(&Command::Load("new.avi".into()));
        let mut snapshot = Snapshot {
            path: Some("new.avi".into()),
            tracks: vec![AudioTrack {
                id: 1,
                title: "Audio".into(),
                gain: 1.0,
            }],
            ..Snapshot::default()
        };
        intent.overlay(&mut snapshot);
        assert_eq!(snapshot.tracks[0].gain, 1.0);
        intent.record(&Command::SetTrackGain { id: 1, gain: 0.4 });
        snapshot.path = Some("old.avi".into());
        intent.overlay(&mut snapshot);
        assert_eq!(snapshot.tracks[0].gain, 1.0);
    }

    #[test]
    fn coalescing_preserves_tracks_barriers_and_revision_order() {
        let commands = vec![
            Command::SetVolume(10.0),
            Command::SetTrackGain { id: 1, gain: 0.1 },
            Command::SetTrackGain { id: 2, gain: 0.2 },
            Command::SetVolume(20.0),
            Command::SetTrackGain { id: 1, gain: 0.3 },
            Command::SeekAbsolute(2.0),
            Command::SetVolume(30.0),
            Command::Load("next.avi".into()),
            Command::SetTrackGain { id: 1, gain: 0.4 },
        ];
        let result = coalesce(
            commands
                .into_iter()
                .enumerate()
                .map(|(i, command)| QueuedCommand {
                    revision: i as u64 + 1,
                    command,
                })
                .collect(),
        );
        assert_eq!(
            result.iter().map(|c| c.revision).collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7, 8, 9]
        );
        assert!(matches!(
            result[0].command,
            Command::SetTrackGain { id: 2, gain: 0.2 }
        ));
        assert!(matches!(result[1].command, Command::SetVolume(20.0)));
    }

    #[test]
    fn rejected_requests_do_not_replace_pending_values() {
        use crate::playback::{Mailbox, Playback};
        use std::sync::{Arc, mpsc};
        let (commands, receiver) = mpsc::sync_channel(1);
        let playback = Playback {
            commands,
            mailbox: Arc::new(Mailbox::new()),
            worker: None,
            native: false,
        };
        playback.send(Command::SetVolume(37.0)).unwrap();
        assert!(playback.send(Command::SetVolume(80.0)).is_err());
        assert!(playback.send(Command::SetVolume(f64::NAN)).is_err());
        assert_eq!(playback.snapshot().volume, 37.0);
        drop(receiver); // Let Playback's shutdown observe the disconnected queue.
    }
}
