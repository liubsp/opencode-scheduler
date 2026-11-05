use crate::{
    adapter::Adapter,
    runtime::{Paths, absent_status},
    store::Store,
    time::resolve_at,
    types::*,
};
use chrono::Utc;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;
use uuid::Uuid;

pub struct Service {
    pub store: Mutex<Store>,
    pub adapter: Adapter,
    pub paths: Paths,
    pub stopping: AtomicBool,
    pub stop: Notify,
}
impl Service {
    pub fn new(paths: Paths) -> Result<Arc<Self>, ApiError> {
        Ok(Arc::new(Self {
            store: Mutex::new(Store::open(&paths.file("schedules.sqlite3"))?),
            adapter: Adapter::new()?,
            paths,
            stopping: AtomicBool::new(false),
            stop: Notify::new(),
        }))
    }
    pub fn store(&self) -> Result<MutexGuard<'_, Store>, ApiError> {
        self.store.lock().map_err(|_| ApiError::storage())
    }
    pub async fn execute(&self, request: Operation) -> Result<OperationResult, ApiError> {
        match request {
            Operation::ScheduleCreate(input) => {
                if self.stopping.load(Ordering::SeqCst) {
                    return Err(crate::adapter::unavailable());
                }
                input.validate()?;
                if let Some(result) = self.store()?.lookup_create(&input)? {
                    return Ok(OperationResult::Created(result));
                }
                let now = Utc::now();
                let time = if chrono::DateTime::parse_from_rfc3339(&input.at).is_ok() {
                    resolve_at(&input.at, &Utc, None, now)?
                } else {
                    let name = iana_time_zone::get_timezone().map_err(|_| ApiError::invalid())?;
                    let zone: chrono_tz::Tz = name.parse().map_err(|_| ApiError::invalid())?;
                    resolve_at(&input.at, &zone, Some(&name), now)?
                };
                let mut adapter = self.adapter.clone();
                let targets = adapter.resolve_targets(&input.target).await?;
                if self.stopping.load(Ordering::SeqCst) {
                    return Err(crate::adapter::unavailable());
                }
                let recipients = targets
                    .into_iter()
                    .map(|target| ResolvedRecipient {
                        target,
                        message_id: format!("msg_{}", Uuid::new_v4().simple()),
                    })
                    .collect();
                Ok(OperationResult::Created(self.store()?.create(
                    ValidatedCreate {
                        input,
                        recipients,
                        time,
                    },
                    Utc::now(),
                )?))
            }
            Operation::ScheduleList(page) => Ok(OperationResult::Page(self.store()?.list(page)?)),
            Operation::ScheduleShow(input) => {
                Ok(OperationResult::Schedule(self.store()?.show(&input.id)?))
            }
            Operation::ScheduleCancel(input) => {
                let result = self.store()?.cancel(&input.id, Utc::now())?;
                if result.disposition == CancelDisposition::OutcomeUncertain {
                    return Err(ApiError::new(
                        "outcome_uncertain",
                        "Prompt admission is unconfirmed.",
                    ));
                }
                if result.disposition == CancelDisposition::AlreadyAdmitted {
                    return Err(ApiError::new(
                        "already_admitted",
                        "Prompt is already admitted.",
                    ));
                }
                Ok(OperationResult::Cancel(result))
            }
            Operation::ScheduleLog(input) => Ok(OperationResult::Log(self.store()?.log(
                &input.id,
                PageInput {
                    limit: input.limit,
                    cursor: input.cursor,
                },
            )?)),
            Operation::Status(_) => {
                let counts = self.store()?.counts()?;
                let mut adapter = self.adapter.clone();
                let (state, version) = match adapter.discover().await {
                    Ok(info) => ("ready", Some(info.version)),
                    Err(error) => (
                        match error.code.as_str() {
                            "unsupported_service" => "unsupported",
                            "authentication_failure" => "authentication_failed",
                            _ => "unavailable",
                        },
                        None,
                    ),
                };
                let mut value = absent_status(&self.paths);
                value["daemon"] = serde_json::json!({"reachable":true,"protocol_compatible":true});
                value["schedules"] =
                    serde_json::to_value(counts).map_err(|_| ApiError::storage())?;
                value["opencode"] = serde_json::json!({"state":state,"service_version":version});
                Ok(OperationResult::Status(value))
            }
            Operation::DaemonStop(_) => {
                self.stopping.store(true, Ordering::SeqCst);
                self.stop.notify_one();
                Ok(OperationResult::Stopping(
                    serde_json::json!({"accepted":true}),
                ))
            }
        }
    }
}
