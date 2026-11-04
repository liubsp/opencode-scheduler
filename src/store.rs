use crate::scheduler::retry_delay;
use crate::types::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashSet, path::Path, time::Duration};
use uuid::Uuid;

pub struct Store {
    conn: Connection,
    // A reopened connection can reconcile persisted work but cannot acquire its POST permission.
    owned_attempts: HashSet<(String, u8)>,
}

fn db<T>(result: rusqlite::Result<T>) -> Result<T, ApiError> {
    result.map_err(|_| ApiError::storage())
}
fn json<T: Serialize>(value: &T) -> Result<String, ApiError> {
    serde_json::to_string(value).map_err(|_| ApiError::storage())
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, ApiError> {
    serde_json::from_str(value).map_err(|_| ApiError::storage())
}
fn conflict() -> ApiError {
    ApiError::new(
        "request_conflict",
        "Request ID has different original input.",
    )
}
fn transition_error() -> ApiError {
    ApiError::new(
        "outcome_uncertain",
        "Attempt is no longer eligible for this operation.",
    )
}

const SCHEMA: &str = "
CREATE TABLE create_requests (
    request_id TEXT PRIMARY KEY, input_json TEXT NOT NULL, result_json TEXT NOT NULL
);
CREATE TABLE schedules (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
    request_id TEXT NOT NULL REFERENCES create_requests(request_id), session_id TEXT NOT NULL,
    message_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL,
    next_attempt_at INTEGER, due_at INTEGER NOT NULL, certainty TEXT NOT NULL,
    blocked INTEGER NOT NULL, data_json TEXT NOT NULL,
    UNIQUE(request_id,session_id)
);
CREATE INDEX schedules_due ON schedules(state,next_attempt_at,due_at);
CREATE TABLE attempts (
    schedule_id TEXT NOT NULL REFERENCES schedules(id), ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 6),
    intent_json TEXT NOT NULL, submit_intent INTEGER NOT NULL DEFAULT 0,
    interrupted INTEGER NOT NULL DEFAULT 0, finished INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(schedule_id,ordinal)
);
CREATE INDEX unfinished_attempts ON attempts(schedule_id,ordinal) WHERE finished=0;
CREATE TABLE delivery_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, schedule_id TEXT NOT NULL REFERENCES schedules(id), data_json TEXT NOT NULL
);
CREATE INDEX delivery_log_schedule ON delivery_log(schedule_id,seq);
PRAGMA user_version=1;
";

const DUE_QUERY: &str = "SELECT data_json FROM schedules
    WHERE state='pending' AND next_attempt_at <= ?1 AND due_at <= ?1
    ORDER BY next_attempt_at,due_at,seq LIMIT 32";
const UNFINISHED_QUERY: &str = "SELECT intent_json,interrupted FROM attempts
    WHERE finished=0 ORDER BY schedule_id,ordinal LIMIT ?1";

impl Store {
    pub fn open(path: &Path) -> Result<Self, ApiError> {
        let mut conn = db(Connection::open(path))?;
        db(conn.busy_timeout(Duration::from_secs(5)))?;
        db(conn.pragma_update(None, "foreign_keys", true))?;
        db(conn.pragma_update(None, "journal_mode", "WAL"))?;
        db(conn.pragma_update(None, "synchronous", "FULL"))?;
        let tx = db(conn.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let version: i64 = db(tx.pragma_query_value(None, "user_version", |r| r.get(0)))?;
        match version {
            0 => db(tx.execute_batch(SCHEMA))?,
            1 => {}
            _ => return Err(ApiError::storage()),
        }
        db(tx.commit())?;
        Ok(Self {
            conn,
            owned_attempts: HashSet::new(),
        })
    }

    pub fn lookup_create(&self, input: &CreateInput) -> Result<Option<CreateResult>, ApiError> {
        lookup(&self.conn, input)
    }

    pub fn create(
        &mut self,
        mut create: ValidatedCreate,
        commit_now: DateTime<Utc>,
    ) -> Result<CreateResult, ApiError> {
        let tx = db(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        if let Some(result) = lookup(&tx, &create.input)? {
            return Ok(result);
        }
        validate_create(&create, commit_now)?;
        create
            .recipients
            .sort_by(|a, b| a.target.session_id.cmp(&b.target.session_id));
        db(tx.execute(
            "INSERT INTO create_requests(request_id,input_json,result_json) VALUES (?1,?2,'')",
            params![create.input.request_id, json(&create.input)?],
        ))?;
        let mut summaries = Vec::with_capacity(create.recipients.len());
        for recipient in create.recipients {
            let schedule = Schedule {
                id: Uuid::new_v4().to_string(),
                request_id: create.input.request_id.clone(),
                created_at: commit_now,
                time: create.time.clone(),
                target: recipient.target,
                prompt: create.input.prompt.clone(),
                delivery: create.input.delivery,
                message_id: recipient.message_id,
                state: ScheduleState::Pending,
                admitted_at: None,
                attempts_used: 0,
                next_attempt_at: Some(create.time.due_at),
                certainty: AdmissionCertainty::NotAdmitted,
                last_error: None,
                lateness_ms: None,
            };
            db(tx.execute(
                "INSERT INTO schedules(id,request_id,session_id,message_id,state,next_attempt_at,
                    due_at,certainty,blocked,data_json)
                 VALUES (?1,?2,?3,?4,'pending',?5,?5,'not_admitted',0,?6)",
                params![
                    schedule.id,
                    schedule.request_id,
                    schedule.target.session_id,
                    schedule.message_id,
                    schedule.time.due_at.timestamp_millis(),
                    json(&schedule)?
                ],
            ))?;
            summaries.push(ScheduleSummary::from(&schedule));
        }
        let result = CreateResult {
            request_id: create.input.request_id,
            schedules: summaries,
        };
        db(tx.execute(
            "UPDATE create_requests SET result_json=?1 WHERE request_id=?2",
            params![json(&result)?, result.request_id],
        ))?;
        db(tx.commit())?;
        Ok(result)
    }

    pub fn show(&self, id: &str) -> Result<Schedule, ApiError> {
        load(&self.conn, id)
    }

    pub fn list(&self, page: PageInput) -> Result<Page<ScheduleSummary>, ApiError> {
        let after = cursor(&page, "list")?;
        let mut statement = db(self
            .conn
            .prepare("SELECT seq,data_json FROM schedules WHERE seq > ?1 ORDER BY seq LIMIT ?2"))?;
        let rows = db(
            statement.query_map(params![after, i64::from(page.limit) + 1], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }),
        )?;
        let mut values = Vec::new();
        for row in rows {
            let (seq, data) = db(row)?;
            values.push((seq, ScheduleSummary::from(&decode::<Schedule>(&data)?)));
        }
        make_page(values, page.limit, "list")
    }

