use crate::types::{ApiError, MAX_TIMESTAMP_BYTES, ResolvedTime};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};

enum At {
    Offset(DateTime<FixedOffset>),
    Local(NaiveDateTime),
    Clock(NaiveTime),
}

fn parse_at(input: &str) -> Result<At, ApiError> {
    let invalid = || ApiError::coded("invalid_time");
    if input.is_empty() || input.len() > MAX_TIMESTAMP_BYTES {
        return Err(invalid());
    }
    if let Ok(offset) = DateTime::parse_from_rfc3339(input) {
        return Ok(At::Offset(offset));
    }
    let parts: Vec<_> = input.split(':').collect();
    if (parts.len() == 2 || parts.len() == 3)
        && (1..=2).contains(&parts[0].len())
        && parts
            .iter()
            .enumerate()
            .all(|(i, p)| (i == 0 || p.len() == 2) && p.bytes().all(|b| b.is_ascii_digit()))
    {
        let hour = parts[0].parse().map_err(|_| invalid())?;
        let minute = parts[1].parse().map_err(|_| invalid())?;
        let second = if parts.len() == 3 {
            parts[2].parse().map_err(|_| invalid())?
        } else {
            0
        };
        return NaiveTime::from_hms_opt(hour, minute, second)
            .map(At::Clock)
            .ok_or_else(invalid);
    }
    let format = match input.len() {
        16 => "%Y-%m-%d %H:%M",
        19 => "%Y-%m-%d %H:%M:%S",
        _ => return Err(invalid()),
    };
    let naive = NaiveDateTime::parse_from_str(input, format).map_err(|_| invalid())?;
    if naive.format(format).to_string() != input {
        return Err(invalid());
    }
    Ok(At::Local(naive))
}

/// Syntax only: do not resolve clocks or reject expired dates before idempotent replay lookup.
pub fn validate_syntax(input: &str) -> Result<(), ApiError> {
    parse_at(input).map(|_| ())
}

pub fn resolve_at<T: TimeZone>(
    input: &str,
    zone: &T,
    zone_name: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ResolvedTime, ApiError> {
    let parsed = parse_at(input)?;
    let resolved = if let At::Offset(offset) = parsed {
        ResolvedTime {
            due_at: offset.with_timezone(&Utc),
            original: input.into(),
            display: offset.to_rfc3339(),
            timezone: None,
        }
    } else {
        let name = zone_name
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::coded("timezone_unavailable"))?;
        let local = match parsed {
            At::Local(naive) => single_local(zone, &naive)?,
            At::Clock(clock) => {
                let local_now = now.with_timezone(zone);
                let today = local_now.date_naive();
                let naive = today.and_time(clock);
                let candidate = match zone.from_local_datetime(&naive) {
                    LocalResult::Single(value) if value.with_timezone(&Utc) > now => Some(value),
                    LocalResult::Single(_) => None,
                    // Only discard an overlap once BOTH interpretations have passed.
                    LocalResult::Ambiguous(a, b)
                        if a.with_timezone(&Utc) <= now && b.with_timezone(&Utc) <= now =>
                    {
                        None
                    }
                    // A gap has no instant; an already-passed local clock selects tomorrow.
                    LocalResult::None if naive <= local_now.naive_local() => None,
                    _ => return Err(ApiError::coded("local_time_invalid")),
                };
                if let Some(candidate) = candidate {
                    candidate
                } else {
                    let tomorrow = today
                        .succ_opt()
                        .ok_or_else(|| ApiError::coded("invalid_time"))?;
                    single_local(zone, &tomorrow.and_time(clock))?
                }
            }
            At::Offset(_) => unreachable!(),
        };
        ResolvedTime {
            due_at: local.with_timezone(&Utc),
            original: input.into(),
            display: format!(
                "{} {} {}",
                local.naive_local().format("%Y-%m-%d %H:%M:%S"),
                local.offset().fix(),
                name
            ),
            timezone: Some(name.into()),
        }
    };
    if resolved.due_at <= now {
        return Err(ApiError::coded("time_in_past"));
    }
    Ok(resolved)
}

