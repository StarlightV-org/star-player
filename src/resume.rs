//! Resume eligibility and a countdown that excludes interaction time.
use std::time::{Duration, Instant};

pub fn position(
    policy: crate::clip_store::RememberPlayback,
    duration: f64,
    position: f64,
) -> Option<f64> {
    (policy.includes(duration) && position.is_finite() && position > 15.0 && position < duration)
        .then_some(position)
}

pub struct Prompt {
    pub position: f64,
    pub hovered: bool,
    remaining: Duration,
    updated: Instant,
}

impl Prompt {
    pub fn new(position: f64, now: Instant) -> Self {
        Self {
            position,
            hovered: false,
            remaining: Duration::from_secs(15),
            updated: now,
        }
    }

    pub fn expired(&mut self, now: Instant, focused: bool) -> bool {
        if !self.hovered && !focused {
            self.remaining = self
                .remaining
                .saturating_sub(now.saturating_duration_since(self.updated));
        }
        self.updated = now;
        self.remaining.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unfinished_long_videos_have_resume_positions() {
        use crate::clip_store::RememberPlayback;
        let policy = RememberPlayback::default();
        assert_eq!(position(policy, 600.0, 30.0), None);
        assert_eq!(position(policy, 600.1, 30.0), Some(30.0));
        for value in [0.1, 14.9, 15.0] {
            assert_eq!(position(policy, 700.0, value), None);
        }
        assert_eq!(position(policy, 700.0, 15.01), Some(15.01));
        for value in [0.0, -1.0, 700.0, 701.0, f64::NAN, f64::INFINITY] {
            assert_eq!(position(policy, 700.0, value), None);
        }
        assert_eq!(position(policy, f64::INFINITY, 30.0), None);
        assert_eq!(position(RememberPlayback::All, 60.0, 30.0), Some(30.0));
        for (policy, minutes) in RememberPlayback::OPTIONS
            .into_iter()
            .zip([0, 5, 10, 15, 30, 60])
        {
            let boundary = f64::from(minutes * 60);
            assert!(!policy.includes(boundary));
            assert!(policy.includes(boundary + 0.1));
            assert!(!policy.includes(f64::NAN));
        }
    }

    #[test]
    fn countdown_excludes_hover_and_keyboard_focus() {
        let now = Instant::now();
        let mut prompt = Prompt::new(42.0, now);
        assert!(!prompt.expired(now + Duration::from_secs(10), false));
        prompt.hovered = true;
        assert!(!prompt.expired(now + Duration::from_secs(40), false));
        prompt.hovered = false;
        assert!(!prompt.expired(now + Duration::from_secs(60), true));
        assert!(!prompt.expired(now + Duration::from_secs(64), false));
        assert!(prompt.expired(now + Duration::from_secs(65), false));
    }
}
