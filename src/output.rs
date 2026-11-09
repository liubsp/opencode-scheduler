//! Human presentation only; the authenticated JSON contract remains unchanged.
use crate::types::{ApiError, OperationResult};
use serde_json::Value;

fn text(value: &Value) -> String {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Null => "unknown".into(),
        _ => value.to_string(),
    };
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn schedule(s: &Value) -> String {
    let mut out = format!(
        "{}  {}  {}  {}  {}\n  Target title: {}\n  Attempts: {}/6; admission: {}; next attempt: {}\n",
        text(&s["id"]),
        text(&s["target"]["session_id"]),
        text(&s["time"]["display"]),
        text(&s["state"]),
        text(&s["delivery"]),
        text(&s["target"]["title"]),
        text(&s["attempts_used"]),
        text(&s["certainty"]),
        next_attempt(&s["next_attempt_at"])
    );
    if !s["last_error"].is_null() {
        out.push_str(&format!("  Error: {}\n", safe_error(&s["last_error"])));
    }
    out
}
fn safe_error(value: &Value) -> ApiError {
    serde_json::from_value::<ApiError>(value.clone())
        .ok()
        .and_then(|error| error.sanitized().ok())
        .unwrap_or_else(ApiError::storage)
}
fn next_attempt(value: &Value) -> String {
    if value.is_null() {
        "none".into()
    } else {
        text(value)
    }
}
fn cursor(data: &Value, out: &mut String) {
    if data["next_cursor"].is_string() {
        out.push_str(&format!(
            "Next page: --cursor {}\n",
            text(&data["next_cursor"])
        ));
    }
}
pub fn render(value: &Value) -> Result<String, ApiError> {
    let result: OperationResult =
        serde_json::from_value(value["result"].clone()).map_err(|_| ApiError::storage())?;
    let data = &value["result"]["data"];
    let mut out = String::new();
    match result {
        OperationResult::Created(_) => {
            out.push_str(&format!(
                "Created {} schedule(s); request {}\n",
                data["schedules"]
                    .as_array()
                    .ok_or_else(ApiError::storage)?
                    .len(),
                text(&data["request_id"])
            ));
            for s in data["schedules"].as_array().ok_or_else(ApiError::storage)? {
                out.push_str(&schedule(s));
            }
        }
        OperationResult::Schedule(_) => out.push_str(&schedule(data)),
        OperationResult::Page(_) => {
            let items = data["items"].as_array().ok_or_else(ApiError::storage)?;
            out.push_str(&format!("{} schedule(s)\n", items.len()));
            for s in items {
                out.push_str(&schedule(s));
            }
            cursor(data, &mut out);
        }
        OperationResult::Cancel(_) => {
            out.push_str(&format!("Cancellation: {}\n", text(&data["disposition"])));
            out.push_str(&schedule(&data["schedule"]));
        }
        OperationResult::Log(_) => {
            let items = data["items"].as_array().ok_or_else(ApiError::storage)?;
            out.push_str(&format!("{} delivery event(s)\n", items.len()));
            for e in items {
                out.push_str(&format!(
                    "{}  {}  attempt {}  {} / {}  admission: {}\n  Next attempt: {}\n",
                    text(&e["schedule_id"]),
                    text(&e["started_at"]),
                    text(&e["ordinal"]),
                    text(&e["phase"]),
                    text(&e["outcome"]),
                    text(&e["certainty"]),
                    next_attempt(&e["next_attempt_at"])
                ));
                if !e["error"].is_null() {
                    out.push_str(&format!("  Error: {}\n", safe_error(&e["error"])));
                }
            }
            cursor(data, &mut out);
        }
        OperationResult::Status(_) => {
            out.push_str(&format!("Binary: {}\nPlugin payload: {}\nDaemon: {}\nOpenCode: {} (version {})\nSchedules: {} pending, {} blocked, {} failed, {} admission unconfirmed\n",
                if data["installed"]["binary"] == true { "installed" } else { "not installed" },
                if data["installed"]["plugin"] == true { "installed" } else { "missing or invalid" },
                if data["daemon"]["reachable"] == true { "running" } else { "not running" },
                text(&data["opencode"]["state"]), text(&data["opencode"]["service_version"]),
                text(&data["schedules"]["pending"]), text(&data["schedules"]["blocked"]), text(&data["schedules"]["failed"]), text(&data["schedules"]["admission_unconfirmed"])));
            out.push_str(&format!(
                "Plugin host: {} (queried location)\n",
                text(&data["plugin_host"]["state"])
            ));
            if data["installed"]["plugin"] == true && data["plugin_host"]["state"] != "active" {
                out.push_str("Hint: check this location's plugin list and disable policy; wait for discovery, then recheck status.\n");
            }
        }
        OperationResult::Stopping(_) => {
            out.push_str("Scheduler daemon stopped. Schedules preserved.\n")
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn human_retry_errors_and_uncertainty_are_visible_without_exposing_prompt() {
        // Catches hiding exhausted retries, uncertain admission or the sanitized reason.
        let schedule = json!({
            "id":"schedule-fixture", "request_id":"request-fixture", "created_at":"2026-11-06T00:00:00Z",
            "time":{"due_at":"2026-11-07T01:30:00Z","original":"2026-11-07 02:30","display":"2026-11-07 02:30:00 +01:00","timezone":"Europe/Stockholm"},
            "target":{"session_id":"ses_fixture","title":"Fixture\n\u{1b}[31m title"}, "prompt":"private prompt",
            "delivery":"queue", "message_id":"msg_fixture", "state":"failed", "admitted_at":null,
            "attempts_used":6, "next_attempt_at":null, "certainty":"unconfirmed", "lateness_ms":42,
            "last_error":{"code":"opencode_unavailable","message":"OpenCode service is unavailable."}
        });
        let text =
            render(&json!({"ok":true,"result":{"kind":"schedule","data":schedule}})).unwrap();
        for expected in [
            "schedule-fixture",
            "ses_fixture",
            "Fixture  [31m title",
            "2026-11-07 02:30:00 +01:00",
            "failed",
            "6/6",
            "unconfirmed",
            "opencode_unavailable",
            "OpenCode service is unavailable.",
            "next attempt: none",
            "Hint:",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("private prompt"));
        assert!(!text.contains('\u{1b}'));
        let text = render(&json!({"ok":true,"result":{"kind":"log","data":{"items":[{
            "seq":1,"schedule_id":"schedule-fixture","ordinal":2,"started_at":"2026-11-07T01:30:10Z","finished_at":"2026-11-07T01:30:11Z",
            "phase":"submit","outcome":"transient","certainty":"unconfirmed",
            "error":{"code":"opencode_unavailable","message":"OpenCode service is unavailable."},"next_attempt_at":"2026-11-07T01:30:31Z"
        }],"next_cursor":"fixture-cursor"}}})).unwrap();
        for expected in [
            "schedule-fixture",
            "attempt 2",
            "transient",
            "unconfirmed",
            "2026-11-07T01:30:31Z",
            "opencode_unavailable",
            "--cursor fixture-cursor",
            "Hint:",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