fn single_local<T: TimeZone>(zone: &T, naive: &NaiveDateTime) -> Result<DateTime<T>, ApiError> {
    match zone.from_local_datetime(naive) {
        LocalResult::Single(value) => Ok(value),
        _ => Err(ApiError::coded("local_time_invalid")),
    }
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

    #[test]
    fn clock_is_next_strictly_future_local_occurrence() {
        for (now, clock, want) in [
            ("2026-01-01T03:00:00Z", "5:00", "2026-01-01T04:00:00Z"),
            ("2026-01-01T04:00:00Z", "05:00", "2026-01-02T04:00:00Z"),
            ("2026-01-01T05:00:00Z", "5:00:01", "2026-01-02T04:00:01Z"),
            ("2026-01-01T22:59:59Z", "00:00", "2026-01-01T23:00:00Z"),
            ("2026-01-01T23:00:00Z", "0:00:00", "2026-01-02T23:00:00Z"),
        ] {
            let value = resolve_at(
                clock,
                &Stockholm,
                Some("Europe/Stockholm"),
                now.parse().unwrap(),
            )
            .unwrap();
            assert_eq!(value.due_at, want.parse::<DateTime<Utc>>().unwrap());
            assert!(value.display.contains("+01:00 Europe/Stockholm"));
            assert!(value.display.starts_with("2026-01-0"));
        }
    }

    #[test]
    fn clock_dst_candidates_are_not_guessed_or_skipped() {
        for now in [
            "2026-03-28T23:00:00Z",
            "2026-10-24T22:00:00Z",
            "2026-03-28T03:00:00Z",
            "2026-10-24T03:00:00Z",
            // First overlap occurrence passed, but the second is still future.
            "2026-10-25T00:45:00Z",
        ] {
            let error = resolve_at(
                "2:30",
                &Stockholm,
                Some("Europe/Stockholm"),
                now.parse().unwrap(),
            )
            .unwrap_err()
            .sanitized()
            .unwrap();
            assert_eq!(error.code, "local_time_invalid");
            assert!(error.message.contains("offset"));
        }
    }

    #[test]
    fn passed_dst_transition_selects_valid_tomorrow_without_guessing() {
        for (now, due, display) in [
            (
                "2026-03-29T12:00:00Z",
                "2026-03-30T00:30:00Z",
                "2026-03-30 02:30:00 +02:00 Europe/Stockholm",
            ),
            (
                "2026-10-25T12:00:00Z",
                "2026-10-26T01:30:00Z",
                "2026-10-26 02:30:00 +01:00 Europe/Stockholm",
            ),
            (
                "2026-10-25T01:30:00Z",
                "2026-10-26T01:30:00Z",
                "2026-10-26 02:30:00 +01:00 Europe/Stockholm",
            ),
        ] {
            let resolved = resolve_at(
                "2:30",
                &Stockholm,
                Some("Europe/Stockholm"),
                now.parse().unwrap(),
            )
            .unwrap();
            assert_eq!(resolved.due_at, due.parse::<DateTime<Utc>>().unwrap());
            assert_eq!(resolved.display, display);
        }
    }

    #[test]
    fn time_errors_explain_accepted_formats_and_recovery() {
        for text in [
            "25:00",
            "5:0",
            "005:00",
            "5:00:0",
            "tomorrow",
            "2026-02-30 02:00",
        ] {
            let error = resolve_at(text, &Stockholm, Some("Europe/Stockholm"), now())
                .unwrap_err()
                .sanitized()
                .unwrap();
            assert_eq!(error.code, "invalid_time");
            assert!(error.message.contains("2099-11-06T05:00:00+01:00"));
            assert!(error.message.contains("5:00"));
        }
        let error = resolve_at(
            "2025-01-01 12:00",
            &Stockholm,
            Some("Europe/Stockholm"),
            now(),
        )
        .unwrap_err()
        .sanitized()
        .unwrap();
        assert_eq!(error.code, "time_in_past");
        assert!(error.message.contains("future"));
        let error = resolve_at("5:00", &Utc, None, now())
            .unwrap_err()
            .sanitized()
            .unwrap();
        assert_eq!(error.code, "timezone_unavailable");
        assert!(error.message.contains("offset"));
    }
}
