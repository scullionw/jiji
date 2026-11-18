//! Ages and timestamps as the interface words them.

/// Compact age for dense graph rows: "now", "5m", "3h", "2d", "5w", "8mo",
/// "2y".
pub fn short_age(timestamp: &str) -> String {
    short_age_at(timestamp, chrono::Utc::now())
}

pub fn short_age_at(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return String::new();
    };
    let minutes = (now - at.with_timezone(&chrono::Utc)).num_minutes();
    if minutes < 1 {
        return "now".into();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        format!("{days}d")
    } else if days < 60 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// "a few seconds ago", "5 minutes ago", "3 hours ago", "2 days ago" — a
/// sentence-shaped age for an RFC 3339 timestamp.
pub fn from_now(timestamp: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return String::new();
    };
    let seconds = (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    let unit = |n: i64, one: &str, many: &str| {
        if n <= 1 {
            format!("{one} ago")
        } else {
            format!("{n} {many} ago")
        }
    };
    match seconds {
        0..=44 => "a few seconds ago".into(),
        45..=89 => "a minute ago".into(),
        90..=2699 => format!("{} minutes ago", (seconds + 30) / 60),
        2700..=5399 => "an hour ago".into(),
        5400..=79199 => format!("{} hours ago", (seconds + 1800) / 3600),
        79200..=129599 => "a day ago".into(),
        129600..=2246399 => format!("{} days ago", (seconds + 43200) / 86400),
        2246400..=3887999 => "a month ago".into(),
        3888000..=29807999 => unit(seconds / 2_592_000, "a month", "months"),
        _ => unit(seconds / 31_536_000, "a year", "years"),
    }
}

/// A full local date and time, for tooltips.
pub fn absolute_time(timestamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d, %Y at %-I:%M %p")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_step_through_units() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(short_age_at("2026-06-10T11:15:00Z", now), "45m");
        assert_eq!(short_age_at("2026-06-08T12:00:00Z", now), "2d");
        assert_eq!(short_age_at("2026-05-01T12:00:00Z", now), "5w");
        assert_eq!(short_age_at("2025-10-10T12:00:00Z", now), "8mo");
        assert_eq!(short_age_at("2024-06-10T12:00:00Z", now), "2y");
    }
}
