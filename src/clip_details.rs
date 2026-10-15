//! Library timestamps use the user's Windows regional settings.
use std::time::SystemTime;

pub fn date(time: SystemTime, now: SystemTime) -> String {
    format!("{} ({})", local_date(time), relative_age(time, now))
}

fn local_date(time: SystemTime) -> String {
    let local = chrono::DateTime::<chrono::Local>::from(time);
    #[cfg(windows)]
    {
        use chrono::{Datelike, Timelike};
        use std::ptr;
        use windows_sys::Win32::{Foundation::SYSTEMTIME, Globalization::*};
        let year = local.year();
        if (1601..=u16::MAX as i32).contains(&year) {
            let time = SYSTEMTIME {
                wYear: year as u16,
                wMonth: local.month() as u16,
                wDayOfWeek: local.weekday().num_days_from_sunday() as u16,
                wDay: local.day() as u16,
                wHour: local.hour() as u16,
                wMinute: local.minute() as u16,
                wSecond: local.second() as u16,
                wMilliseconds: 0,
            };
            // A null locale and format use the current user's preferences,
            // including their overrides for date order and 12/24-hour time.
            let date = wide_string(|buffer, length| unsafe {
                GetDateFormatEx(
                    ptr::null(),
                    DATE_SHORTDATE,
                    &time,
                    ptr::null(),
                    buffer,
                    length,
                    ptr::null(),
                )
            });
            let clock = wide_string(|buffer, length| unsafe {
                GetTimeFormatEx(ptr::null(), 0, &time, ptr::null(), buffer, length)
            });
            if let (Some(date), Some(clock)) = (date, clock) {
                return format!("{date} {clock}");
            }
        }
    }
    local.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(windows)]
fn wide_string(format: impl Fn(*mut u16, i32) -> i32) -> Option<String> {
    let length = format(std::ptr::null_mut(), 0);
    if length <= 1 {
        return None;
    }
    let mut buffer = vec![0; length as usize];
    let written = format(buffer.as_mut_ptr(), length);
    (written > 1).then(|| String::from_utf16_lossy(&buffer[..written as usize - 1]))
}

pub fn relative_age(time: SystemTime, now: SystemTime) -> String {
    let (seconds, future) = match now.duration_since(time) {
        Ok(age) => (age.as_secs(), false),
        Err(age) => (age.duration().as_secs(), true),
    };
    let (amount, unit) = match seconds {
        0..120 => (seconds, "s"),
        120..3600 => (seconds / 60, "min"),
        3600..86400 => (seconds / 3600, "h"),
        86400..604800 => (seconds / 86400, "d"),
        604800..31536000 => (seconds / 604800, "w"),
        _ => (seconds / 31536000, "y"),
    };
    if future {
        format!("in {amount}{unit}")
    } else {
        format!("{amount}{unit} ago")
    }
}

pub fn duration(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds.filter(|s| s.is_finite() && *s > 0.0) else {
        return "--:--".into();
    };
    let seconds = seconds.round() as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn relative_dates_handle_seconds_minutes_and_future_times() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for (seconds, expected) in [
            (0, "0s ago"),
            (60, "60s ago"),
            (120, "2min ago"),
            (3600, "1h ago"),
            (86400, "1d ago"),
            (604800, "1w ago"),
            (31536000, "1y ago"),
        ] {
            assert_eq!(
                relative_age(time, time + Duration::from_secs(seconds)),
                expected
            );
        }
        assert_eq!(relative_age(time + Duration::from_secs(60), time), "in 60s");
        assert!(date(time, time + Duration::from_secs(60)).ends_with(" (60s ago)"));
    }

    #[test]
    fn duration_formats_short_and_long_clips_and_unknown_values() {
        assert_eq!(duration(Some(60.0)), "1:00");
        assert_eq!(duration(Some(3661.0)), "1:01:01");
        assert_eq!(duration(Some(59.9)), "1:00");
        for seconds in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert_eq!(duration(seconds), "--:--");
        }
    }
}
