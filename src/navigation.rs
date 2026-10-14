use std::path::{Path, PathBuf};

pub struct History<T> {
    back: Vec<T>,
    forward: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
        }
    }
}

impl<T> History<T> {
    pub fn visit(&mut self, current: T) {
        self.back.push(current);
        self.forward.clear();
    }

    pub fn back(&mut self, current: T) -> Option<T> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    pub fn forward(&mut self, current: T) -> Option<T> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }
}

/// Only siblings belong to the queue for a file opened outside the library.
pub fn siblings(path: &Path) -> Vec<PathBuf> {
    let mut videos: Vec<_> = path
        .parent()
        .and_then(|parent| std::fs::read_dir(parent).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|entry| crate::clip_library::is_video(entry) || entry == path)
        .collect();
    if !videos.iter().any(|entry| entry == path) {
        videos.push(path.to_path_buf());
    }
    videos.sort_by_cached_key(|entry| {
        (
            entry
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
            entry.clone(),
        )
    });
    videos
}

pub fn adjacent(queue: &[PathBuf], path: &Path, next: bool) -> Option<PathBuf> {
    let index = queue.iter().position(|entry| entry == path)?;
    let index = if next {
        index.checked_add(1)?
    } else {
        index.checked_sub(1)?
    };
    queue.get(index).cloned()
}

pub fn near_edge(x: f32, y: f32, width: f32, height: f32) -> Option<bool> {
    if x < 0.0 || x > width || (y - height * 0.5).abs() > 140.0 {
        return None;
    }
    if x <= 236.0 {
        Some(false)
    } else if x >= width - 236.0 {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_revealed_only_near_the_middle_of_each_edge() {
        assert_eq!(near_edge(40.0, 350.0, 960.0, 700.0), Some(false));
        assert_eq!(near_edge(920.0, 350.0, 960.0, 700.0), Some(true));
        assert_eq!(near_edge(480.0, 350.0, 960.0, 700.0), None);
        assert_eq!(near_edge(40.0, 50.0, 960.0, 700.0), None);
        assert_eq!(near_edge(40.0, 680.0, 960.0, 700.0), None);
        assert_eq!(near_edge(-1.0, 350.0, 960.0, 700.0), None);
    }

    #[test]
    fn history_retraces_views_and_discards_forward_on_a_new_visit() {
        let mut history = History::default();
        assert_eq!(history.back("library"), None);
        assert_eq!(history.forward("library"), None);
        history.visit("library");
        history.visit("first");
        assert_eq!(history.back("second"), Some("first"));
        assert_eq!(history.back("first"), Some("library"));
        assert_eq!(history.forward("library"), Some("first"));
        assert_eq!(history.forward("first"), Some("second"));
        assert_eq!(history.back("second"), Some("first"));
        history.visit("first");
        assert_eq!(history.forward("third"), None);
        assert_eq!(history.back("third"), Some("first"));
    }

    #[test]
    fn standalone_queue_contains_only_sibling_videos_in_filename_order() {
        let folder = tempfile::tempdir().unwrap();
        for name in ["c.webm", "B.MP4", "a.mkv", "notes.txt"] {
            std::fs::write(folder.path().join(name), []).unwrap();
        }
        std::fs::create_dir(folder.path().join("folder.mp4")).unwrap();
        std::fs::write(folder.path().join("folder.mp4/nested.mp4"), []).unwrap();
        let current = folder.path().join("B.MP4");
        let queue = siblings(&current);
        assert_eq!(queue.len(), 3);
        assert_eq!(
            adjacent(&queue, &current, false),
            Some(folder.path().join("a.mkv"))
        );
        assert_eq!(
            adjacent(&queue, &current, true),
            Some(folder.path().join("c.webm"))
        );
        assert_eq!(adjacent(&queue, &queue[0], false), None);
        assert_eq!(adjacent(&queue, &queue[2], true), None);
    }

    #[test]
    fn single_file_and_unreadable_folder_have_no_neighbors() {
        let folder = tempfile::tempdir().unwrap();
        let current = folder.path().join("only.mp4");
        std::fs::write(&current, []).unwrap();
        let queue = siblings(&current);
        assert_eq!(queue, vec![current.clone()]);
        assert_eq!(adjacent(&queue, &current, false), None);
        assert_eq!(adjacent(&queue, &current, true), None);
        let missing = folder.path().join("missing/video.mp4");
        assert_eq!(siblings(&missing), vec![missing]);
    }
}
