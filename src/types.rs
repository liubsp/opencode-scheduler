use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::{Uuid, Version};

pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_PROMPT_BYTES: usize = 65_536;
pub const MAX_TITLE_BYTES: usize = 1_024;
pub const MAX_TIMESTAMP_BYTES: usize = 128;
pub const MAX_CURSOR_BYTES: usize = 512;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_ERROR_BYTES: usize = 512;
pub const MAX_RECIPIENTS: usize = 100;
pub const MAX_DUE_CANDIDATES: u16 = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid() -> Self {
        Self::new("invalid_input", "Invalid scheduler input.")
    }
    pub fn storage() -> Self {
        Self::new("storage_failure", "Scheduler storage operation failed.")
    }
    /// Persist only scheduler-owned wording, never an upstream response body or caller text.
    pub fn sanitized(&self) -> Result<Self, Self> {
        self.validate()?;
        let message = match self.code.as_str() {
            "invalid_input" => "Invalid scheduler input.",
            "missing_session" => "Target session does not exist.",
            "opencode_unavailable" => "OpenCode service is unavailable.",
            "unsupported_service" => "OpenCode service is unsupported.",
            "unsupported_protocol" => "Scheduler protocol is unsupported.",
            "storage_failure" => "Scheduler storage operation failed.",
            "authentication_failure" => "Service authentication failed.",
            "schedule_missing" => "Schedule does not exist.",
            "already_admitted" => "Prompt is already admitted.",
            "outcome_uncertain" => "Prompt admission is unconfirmed.",
            "request_conflict" => "Request ID has different original input.",
            "resource_limit" => "Scheduler resource limit exceeded.",
            _ => return Err(Self::invalid()),
        };
        Ok(Self::new(&self.code, message))
    }
    pub fn validate(&self) -> Result<(), Self> {
        const CODES: &[&str] = &[
            "invalid_input",
            "missing_session",
            "opencode_unavailable",
            "unsupported_service",
            "unsupported_protocol",
            "storage_failure",
            "authentication_failure",
            "schedule_missing",
            "already_admitted",
            "outcome_uncertain",
            "request_conflict",
            "resource_limit",
        ];
        if !CODES.contains(&self.code.as_str())
            || self.message.is_empty()
            || self.message.len() > MAX_ERROR_BYTES
        {
            return Err(Self::invalid());
        }
        Ok(())
    }
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

