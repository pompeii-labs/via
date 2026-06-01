use anyhow::{bail, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime, UtcOffset};

const DISPLAY_TIME_FORMAT: &[FormatItem<'_>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]"
);

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn format_ts(ts: i64) -> String {
    let Ok(datetime) = OffsetDateTime::from_unix_timestamp(ts) else {
        return ts.to_string();
    };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    datetime
        .to_offset(offset)
        .format(DISPLAY_TIME_FORMAT)
        .unwrap_or_else(|_| ts.to_string())
}

pub fn normalize_slug(input: &str) -> Result<String> {
    let trimmed = input.trim().trim_end_matches(".local");
    let mut out = String::new();
    let mut last_dash = false;

    for ch in trimmed.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch == '.' || ch == ' ' || ch == '@' || ch == ':' {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = mapped {
            if ch == '-' {
                if !last_dash && !out.is_empty() {
                    out.push(ch);
                    last_dash = true;
                }
            } else {
                out.push(ch);
                last_dash = false;
            }
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        bail!("slug cannot be empty");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{format_ts, normalize_slug};

    #[test]
    fn normalizes_hostnames() {
        assert_eq!(
            normalize_slug("Mattys-MacBook-Pro.local").unwrap(),
            "mattys-macbook-pro"
        );
        assert_eq!(normalize_slug("mac mini.local").unwrap(), "mac-mini");
        assert_eq!(
            normalize_slug("root@203.0.113.10").unwrap(),
            "root-203-0-113-10"
        );
    }

    #[test]
    fn formats_unix_timestamps_for_display() {
        let formatted = format_ts(0);
        assert_ne!(formatted, "0");
        assert!(formatted.contains(':'));
    }
}
