//! The send-time policy (design doc §10.4).
//!
//! - Per-kind mute: skip.
//! - Older than 24 hours: expire, except `concern`.
//! - Quiet hours (default 21:00–08:00 local): wait until they end.
//! - Daily limit (default 1): skip, except `concern`.
//! - Calm mode: no amounts in the text.

use jiff::civil::{DateTime, Time};

/// The person's notification settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Quiet hours start (local).
    pub quiet_start: Time,
    /// Quiet hours end (local).
    pub quiet_end: Time,
    /// Notifications a day, besides concerns.
    pub daily_limit: u8,
    /// Insight kinds not to send.
    pub muted_kinds: Vec<String>,
    /// Calm mode: no amounts in notification text.
    pub calm_mode: bool,
}

/// What to do with one queued notification now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Send now.
    Send,
    /// Wait until this local time.
    Delay(DateTime),
    /// Do not send. The reason is stored.
    Skip(&'static str),
    /// Too old to send.
    Expire,
}

/// True if `time` is in quiet hours. Quiet hours can cross midnight.
pub fn in_quiet_hours(settings: &Settings, time: Time) -> bool {
    let (start, end) = (settings.quiet_start, settings.quiet_end);
    if start == end {
        return false;
    }
    if start < end {
        time >= start && time < end
    } else {
        time >= start || time < end
    }
}

/// Decides for one notification. `now` is local time, `age_hours` is the age
/// of the notification, `sent_today` counts notifications sent today
/// (local), concerns included.
pub fn decide(
    settings: &Settings,
    kind: &str,
    now: DateTime,
    age_hours: i64,
    sent_today: u32,
) -> Decision {
    let concern = kind == "concern";
    if settings.muted_kinds.iter().any(|k| k == kind) {
        return Decision::Skip("muted");
    }
    if age_hours >= 24 && !concern {
        return Decision::Expire;
    }
    if in_quiet_hours(settings, now.time()) {
        let end_today = now.date().to_datetime(settings.quiet_end);
        let until = if end_today > now {
            end_today
        } else {
            now.date()
                .tomorrow()
                .map_or(end_today, |d| d.to_datetime(settings.quiet_end))
        };
        return Decision::Delay(until);
    }
    if !concern && sent_today >= u32::from(settings.daily_limit) {
        return Decision::Skip("daily_limit");
    }
    Decision::Send
}

/// The notification text: one sentence (Appendix B.3). In calm mode, a
/// general sentence per kind with no amounts.
pub fn body(text: &str, kind: &str, calm_mode: bool) -> String {
    if calm_mode {
        return match kind {
            "concern" => "Something in SaveSpace needs your attention.",
            "anticipation" => "A bill is expected soon. Open SaveSpace to see it.",
            "awareness" => "There is a change in your spending. Open SaveSpace to see it.",
            _ => "There is something new in SaveSpace.",
        }
        .to_owned();
    }
    // The first sentence. A period inside an amount ($15.49) is not an end:
    // a sentence ends at ". " or at the end of the text.
    text.split_once(". ")
        .map_or_else(|| text.to_owned(), |(first, _)| format!("{first}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    fn settings() -> Settings {
        Settings {
            quiet_start: time(21, 0, 0, 0),
            quiet_end: time(8, 0, 0, 0),
            daily_limit: 1,
            muted_kinds: vec!["awareness".into()],
            calm_mode: false,
        }
    }

    fn at(h: i8, m: i8) -> DateTime {
        date(2026, 11, 16).at(h, m, 0, 0)
    }

    #[test]
    fn quiet_hours_cross_midnight() {
        let s = settings();
        assert!(in_quiet_hours(&s, time(22, 0, 0, 0)));
        assert!(in_quiet_hours(&s, time(7, 59, 0, 0)));
        assert!(!in_quiet_hours(&s, time(8, 0, 0, 0)));
        assert!(!in_quiet_hours(&s, time(20, 59, 0, 0)));
        let day = Settings {
            quiet_start: time(12, 0, 0, 0),
            quiet_end: time(13, 0, 0, 0),
            ..s
        };
        assert!(in_quiet_hours(&day, time(12, 30, 0, 0)));
        assert!(!in_quiet_hours(&day, time(13, 30, 0, 0)));
    }

    #[test]
    fn decides_by_policy() {
        let s = settings();
        assert_eq!(decide(&s, "anticipation", at(10, 0), 0, 0), Decision::Send);
        assert_eq!(
            decide(&s, "anticipation", at(10, 0), 0, 1),
            Decision::Skip("daily_limit")
        );
        assert_eq!(
            decide(&s, "concern", at(10, 0), 0, 5),
            Decision::Send,
            "concerns always send"
        );
        assert_eq!(
            decide(&s, "awareness", at(10, 0), 0, 0),
            Decision::Skip("muted")
        );
        assert_eq!(
            decide(&s, "anticipation", at(10, 0), 30, 0),
            Decision::Expire
        );
        assert_eq!(
            decide(&s, "concern", at(10, 0), 30, 0),
            Decision::Send,
            "concerns do not expire"
        );
        // Quiet hours: late evening waits until 08:00 tomorrow; early
        // morning until 08:00 today. Concerns wait too.
        assert_eq!(
            decide(&s, "concern", at(22, 30), 0, 0),
            Decision::Delay(date(2026, 11, 17).at(8, 0, 0, 0))
        );
        assert_eq!(
            decide(&s, "anticipation", at(6, 0), 0, 0),
            Decision::Delay(at(8, 0))
        );
    }

    #[test]
    fn one_sentence_and_no_amounts_in_calm_mode() {
        assert_eq!(
            body(
                "Oak Apartments ($1,800) is expected on Dec 1. Your cash covers it.",
                "anticipation",
                false
            ),
            "Oak Apartments ($1,800) is expected on Dec 1."
        );
        assert_eq!(
            body(
                "This Netflix charge is $7.50 higher than usual.",
                "awareness",
                false
            ),
            "This Netflix charge is $7.50 higher than usual."
        );
        let calm = body(
            "Your cash may not cover Rent ($1,800) on Dec 1.",
            "concern",
            true,
        );
        assert!(!calm.contains('$') && !calm.chars().any(|c| c.is_ascii_digit()));
    }
}
