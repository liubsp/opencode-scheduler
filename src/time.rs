use crate::types::{ApiError, MAX_TIMESTAMP_BYTES, ResolvedTime};
use chrono::{DateTime, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};

pub fn resolve_at<T: TimeZone>(
    input: &str,
    zone: &T,
    zone_name: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ResolvedTime, ApiError> {
    if input.is_empty() || input.len() > MAX_TIMESTAMP_BYTES {
        return Err(ApiError::invalid());
    }
    let resolved = if let Ok(offset) = DateTime::parse_from_rfc3339(input) {
        ResolvedTime {
            due_at: offset.with_timezone(&Utc),
            original: input.into(),
            display: offset.to_rfc3339(),
            timezone: None,
        }
    } else {
        let name = zone_name
            .filter(|s| !s.is_empty())
            .ok_or_else(ApiError::invalid)?;
        let format = match input.len() {
            16 => "%Y-%m-%d %H:%M",
            19 => "%Y-%m-%d %H:%M:%S",
            _ => return Err(ApiError::invalid()),
        };
        let naive =
            NaiveDateTime::parse_from_str(input, format).map_err(|_| ApiError::invalid())?;
        // Chrono's parser accepts variable-width fields; the public local grammar does not.
        if naive.format(format).to_string() != input {
            return Err(ApiError::invalid());
        }
        let local = match zone.from_local_datetime(&naive) {
            LocalResult::Single(value) => value,
            _ => return Err(ApiError::invalid()),
        };
        ResolvedTime {
            due_at: local.with_timezone(&Utc),
            original: input.into(),
            display: format!(
                "{} {} {}",
                naive.format("%Y-%m-%d %H:%M:%S"),
                local.offset().fix(),
                name
            ),
            timezone: Some(name.into()),
        }
    };
    if resolved.due_at <= now {
        return Err(ApiError::invalid());
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use chrono_tz::Europe::Stockholm;

    fn now() -> DateTime<Utc> {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn offset_and_local_echo_full_instant() {
        let offset = resolve_at(
            "2026-11-06T02:00:00+01:00",
            &Stockholm,
            Some("Europe/Stockholm"),
            now(),
        )
        .unwrap();
        assert_eq!(offset.due_at.to_rfc3339(), "2026-11-06T01:00:00+00:00");
        for text in ["2026-11-06 02:00", "2026-11-06 02:00:00"] {
            let local = resolve_at(text, &Stockholm, Some("Europe/Stockholm"), now()).unwrap();
            assert_eq!(local.due_at, offset.due_at);
            assert_eq!(local.original, text);
            assert_eq!(local.display, "2026-11-06 02:00:00 +01:00 Europe/Stockholm");
            assert_eq!(local.timezone.as_deref(), Some("Europe/Stockholm"));
        }
    }

    #[test]
    fn rejects_gap_overlap_and_unsupported_inputs() {
        for text in [
            "2026-03-29 02:30",
            "2026-10-25 02:30",
            "2026-02-30 02:00",
            "02:00",
            "tomorrow",
            "in 2h",
            "2026-11-06T02:00:00",
            "2026-01-01T00:00:00Z",
            "2025-01-01 12:00",
        ] {
            assert!(
                resolve_at(text, &Stockholm, Some("Europe/Stockholm"), now()).is_err(),
                "{text}"
            );
        }
        assert!(resolve_at("2026-10-25T02:30:00+01:00", &Stockholm, None, now()).is_ok());
        assert!(resolve_at("2026-11-06 02:00", &Utc, None, now()).is_err());
    }
}