pub fn validate_uuid(id: &str) -> Result<(), ApiError> {
    let uuid = Uuid::parse_str(id).map_err(|_| ApiError::invalid())?;
    if id.len() != 36
        || uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != uuid::Variant::RFC4122
    {
        return Err(ApiError::invalid());
    }
    Ok(())
}
pub fn validate_session_id(id: &str) -> Result<(), ApiError> {
    if !(4..=MAX_ID_BYTES).contains(&id.len())
        || !id.starts_with("ses")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ApiError::invalid());
    }
    Ok(())
}
pub fn validate_message_id(id: &str) -> Result<(), ApiError> {
    let hex = id.strip_prefix("msg_").ok_or_else(ApiError::invalid)?;
    let uuid = Uuid::parse_str(hex).map_err(|_| ApiError::invalid())?;
    if hex.len() != 32
        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        || uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != uuid::Variant::RFC4122
    {
        return Err(ApiError::invalid());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    #[default]
    Steer,
    Queue,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    Pending,
    Dispatching,
    Admitted,
    Cancelled,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionCertainty {
    NotAdmitted,
    Unconfirmed,
    Admitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TargetSelector {
    Id(String),
    Title(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawCreateInput")]
pub struct CreateInput {
    pub request_id: String,
    pub target: TargetSelector,
    pub at: String,
    pub prompt: String,
    pub delivery: DeliveryMode,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCreateInput {
    request_id: String,
    target: TargetSelector,
    at: String,
    prompt: String,
    #[serde(default)]
    delivery: DeliveryMode,
}
impl TryFrom<RawCreateInput> for CreateInput {
    type Error = ApiError;
    fn try_from(v: RawCreateInput) -> Result<Self, ApiError> {
        let input = Self {
            request_id: v.request_id,
            target: v.target,
            at: v.at,
            prompt: v.prompt,
            delivery: v.delivery,
        };
        input.validate()?;
        Ok(input)
    }
}
impl CreateInput {
    pub fn validate(&self) -> Result<(), ApiError> {
        validate_uuid(&self.request_id)?;
        match &self.target {
            TargetSelector::Id(id) => validate_session_id(id)?,
            TargetSelector::Title(title) if title.is_empty() || title.len() > MAX_TITLE_BYTES => {
                return Err(ApiError::invalid());
            }
            _ => {}
        }
        if self.at.is_empty()
            || self.at.len() > MAX_TIMESTAMP_BYTES
            || self.prompt.trim().is_empty()
            || self.prompt.len() > MAX_PROMPT_BYTES
        {
            return Err(ApiError::invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTime {
    pub due_at: DateTime<Utc>,
    pub original: String,
    pub display: String,
    pub timezone: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub session_id: String,
    pub title: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRecipient {
    pub target: Target,
    pub message_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCreate {
    pub input: CreateInput,
    pub recipients: Vec<ResolvedRecipient>,
    pub time: ResolvedTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub id: String,
    pub request_id: String,
    pub created_at: DateTime<Utc>,
    pub time: ResolvedTime,
    pub target: Target,
    pub prompt: String,
    pub delivery: DeliveryMode,
    pub message_id: String,
    pub state: ScheduleState,
    pub admitted_at: Option<DateTime<Utc>>,
    pub attempts_used: u8,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub certainty: AdmissionCertainty,
    pub last_error: Option<ApiError>,
    pub lateness_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSummary {
    pub id: String,
    pub request_id: String,
    pub created_at: DateTime<Utc>,
    pub time: ResolvedTime,
    pub target: Target,
    pub delivery: DeliveryMode,
    pub message_id: String,
    pub state: ScheduleState,
    pub admitted_at: Option<DateTime<Utc>>,
    pub attempts_used: u8,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub certainty: AdmissionCertainty,
    pub last_error: Option<ApiError>,
    pub lateness_ms: Option<i64>,
}
impl From<&Schedule> for ScheduleSummary {
    fn from(s: &Schedule) -> Self {
        Self {
            id: s.id.clone(),
            request_id: s.request_id.clone(),
            created_at: s.created_at,
            time: s.time.clone(),
            target: s.target.clone(),
            delivery: s.delivery,
            message_id: s.message_id.clone(),
            state: s.state,
            admitted_at: s.admitted_at,
            attempts_used: s.attempts_used,
            next_attempt_at: s.next_attempt_at,
            certainty: s.certainty,
            last_error: s.last_error.clone(),
            lateness_ms: s.lateness_ms,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateResult {
    pub request_id: String,
    pub schedules: Vec<ScheduleSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIntent {
    pub schedule_id: String,
    pub ordinal: u8,
    pub session_id: String,
    pub message_id: String,
    pub prompt: String,
    pub delivery: DeliveryMode,
    pub started_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub previous_certainty: AdmissionCertainty,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Discovery,
    Reconcile,
    Submit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Admitted,
    Transient,
    Definitive,
    Interrupted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptResult {
    pub finished_at: DateTime<Utc>,
    pub phase: AttemptPhase,
    pub outcome: AttemptOutcome,
    pub certainty: AdmissionCertainty,
    pub error: Option<ApiError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventOutcome {
    Started,
    SubmitIntent,
    Admitted,
    Transient,
    Definitive,
    Interrupted,
    Cancelled,
    DefinitiveFailure,
    RetryExhausted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEvent {
    pub seq: i64,
    pub schedule_id: String,
    pub ordinal: Option<u8>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub phase: AttemptPhase,
    pub outcome: EventOutcome,
    pub certainty: AdmissionCertainty,
    pub error: Option<ApiError>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPageInput")]
pub struct PageInput {
    pub limit: u16,
    pub cursor: Option<String>,
}
impl Default for PageInput {
    fn default() -> Self {
        Self {
            limit: 50,
            cursor: None,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPageInput {
    #[serde(default = "default_page_limit")]
    limit: u16,
    #[serde(default)]
    cursor: Option<String>,
}
fn default_page_limit() -> u16 {
    50
}
impl TryFrom<RawPageInput> for PageInput {
    type Error = ApiError;
    fn try_from(v: RawPageInput) -> Result<Self, ApiError> {
        let page = Self {
            limit: v.limit,
            cursor: v.cursor,
        };
        page.validate()?;
        Ok(page)
    }
}
impl PageInput {
    pub fn validate(&self) -> Result<(), ApiError> {
        if !(1..=100).contains(&self.limit)
            || self
                .cursor
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > MAX_CURSOR_BYTES)
        {
            return Err(ApiError::invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelDisposition {
    Cancelled,
    AlreadyCancelled,
    AlreadyAdmitted,
    OutcomeUncertain,
    AlreadyFailed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelResult {
    pub schedule: Schedule,
    pub disposition: CancelDisposition,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleCounts {
    pub pending: u64,
    pub blocked: u64,
    pub failed: u64,
    pub admission_unconfirmed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdInput {
    pub id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogInput {
    pub id: String,
    #[serde(default = "default_page_limit")]
    pub limit: u16,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyInput {}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Operation {
    ScheduleCreate(CreateInput),
    ScheduleList(PageInput),
    ScheduleShow(IdInput),
    ScheduleCancel(IdInput),
    ScheduleLog(LogInput),
    Status(EmptyInput),
    DaemonStop(EmptyInput),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum OperationResult {
    Created(CreateResult),
    Schedule(Schedule),
    Page(Page<ScheduleSummary>),
    Log(Page<DeliveryEvent>),
    Cancel(CancelResult),
    Status(serde_json::Value),
    Stopping(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> serde_json::Value {
        serde_json::json!({
            "request_id": "c10ee051-9ad1-428e-8097-2bf4a62d7628",
            "target": {"kind":"id","value":"ses_test"},
            "at":"2026-11-06T02:00:00+01:00", "prompt":"hello"
        })
    }

    #[test]
    fn bounds_unknown_fields_and_prompt_privacy() {
        let base = input();
        let parsed: CreateInput = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(parsed.delivery, DeliveryMode::Steer);
        parsed.validate().unwrap();
        for (field, value) in [
            ("prompt", serde_json::json!(" ")),
            ("prompt", serde_json::json!("é".repeat(32_769))),
            ("at", serde_json::json!("x".repeat(129))),
            ("request_id", serde_json::json!("not-a-uuid")),
            ("delivery", serde_json::json!("other")),
            ("extra", serde_json::json!(true)),
        ] {
            let mut bad = base.clone();
            bad[field] = value;
            assert!(
                serde_json::from_value::<CreateInput>(bad).is_err(),
                "{field}"
            );
        }
        for id in ["ses", "abc_test", "ses_é", "ses space", &"s".repeat(129)] {
            let mut bad = base.clone();
            bad["target"]["value"] = serde_json::json!(id);
            assert!(serde_json::from_value::<CreateInput>(bad).is_err());
        }
        let mut nested = base.clone();
        nested["target"]["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CreateInput>(nested).is_err());
        let mut exact = base.clone();
        exact["prompt"] = serde_json::json!("é".repeat(32_768));
        assert!(serde_json::from_value::<CreateInput>(exact).is_ok());
        let mut title = base;
        title["target"] = serde_json::json!({"kind":"title","value":"é".repeat(512)});
        assert!(serde_json::from_value::<CreateInput>(title.clone()).is_ok());
        title["target"]["value"] = serde_json::json!("é".repeat(513));
        assert!(serde_json::from_value::<CreateInput>(title).is_err());
        for page in [r#"{"limit":0}"#, r#"{"limit":101}"#, r#"{"extra":1}"#] {
            assert!(serde_json::from_str::<PageInput>(page).is_err());
        }
        assert!(
            PageInput {
                limit: 50,
                cursor: Some("x".repeat(513))
            }
            .validate()
            .is_err()
        );
        assert!(validate_uuid("00000000-0000-1000-8000-000000000000").is_err());
        assert!(
            ApiError::new("storage_failure", "x".repeat(513))
                .validate()
                .is_err()
        );
    }
}