    pub fn log(&self, id: &str, page: PageInput) -> Result<Page<DeliveryEvent>, ApiError> {
        validate_uuid(id)?;
        let scope = format!("log:{id}");
        let after = cursor(&page, &scope)?;
        // Check existence without loading a prompt just to page delivery history.
        if !db(self
            .conn
            .query_row("SELECT 1 FROM schedules WHERE id=?1", [id], |_| Ok(true))
            .optional())?
        .unwrap_or(false)
        {
            return Err(ApiError::new(
                "schedule_missing",
                "Schedule does not exist.",
            ));
        }
        let mut statement = db(self.conn.prepare("SELECT seq,data_json FROM delivery_log WHERE schedule_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3"))?;
        let rows = db(
            statement.query_map(params![id, after, i64::from(page.limit) + 1], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }),
        )?;
        let mut values = Vec::new();
        for row in rows {
            let (seq, data) = db(row)?;
            let mut event: DeliveryEvent = decode(&data)?;
            event.seq = seq;
            values.push((seq, event));
        }
        make_page(values, page.limit, &scope)
    }

    pub fn cancel(&mut self, id: &str, now: DateTime<Utc>) -> Result<CancelResult, ApiError> {
        let tx = db(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        let mut schedule = load(&tx, id)?;
        let disposition = match schedule.state {
            ScheduleState::Admitted => CancelDisposition::AlreadyAdmitted,
            ScheduleState::Cancelled => CancelDisposition::AlreadyCancelled,
            ScheduleState::Failed => CancelDisposition::AlreadyFailed,
            ScheduleState::Dispatching => CancelDisposition::OutcomeUncertain,
            ScheduleState::Pending if schedule.certainty == AdmissionCertainty::Unconfirmed => {
                CancelDisposition::OutcomeUncertain
            }
            ScheduleState::Pending => {
                schedule.state = ScheduleState::Cancelled;
                schedule.next_attempt_at = None;
                save(&tx, &schedule)?;
                append(
                    &tx,
                    &DeliveryEvent {
                        seq: 0,
                        schedule_id: id.into(),
                        ordinal: None,
                        started_at: now,
                        finished_at: Some(now),
                        phase: AttemptPhase::Discovery,
                        outcome: EventOutcome::Cancelled,
                        certainty: schedule.certainty,
                        error: None,
                        next_attempt_at: None,
                    },
                )?;
                CancelDisposition::Cancelled
            }
        };
        db(tx.commit())?;
        Ok(CancelResult {
            schedule,
            disposition,
        })
    }

    /// Runtime calls this with one immediately before its single worker starts I/O.
    pub fn claim_due(
        &mut self,
        now: DateTime<Utc>,
        limit: u16,
    ) -> Result<Vec<AttemptIntent>, ApiError> {
        bound_due(limit)?;
        let tx = db(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        let candidates = {
            let mut statement = db(tx.prepare(DUE_QUERY))?;
            let rows =
                db(statement.query_map([now.timestamp_millis()], |r| r.get::<_, String>(0)))?;
            rows.map(|row| decode::<Schedule>(&db(row)?))
                .collect::<Result<Vec<_>, ApiError>>()?
        };
        let mut intents = Vec::new();
        for mut s in candidates {
            // Index milliseconds are only a candidate filter: exact UTC precision is authoritative.
            if s.time.due_at > now || s.next_attempt_at.is_none_or(|at| at > now) {
                continue;
            }
            if s.attempts_used >= 6 {
                return Err(ApiError::storage());
            }
            s.attempts_used += 1;
            s.state = ScheduleState::Dispatching;
            s.next_attempt_at = None;
            s.lateness_ms = Some((now - s.time.due_at).num_milliseconds());
            let intent = AttemptIntent {
                schedule_id: s.id.clone(),
                ordinal: s.attempts_used,
                session_id: s.target.session_id.clone(),
                message_id: s.message_id.clone(),
                prompt: s.prompt.clone(),
                delivery: s.delivery,
                started_at: now,
                due_at: s.time.due_at,
                previous_certainty: s.certainty,
            };
            db(tx.execute(
                "INSERT INTO attempts(schedule_id,ordinal,intent_json) VALUES (?1,?2,?3)",
                params![s.id, intent.ordinal, json(&intent)?],
            ))?;
            save(&tx, &s)?;
            append(
                &tx,
                &event(
                    &intent,
                    None,
                    AttemptPhase::Discovery,
                    EventOutcome::Started,
                    s.certainty,
                    None,
                    None,
                ),
            )?;
            intents.push(intent);
            if intents.len() == usize::from(limit) {
                break;
            }
        }
        db(tx.commit())?;
        self.owned_attempts
            .extend(intents.iter().map(|i| (i.schedule_id.clone(), i.ordinal)));
        Ok(intents)
    }

    /// Grants exactly one POST permission, and only after the caller persists this boundary.
    pub fn mark_submit_intent(&mut self, id: &str, ordinal: u8) -> Result<(), ApiError> {
        validate_uuid(id)?;
        if !self.owned_attempts.contains(&(id.into(), ordinal)) {
            return Err(transition_error());
        }
        let tx = db(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        let mut s = load(&tx, id)?;
        let attempt = active_attempt(&tx, id, ordinal)?;
        if s.state != ScheduleState::Dispatching
            || s.attempts_used != ordinal
            || attempt.submitted
            || attempt.interrupted
        {
            return Err(transition_error());
        }
        db(tx.execute(
            "UPDATE attempts SET submit_intent=1 WHERE schedule_id=?1 AND ordinal=?2",
            params![id, ordinal],
        ))?;
        s.certainty = AdmissionCertainty::Unconfirmed;
        save(&tx, &s)?;
        append(
            &tx,
            &event(
                &attempt.intent,
                None,
                AttemptPhase::Submit,
                EventOutcome::SubmitIntent,
                s.certainty,
                None,
                None,
            ),
        )?;
        db(tx.commit())
    }

    pub fn finish_attempt(
        &mut self,
        intent: &AttemptIntent,
        mut result: AttemptResult,
    ) -> Result<Schedule, ApiError> {
        result.error = result.error.as_ref().map(ApiError::sanitized).transpose()?;
        if (result.outcome == AttemptOutcome::Admitted)
            != (result.certainty == AdmissionCertainty::Admitted)
        {
            return Err(ApiError::invalid());
        }
        let tx = db(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        let mut s = load(&tx, &intent.schedule_id)?;
        let attempt = active_attempt(&tx, &intent.schedule_id, intent.ordinal)?;
        if &attempt.intent != intent
            || s.state != ScheduleState::Dispatching
            || s.attempts_used != intent.ordinal
        {
            return Err(transition_error());
        }
        if (attempt.interrupted && result.phase != AttemptPhase::Reconcile)
            || (!attempt.interrupted
                && !self
                    .owned_attempts
                    .contains(&(intent.schedule_id.clone(), intent.ordinal)))
        {
            return Err(transition_error());
        }
        if result.phase == AttemptPhase::Submit && (!attempt.submitted || attempt.interrupted) {
            return Err(transition_error());
        }
        // Only positive admission evidence resolves prior uncertainty. Later rejections cannot.
        let known_current_rejection = result.phase == AttemptPhase::Submit
            && attempt.submitted
            && !attempt.interrupted
            && intent.previous_certainty == AdmissionCertainty::NotAdmitted
            && result.certainty == AdmissionCertainty::NotAdmitted;
        s.certainty = if result.certainty == AdmissionCertainty::Admitted {
            AdmissionCertainty::Admitted
        } else if !known_current_rejection
            && (s.certainty == AdmissionCertainty::Unconfirmed
                || result.certainty == AdmissionCertainty::Unconfirmed)
        {
            AdmissionCertainty::Unconfirmed
        } else {
            AdmissionCertainty::NotAdmitted
        };
        s.last_error = result.error.clone();
        let terminal = match result.outcome {
            AttemptOutcome::Admitted => {
                s.state = ScheduleState::Admitted;
                s.admitted_at = Some(result.finished_at);
                Some(EventOutcome::Admitted)
            }
            AttemptOutcome::Definitive => {
                s.state = ScheduleState::Failed;
                Some(EventOutcome::DefinitiveFailure)
            }
            _ => match retry_delay(intent.ordinal) {
                Some(delay) => {
                    s.state = ScheduleState::Pending;
                    s.next_attempt_at = Some(
                        result
                            .finished_at
                            .checked_add_signed(
                                chrono::Duration::from_std(delay)
                                    .map_err(|_| ApiError::invalid())?,
                            )
                            .ok_or_else(ApiError::invalid)?,
                    );
                    None
                }
                None => {
                    s.state = ScheduleState::Failed;
                    Some(EventOutcome::RetryExhausted)
                }
            },
        };
        db(tx.execute(
            "UPDATE attempts SET finished=1 WHERE schedule_id=?1 AND ordinal=?2",
            params![s.id, intent.ordinal],
        ))?;
        save(&tx, &s)?;
        let outcome = match result.outcome {
            AttemptOutcome::Admitted => EventOutcome::Admitted,
            AttemptOutcome::Definitive => EventOutcome::Definitive,
            AttemptOutcome::Transient => EventOutcome::Transient,
            AttemptOutcome::Interrupted if attempt.interrupted => EventOutcome::Transient,
            AttemptOutcome::Interrupted => EventOutcome::Interrupted,
        };
        append(
            &tx,
            &event(
                intent,
                Some(result.finished_at),
                result.phase,
                outcome,
                s.certainty,
                result.error.clone(),
                s.next_attempt_at,
            ),
        )?;
        if let Some(terminal) = terminal.filter(|e| *e != outcome) {
            append(
                &tx,
                &event(
                    intent,
                    Some(result.finished_at),
                    result.phase,
                    terminal,
                    s.certainty,
                    result.error,
                    None,
                ),
            )?;
        }
        db(tx.commit())?;
        self.owned_attempts
            .remove(&(intent.schedule_id.clone(), intent.ordinal));
        Ok(s)
    }

    /// Startup recovery only: returned ordinals permit reconciliation, never another POST.
    /// Recording interruption is idempotent; opening an additional connection is not a restart.
    pub fn unfinished(&self, limit: u16) -> Result<Vec<AttemptIntent>, ApiError> {
        bound_due(limit)?;
        let tx = db(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        ))?;
        let attempts = {
            let mut statement = db(tx.prepare(UNFINISHED_QUERY))?;
            let rows = db(statement.query_map([limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            }))?;
            rows.map(|r| {
                let (data, interrupted) = db(r)?;
                Ok((decode::<AttemptIntent>(&data)?, interrupted))
            })
            .collect::<Result<Vec<_>, ApiError>>()?
        };
        let mut intents = Vec::new();
        for (intent, interrupted) in attempts {
            if !interrupted {
                let s = load(&tx, &intent.schedule_id)?;
                db(tx.execute(
                    "UPDATE attempts SET interrupted=1 WHERE schedule_id=?1 AND ordinal=?2",
                    params![intent.schedule_id, intent.ordinal],
                ))?;
                append(
                    &tx,
                    &event(
                        &intent,
                        Some(Utc::now()),
                        AttemptPhase::Reconcile,
                        EventOutcome::Interrupted,
                        s.certainty,
                        None,
                        None,
                    ),
                )?;
            }
            intents.push(intent);
        }
        db(tx.commit())?;
        Ok(intents)
    }

    pub fn counts(&self) -> Result<ScheduleCounts, ApiError> {
        db(self.conn.query_row(
            "SELECT COALESCE(SUM(state='pending'),0), COALESCE(SUM(blocked),0),
                COALESCE(SUM(state='failed'),0), COALESCE(SUM(certainty='unconfirmed'),0)
             FROM schedules",
            [],
            |r| {
                Ok(ScheduleCounts {
                    pending: r.get(0)?,
                    blocked: r.get(1)?,
                    failed: r.get(2)?,
                    admission_unconfirmed: r.get(3)?,
                })
            },
        ))
    }
}

fn lookup(conn: &Connection, input: &CreateInput) -> Result<Option<CreateResult>, ApiError> {
    input.validate()?;
    let row = db(conn
        .query_row(
            "SELECT input_json,result_json FROM create_requests WHERE request_id=?1",
            [&input.request_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional())?;
    match row {
        None => Ok(None),
        Some((original, result)) => {
            if original != json(input)? {
                return Err(conflict());
            }
            Ok(Some(decode(&result)?))
        }
    }
}
fn validate_create(create: &ValidatedCreate, now: DateTime<Utc>) -> Result<(), ApiError> {
    create.input.validate()?;
    if create.recipients.is_empty()
        || create.time.due_at <= now
        || create.time.original != create.input.at
        || create.time.display.is_empty()
    {
        return Err(ApiError::invalid());
    }
    if create.recipients.len() > MAX_RECIPIENTS {
        return Err(ApiError::new(
            "resource_limit",
            "Too many schedule recipients.",
        ));
    }
    let mut sessions = HashSet::new();
    let mut messages = HashSet::new();
    for r in &create.recipients {
        validate_session_id(&r.target.session_id)?;
        validate_message_id(&r.message_id)?;
        if !sessions.insert(&r.target.session_id)
            || !messages.insert(&r.message_id)
            || r.target
                .title
                .as_ref()
                .is_some_and(|t| t.len() > MAX_TITLE_BYTES)
        {
            return Err(ApiError::invalid());
        }
        match &create.input.target {
            TargetSelector::Id(id)
                if create.recipients.len() != 1 || id != &r.target.session_id =>
            {
                return Err(ApiError::invalid());
            }
            TargetSelector::Title(title) if r.target.title.as_ref() != Some(title) => {
                return Err(ApiError::invalid());
            }
            _ => {}
        }
    }
    Ok(())
}
fn load(conn: &Connection, id: &str) -> Result<Schedule, ApiError> {
    validate_uuid(id)?;
    let data: Option<String> = db(conn
        .query_row("SELECT data_json FROM schedules WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional())?;
    decode(&data.ok_or_else(|| ApiError::new("schedule_missing", "Schedule does not exist."))?)
}
fn save(conn: &Connection, s: &Schedule) -> Result<(), ApiError> {
    let state = json(&s.state)?;
    let certainty = json(&s.certainty)?;
    let blocked = s.state == ScheduleState::Pending && s.last_error.is_some();
    db(conn.execute(
        "UPDATE schedules SET state=?1,next_attempt_at=?2,certainty=?3,blocked=?4,data_json=?5
         WHERE id=?6",
        params![
            state.trim_matches('"'),
            s.next_attempt_at.map(|at| at.timestamp_millis()),
            certainty.trim_matches('"'),
            blocked,
            json(s)?,
            s.id
        ],
    ))?;
    Ok(())
}
fn append(conn: &Connection, event: &DeliveryEvent) -> Result<(), ApiError> {
    db(conn.execute(
        "INSERT INTO delivery_log(schedule_id,data_json) VALUES (?1,?2)",
        params![event.schedule_id, json(event)?],
    ))?;
    Ok(())
}
fn event(
    intent: &AttemptIntent,
    finished_at: Option<DateTime<Utc>>,
    phase: AttemptPhase,
    outcome: EventOutcome,
    certainty: AdmissionCertainty,
    error: Option<ApiError>,
    next_attempt_at: Option<DateTime<Utc>>,
) -> DeliveryEvent {
    DeliveryEvent {
        seq: 0,
        schedule_id: intent.schedule_id.clone(),
        ordinal: Some(intent.ordinal),
        started_at: intent.started_at,
        finished_at,
        phase,
        outcome,
        certainty,
        error,
        next_attempt_at,
    }
}
struct ActiveAttempt {
    intent: AttemptIntent,
    submitted: bool,
    interrupted: bool,
}
fn active_attempt(conn: &Connection, id: &str, ordinal: u8) -> Result<ActiveAttempt, ApiError> {
    let row = db(conn
        .query_row(
            "SELECT intent_json,submit_intent,interrupted FROM attempts
         WHERE schedule_id=?1 AND ordinal=?2 AND finished=0",
            params![id, ordinal],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            },
        )
        .optional())?
    .ok_or_else(transition_error)?;
    Ok(ActiveAttempt {
        intent: decode(&row.0)?,
        submitted: row.1,
        interrupted: row.2,
    })
}
fn bound_due(limit: u16) -> Result<(), ApiError> {
    if !(1..=MAX_DUE_CANDIDATES).contains(&limit) {
        return Err(ApiError::invalid());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    scope: String,
    after: i64,
}
fn cursor(page: &PageInput, scope: &str) -> Result<i64, ApiError> {
    page.validate()?;
    let Some(value) = &page.cursor else {
        return Ok(0);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::invalid())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid())?;
    if cursor.scope != scope || cursor.after <= 0 {
        return Err(ApiError::invalid());
    }
    Ok(cursor.after)
}
fn make_page<T>(mut values: Vec<(i64, T)>, limit: u16, scope: &str) -> Result<Page<T>, ApiError> {
    let more = values.len() > usize::from(limit);
    values.truncate(usize::from(limit));
    let next_cursor = if more {
        Some(URL_SAFE_NO_PAD.encode(json(&Cursor {
            scope: scope.into(),
            after: values.last().ok_or_else(ApiError::storage)?.0,
        })?))
    } else {
        None
    };
    Ok(Page {
        items: values.into_iter().map(|(_, item)| item).collect(),
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::resolve_at;
    use chrono::{DateTime, Duration, Utc};
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn fixture() -> (TempDir, Store) {
        std::fs::create_dir_all("target/test-data").unwrap();
        let dir = tempfile::tempdir_in("target/test-data").unwrap();
        let store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
        (dir, store)
    }
    fn now() -> DateTime<Utc> {
        "2026-11-05T00:00:00Z".parse().unwrap()
    }
    fn create(ids: &[&str]) -> ValidatedCreate {
        let input = CreateInput {
            request_id: Uuid::new_v4().to_string(),
            target: if ids.len() == 1 {
                TargetSelector::Id(ids[0].into())
            } else {
                TargetSelector::Title("fixture".into())
            },
            at: "2026-11-06T01:00:00Z".into(),
            prompt: "private fixture prompt".into(),
            delivery: DeliveryMode::Steer,
        };
        let time = resolve_at(&input.at, &Utc, None, now()).unwrap();
        let recipients = ids
            .iter()
            .map(|id| ResolvedRecipient {
                target: Target {
                    session_id: (*id).into(),
                    title: Some("fixture".into()),
                },
                message_id: format!("msg_{}", Uuid::new_v4().simple()),
            })
            .collect();
        ValidatedCreate {
            input,
            recipients,
            time,
        }
    }
    fn add(store: &mut Store) -> Schedule {
        let result = store.create(create(&["ses_test"]), now()).unwrap();
        store.show(&result.schedules[0].id).unwrap()
    }
    fn failure(at: DateTime<Utc>, outcome: AttemptOutcome) -> AttemptResult {
        AttemptResult {
            finished_at: at,
            phase: AttemptPhase::Discovery,
            outcome,
            certainty: AdmissionCertainty::NotAdmitted,
            error: Some(ApiError::new(
                "opencode_unavailable",
                "Service unavailable.",
            )),
        }
    }
    fn all_logs(store: &Store, id: &str) -> Vec<DeliveryEvent> {
        let mut cursor = None;
        let mut items = Vec::new();
        loop {
            let page = store.log(id, PageInput { limit: 2, cursor }).unwrap();
            items.extend(page.items);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return items;
            }
        }
    }

    #[test]
    fn create_batch_is_durable_idempotent_and_conflict_checked() {
        let (dir, mut store) = fixture();
        let batch = create(&["ses_c", "ses_a", "ses_b"]);
        let input = batch.input.clone();
        let result = store.create(batch.clone(), now()).unwrap();
        assert_eq!(result.schedules.len(), 3);
        assert_eq!(
            result
                .schedules
                .iter()
                .map(|s| s.target.session_id.as_str())
                .collect::<Vec<_>>(),
            ["ses_a", "ses_b", "ses_c"]
        );
        assert_eq!(
            result
                .schedules
                .iter()
                .map(|s| &s.id)
                .collect::<HashSet<_>>()
                .len(),
            3
        );
        assert_eq!(
            result
                .schedules
                .iter()
                .map(|s| &s.message_id)
                .collect::<HashSet<_>>()
                .len(),
            3
        );
        assert!(
            result
                .schedules
                .iter()
                .all(|s| s.request_id == input.request_id)
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private fixture prompt")
        );
        assert!(
            !serde_json::to_value(&result.schedules[0])
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("prompt")
        );
        drop(store);
        let mut store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
        assert_eq!(store.lookup_create(&input).unwrap(), Some(result.clone()));
        assert_eq!(
            store
                .create(batch.clone(), now() + Duration::days(3))
                .unwrap(),
            result
        );
        for field in ["prompt", "target", "at", "delivery"] {
            let mut changed = input.clone();
            match field {
                "prompt" => changed.prompt.push('!'),
                "target" => changed.target = TargetSelector::Id("ses_other".into()),
                "at" => changed.at = "2026-11-07T01:00:00Z".into(),
                _ => changed.delivery = DeliveryMode::Queue,
            }
            assert_eq!(
                store.lookup_create(&changed).unwrap_err().code,
                "request_conflict"
            );
            let mut changed_batch = batch.clone();
            changed_batch.input = changed;
            assert_eq!(
                store.create(changed_batch, now()).unwrap_err().code,
                "request_conflict"
            );
        }
        assert_eq!(
            store
                .create(create(&["ses_single"]), now())
                .unwrap()
                .schedules
                .len(),
            1
        );
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "busy_timeout", |r| r.get::<_, i64>(0))
                .unwrap(),
            5000
        );
    }

    #[test]
    fn batch_creation_is_all_or_none() {
        let (_dir, mut store) = fixture();
        // A real SQLite trigger aborts the second insert, after recipient one was written.
        store.conn.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON schedules WHEN NEW.session_id='ses_b' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let batch = create(&["ses_a", "ses_b", "ses_c"]);
        assert_eq!(
            store.create(batch.clone(), now()).unwrap_err().code,
            "storage_failure"
        );
        assert!(store.lookup_create(&batch.input).unwrap().is_none());
        assert!(store.list(PageInput::default()).unwrap().items.is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER fail_second;")
            .unwrap();
        // A deferred FK fails at COMMIT rather than at either recipient insert.
        store.conn.execute_batch("CREATE TABLE commit_guard (id TEXT REFERENCES create_requests(request_id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_commit AFTER INSERT ON schedules BEGIN INSERT INTO commit_guard VALUES ('missing'); END;").unwrap();
        assert!(store.create(batch.clone(), now()).is_err());
        assert!(store.lookup_create(&batch.input).unwrap().is_none());
        assert!(store.list(PageInput::default()).unwrap().items.is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER fail_commit;")
            .unwrap();
        let mut duplicate = batch.clone();
        duplicate.recipients[1].target = duplicate.recipients[0].target.clone();
        assert_eq!(
            store.create(duplicate, now()).unwrap_err().code,
            "invalid_input"
        );
        let mut empty = batch.clone();
        empty.recipients.clear();
        assert!(store.create(empty, now()).is_err());
        assert!(store.create(batch.clone(), batch.time.due_at).is_err());

        let (dir, first) = fixture();
        drop(first);
        let path = Arc::new(dir.path().join("db.sqlite3"));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [vec!["ses_a", "ses_b"], vec!["ses_c", "ses_d", "ses_e"]]
            .into_iter()
            .map(|ids| {
                let path = path.clone();
                let barrier = barrier.clone();
                let original = batch.input.clone();
                std::thread::spawn(move || {
                    let mut db = Store::open(&path).unwrap();
                    let mut candidate = create(&ids);
                    candidate.input = original;
                    barrier.wait();
                    db.create(candidate, now()).unwrap()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results[0], results[1]);
        let db = Store::open(&path).unwrap();
        assert_eq!(
            db.list(PageInput::default()).unwrap().items.len(),
            results[0].schedules.len()
        );
    }

    #[test]
    fn batch_retry_freezes_recipients_and_original_result() {
        let (_dir, mut store) = fixture();
        let batch = create(&["ses_a", "ses_b", "ses_c"]);
        let result = store.create(batch.clone(), now()).unwrap();
        let bytes = serde_json::to_vec(&result).unwrap();
        let id = &result.schedules[0].id;
        assert_eq!(
            store.cancel(id, now()).unwrap().disposition,
            CancelDisposition::Cancelled
        );
        let mut retry = batch.clone();
        retry.recipients.remove(0);
        retry.recipients[0].target.title = Some("renamed".into());
        retry
            .recipients
            .push(create(&["ses_new"]).recipients.remove(0));
        assert_eq!(
            serde_json::to_vec(&store.create(retry, now() + Duration::days(9)).unwrap()).unwrap(),
            bytes
        );
        assert_eq!(
            serde_json::to_vec(&store.lookup_create(&batch.input).unwrap().unwrap()).unwrap(),
            bytes
        );
        assert_eq!(store.show(id).unwrap().state, ScheduleState::Cancelled);
        assert_eq!(
            store.show(&result.schedules[1].id).unwrap().state,
            ScheduleState::Pending
        );
        assert!(all_logs(&store, &result.schedules[1].id).is_empty());
        assert_eq!(all_logs(&store, id).len(), 1);
    }

    #[test]
    fn claim_rechecks_wall_clock_and_recovers_overdue() {
        let (_dir, mut store) = fixture();
        let s = add(&mut store);
        assert!(
            store
                .claim_due(s.time.due_at - Duration::nanoseconds(1), 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .claim_due(now() - Duration::days(1), 1)
                .unwrap()
                .is_empty()
        );
        let later = s.time.due_at + Duration::days(1);
        let intent = store.claim_due(later, 1).unwrap().remove(0);
        assert_eq!(intent.due_at.to_rfc3339(), "2026-11-06T01:00:00+00:00");
        assert_eq!(store.show(&s.id).unwrap().lateness_ms, Some(86_400_000));
        assert!(store.claim_due(later, 1).unwrap().is_empty());
        assert_eq!(store.show(&s.id).unwrap().attempts_used, 1);
    }

    #[test]
    fn six_attempts_use_completion_based_backoff() {
        let (_dir, mut store) = fixture();
        let s = add(&mut store);
        let mut clock = s.time.due_at;
        let mut ordinals = Vec::new();
        for gap in [Some(10), Some(20), Some(40), Some(80), Some(160), None] {
            let intent = store.claim_due(clock, 1).unwrap().remove(0);
            ordinals.push(intent.ordinal);
            let finish = clock + Duration::seconds(7);
            let next = store
                .finish_attempt(&intent, failure(finish, AttemptOutcome::Transient))
                .unwrap();
            if let Some(gap) = gap {
                let expected = finish + Duration::seconds(gap);
                assert_eq!(next.next_attempt_at, Some(expected));
                assert!(
                    store
                        .claim_due(expected - Duration::nanoseconds(1), 1)
                        .unwrap()
                        .is_empty()
                );
                clock = expected;
            } else {
                assert_eq!(next.state, ScheduleState::Failed);
                assert_eq!(next.next_attempt_at, None);
            }
        }
        assert_eq!(ordinals, [1, 2, 3, 4, 5, 6]);
        assert!(
            store
                .claim_due(clock + Duration::days(1), 32)
                .unwrap()
                .is_empty()
        );
        let log = all_logs(&store, &s.id);
        assert_eq!(log.last().unwrap().outcome, EventOutcome::RetryExhausted);
        assert_eq!(
            log.iter()
                .filter(|e| e.outcome == EventOutcome::Transient)
                .count(),
            6
        );
        assert_eq!(store.counts().unwrap().failed, 1);
    }

    #[test]
    fn restart_preserves_budget_and_deadlines_without_expiry() {
        let (dir, mut store) = fixture();
        let s = add(&mut store);
        let mut clock = s.time.due_at;
        for ordinal in 1..=6 {
            let intent = store.claim_due(clock, 1).unwrap().remove(0);
            assert_eq!(intent.ordinal, ordinal);
            let updated = store
                .finish_attempt(&intent, failure(clock, AttemptOutcome::Transient))
                .unwrap();
            if ordinal < 6 {
                clock = updated.next_attempt_at.unwrap();
                if ordinal == 1 || ordinal == 3 {
                    drop(store);
                    store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
                    assert_eq!(store.show(&s.id).unwrap().next_attempt_at, Some(clock));
                    clock += Duration::days(1);
                    assert_eq!(store.show(&s.id).unwrap().attempts_used, ordinal);
                }
            }
        }
        assert_eq!(store.show(&s.id).unwrap().attempts_used, 6);
    }

    #[test]
    fn interrupted_submit_retains_identity_and_uncertainty() {
        let (dir, mut store) = fixture();
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        store.mark_submit_intent(&s.id, 1).unwrap();
        assert!(store.mark_submit_intent(&s.id, 1).is_err());
        assert_eq!(
            store.show(&s.id).unwrap().certainty,
            AdmissionCertainty::Unconfirmed
        );
        drop(store);
        let mut store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
        assert_eq!(store.unfinished(32).unwrap(), vec![intent.clone()]);
        assert_eq!(store.unfinished(32).unwrap(), vec![intent.clone()]);
        assert!(store.mark_submit_intent(&s.id, 1).is_err());
        assert_eq!(
            all_logs(&store, &s.id)
                .iter()
                .filter(|e| e.outcome == EventOutcome::Interrupted)
                .count(),
            1
        );
        let mut result = failure(
            s.time.due_at + Duration::seconds(2),
            AttemptOutcome::Interrupted,
        );
        result.phase = AttemptPhase::Reconcile;
        let updated = store.finish_attempt(&intent, result).unwrap();
        assert_eq!(updated.certainty, AdmissionCertainty::Unconfirmed);
        let second = store
            .claim_due(updated.next_attempt_at.unwrap(), 1)
            .unwrap()
            .remove(0);
        assert_eq!(second.message_id, intent.message_id);
        assert_eq!(second.ordinal, 2);
        let updated = store
            .finish_attempt(
                &second,
                failure(second.started_at, AttemptOutcome::Definitive),
            )
            .unwrap();
        assert_eq!(updated.state, ScheduleState::Failed);
        assert_eq!(updated.certainty, AdmissionCertainty::Unconfirmed);
        assert_eq!(
            all_logs(&store, &s.id).last().unwrap().outcome,
            EventOutcome::DefinitiveFailure
        );
        assert_eq!(store.counts().unwrap().admission_unconfirmed, 1);
        assert!(
            store
                .finish_attempt(
                    &second,
                    failure(second.started_at, AttemptOutcome::Transient)
                )
                .is_err()
        );
    }

    #[test]
    fn recovery_before_submit_cannot_gain_post_permission() {
        let (dir, mut store) = fixture();
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        drop(store);
        let mut store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
        assert!(
            store.mark_submit_intent(&s.id, 1).is_err(),
            "only the live claimant may begin a submission"
        );
        assert!(
            store
                .finish_attempt(&intent, failure(s.time.due_at, AttemptOutcome::Transient))
                .is_err(),
            "recovery must preserve interruption before recording a result"
        );
        assert_eq!(store.unfinished(1).unwrap(), vec![intent.clone()]);
        drop(store);
        let mut store = Store::open(&dir.path().join("db.sqlite3")).unwrap();
        assert_eq!(store.unfinished(1).unwrap(), vec![intent.clone()]);
        assert_eq!(
            all_logs(&store, &s.id)
                .iter()
                .filter(|e| e.outcome == EventOutcome::Interrupted)
                .count(),
            1
        );
        let mut result = failure(
            s.time.due_at + Duration::seconds(1),
            AttemptOutcome::Interrupted,
        );
        result.phase = AttemptPhase::Reconcile;
        let updated = store.finish_attempt(&intent, result).unwrap();
        assert_eq!(updated.certainty, AdmissionCertainty::NotAdmitted);
        let next = store
            .claim_due(updated.next_attempt_at.unwrap(), 1)
            .unwrap()
            .remove(0);
        assert_eq!(next.ordinal, 2);
        assert_eq!(next.message_id, intent.message_id);
        store.mark_submit_intent(&s.id, 2).unwrap();
    }

    #[test]
    fn explicit_rejection_resolves_only_its_own_submission_and_errors_are_sanitized() {
        let (_dir, mut store) = fixture();
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        store.mark_submit_intent(&s.id, 1).unwrap();
        let mut rejection = failure(s.time.due_at, AttemptOutcome::Transient);
        rejection.phase = AttemptPhase::Submit;
        rejection.error = Some(ApiError::new(
            "opencode_unavailable",
            "private fixture prompt; secret raw response",
        ));
        let updated = store.finish_attempt(&intent, rejection).unwrap();
        assert_eq!(
            updated.certainty,
            AdmissionCertainty::NotAdmitted,
            "a known rejection resolves this attempt's intent"
        );
        assert!(
            !serde_json::to_string(&all_logs(&store, &s.id))
                .unwrap()
                .contains("private fixture prompt")
        );
        assert!(
            !updated
                .last_error
                .unwrap()
                .message
                .contains("secret raw response")
        );
        assert_eq!(
            store.cancel(&s.id, now()).unwrap().disposition,
            CancelDisposition::Cancelled
        );

        let s = add(&mut store);
        let mut clock = s.time.due_at;
        for ordinal in 1..=6 {
            let intent = store.claim_due(clock, 1).unwrap().remove(0);
            store.mark_submit_intent(&s.id, ordinal).unwrap();
            let mut result = failure(clock, AttemptOutcome::Transient);
            result.phase = AttemptPhase::Submit;
            if ordinal == 1 {
                result.certainty = AdmissionCertainty::Unconfirmed;
            }
            let updated = store.finish_attempt(&intent, result).unwrap();
            assert_eq!(updated.certainty, AdmissionCertainty::Unconfirmed);
            if ordinal < 6 {
                clock = updated.next_attempt_at.unwrap();
            }
        }
        let updated = store.show(&s.id).unwrap();
        assert_eq!(updated.state, ScheduleState::Failed);
        assert_eq!(
            all_logs(&store, &s.id).last().unwrap().outcome,
            EventOutcome::RetryExhausted
        );
        assert_eq!(
            all_logs(&store, &s.id).last().unwrap().certainty,
            AdmissionCertainty::Unconfirmed
        );
    }

    #[test]
    fn cancel_is_idempotent_and_race_safe() {
        let (_dir, mut store) = fixture();
        let cancelled = add(&mut store);
        assert_eq!(
            store.cancel(&cancelled.id, now()).unwrap().disposition,
            CancelDisposition::Cancelled
        );
        assert_eq!(
            store.cancel(&cancelled.id, now()).unwrap().disposition,
            CancelDisposition::AlreadyCancelled
        );
        assert_eq!(all_logs(&store, &cancelled.id).len(), 1);
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        assert_eq!(
            store.cancel(&s.id, now()).unwrap().disposition,
            CancelDisposition::OutcomeUncertain
        );
        let updated = store
            .finish_attempt(&intent, failure(s.time.due_at, AttemptOutcome::Transient))
            .unwrap();
        assert_eq!(store.counts().unwrap().blocked, 1);
        assert_eq!(
            store
                .cancel(&s.id, updated.next_attempt_at.unwrap())
                .unwrap()
                .disposition,
            CancelDisposition::Cancelled
        );
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        store.mark_submit_intent(&s.id, 1).unwrap();
        let updated = store
            .finish_attempt(&intent, failure(s.time.due_at, AttemptOutcome::Transient))
            .unwrap();
        assert_eq!(
            store
                .cancel(&s.id, updated.next_attempt_at.unwrap())
                .unwrap()
                .disposition,
            CancelDisposition::OutcomeUncertain
        );
        let second = store
            .claim_due(updated.next_attempt_at.unwrap(), 1)
            .unwrap()
            .remove(0);
        let admitted = AttemptResult {
            finished_at: second.started_at,
            phase: AttemptPhase::Reconcile,
            outcome: AttemptOutcome::Admitted,
            certainty: AdmissionCertainty::Admitted,
            error: None,
        };
        store.finish_attempt(&second, admitted).unwrap();
        assert_eq!(
            store.cancel(&s.id, now()).unwrap().disposition,
            CancelDisposition::AlreadyAdmitted
        );
        let s = add(&mut store);
        let intent = store.claim_due(s.time.due_at, 1).unwrap().remove(0);
        store
            .finish_attempt(&intent, failure(s.time.due_at, AttemptOutcome::Definitive))
            .unwrap();
        let before = all_logs(&store, &s.id);
        assert_eq!(
            store.cancel(&s.id, now()).unwrap().disposition,
            CancelDisposition::AlreadyFailed
        );
        assert_eq!(before, all_logs(&store, &s.id));
    }

    #[test]
    fn pagination_is_bounded_and_stable() {
        let (_dir, mut store) = fixture();
        let mut ids = Vec::new();
        for _ in 0..40 {
            ids.push(add(&mut store).id);
        }
        let mut cursor = None;
        let mut seen = Vec::new();
        loop {
            let page = store.list(PageInput { limit: 7, cursor }).unwrap();
            seen.extend(page.items.into_iter().map(|s| s.id));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, ids);
        for limit in [0, 101] {
            assert!(
                store
                    .list(PageInput {
                        limit,
                        cursor: None
                    })
                    .is_err()
            );
        }
        for cursor in ["not base64", "e30", &"x".repeat(513)] {
            assert!(
                store
                    .list(PageInput {
                        limit: 50,
                        cursor: Some(cursor.into())
                    })
                    .is_err()
            );
        }
        assert!(store.show("bogus").is_err());
        assert_eq!(
            store.show(&Uuid::new_v4().to_string()).unwrap_err().code,
            "schedule_missing"
        );
        assert!(store.claim_due(now(), 33).is_err());
        let due: DateTime<Utc> = "2026-11-06T01:00:00Z".parse().unwrap();
        assert_eq!(store.claim_due(due, 32).unwrap().len(), 32);
        assert_eq!(store.claim_due(due, 32).unwrap().len(), 8);
        let plan: String = store
            .conn
            .query_row(
                &format!("EXPLAIN QUERY PLAN {DUE_QUERY}"),
                [due.timestamp_millis()],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("schedules_due"), "{plan}");
        let recovery_plan: String = store
            .conn
            .query_row(
                &format!("EXPLAIN QUERY PLAN {UNFINISHED_QUERY}"),
                [32],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            recovery_plan.contains("unfinished_attempts"),
            "{recovery_plan}"
        );
        let log = all_logs(&store, &ids[0]);
        assert!(log.windows(2).all(|w| w[0].seq < w[1].seq));
        assert!(
            !serde_json::to_string(&log)
                .unwrap()
                .contains("private fixture prompt")
        );
        let list_cursor = store
            .list(PageInput {
                limit: 1,
                cursor: None,
            })
            .unwrap()
            .next_cursor;
        assert!(
            store
                .log(
                    &ids[0],
                    PageInput {
                        limit: 1,
                        cursor: list_cursor
                    }
                )
                .is_err()
        );
    }
}
