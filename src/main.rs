use clap::{Args, Parser, Subcommand};
use opencode_scheduler::{
    daemon,
    runtime::{self, Paths},
    types::*,
};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    version,
    about = "Durable one-shot prompt scheduling for OpenCode V2",
    after_help = "Examples:\n  opencode-scheduler status\n  opencode-scheduler schedule create --session ses_example --at 5:00 --prompt 'Continue the task'\n  opencode-scheduler schedule list\n\nUse <command> --help for all arguments. Scheduling is manual, one-shot, and requires the machine to be awake; no wake or login startup registration is installed."
)]
struct Cli {
    /// Return the structured API envelope on stdout instead of readable summaries (automation).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Manage one-shot prompts and inspect their delivery history.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Manage the private background scheduler (not the OpenCode service).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Inspect installation, daemon, OpenCode service and counts; read-only, never starts services.
    Status,
    /// Install/upgrade the global CLI and user PATH, preserving schedules and unrelated entries.
    Install {
        /// Install without changing the user's PATH (isolated/manual registration).
        #[arg(long)]
        no_path: bool,
    },
    #[command(hide = true)]
    DaemonRun,
}
#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the scheduler and resume pending work; does not register login/startup tasks.
    Start,
    /// Inspect service health and counts; read-only, does not start either service.
    Status,
    /// Stop only the scheduler, preserving schedules; does not stop OpenCode or retract admission.
    Stop,
}
#[derive(Args)]
struct Paging {
    /// Maximum items in this page (1-100); list omits prompt bodies.
    #[arg(long, default_value_t = 50)]
    limit: u16,
    /// Opaque continuation cursor printed by the previous page; pass unchanged.
    #[arg(long)]
    cursor: Option<String>,
}
#[derive(Subcommand)]
enum ScheduleCommand {
    /// Create an atomic batch; title selection freezes ALL exact matches at creation.
    #[command(
        after_help = "Time formats:\n  5:00, 05:00 or 05:00:30: next strictly future local occurrence, today or tomorrow.\n  'YYYY-MM-DD HH:MM[:SS]': explicit local date on the daemon host.\n  RFC 3339 with offset, e.g. 2099-11-06T05:00:00+01:00: explicit instant.\nLocal forms use the daemon host IANA timezone. DST gaps/overlaps require an explicit offset; no guessing. The full resolved date, offset and timezone are printed. The appointment must still be future at commit; validation does not roll it forward again.\n\nDelivery: steer admits immediately into the target session; queue waits in OpenCode's queue. Admission is not agent completion. Six total attempts use the same message ID, with 10/20/40/80/160 second waits after failures; restart does not reset the budget.\n\nExamples:\n  opencode-scheduler schedule create --session ses_example --at 5:00 --prompt 'Continue the task'\n  opencode-scheduler schedule create --session-title 'Exact title' --at '2099-11-06 05:00' --prompt 'Continue' --delivery queue\n\nA request UUID is printed before contacting the daemon. If the response is lost, repeat identical input with --request-id <printed-UUID>. Replay returns the original frozen batch before resolving the clock again, even on another day. Never use a new UUID as a blind retry."
    )]
    Create {
        /// Existing OpenCode session ID (ses...); mutually exclusive with --session-title.
        #[arg(
            long,
            required_unless_present = "session_title",
            conflicts_with = "session_title"
        )]
        session: Option<String>,
        /// Exact, case-sensitive service-wide title: ALL matches receive separate schedules.
        #[arg(long, required_unless_present = "session", conflicts_with = "session")]
        session_title: Option<String>,
        /// Appointment: clock H:MM[:SS], local YYYY-MM-DD HH:MM[:SS], or RFC 3339 with offset.
        #[arg(long)]
        at: String,
        /// Prompt text as received from the shell (nonblank; at most 65536 UTF-8 bytes).
        #[arg(
            long,
            long_help = "Prompt text as received from the shell (nonblank; at most 65536 UTF-8 bytes). Windows PowerShell 5.1 legacy argument passing can remove embedded double quotes and alter adjacent backslashes. For quoted/multiline prompts, run in PowerShell 7.3+ (pwsh), set $PSNativeCommandArgumentPassing = 'Standard', and pass a single-quoted here-string variable with --prompt $prompt. Save recipe scripts as UTF-8. See the README/CLI skill for a complete recipe; do not forward the prompt through a legacy shell. The CLI cannot reconstruct text already changed by the shell."
        )]
        prompt: String,
        /// OpenCode admission mode, not a guarantee of agent completion; see below.
        #[arg(long,default_value="steer",value_parser=["steer","queue"])]
        delivery: String,
        /// UUID v4 retry identity; reuse only with identical input after a lost response.
        #[arg(long)]
        request_id: Option<String>,
    },
    /// List schedules, states and attempts without prompt bodies; use --cursor for more.
    List {
        #[command(flatten)]
        page: Paging,
    },
    /// Show one schedule's target, date and admission state; --json also includes the literal prompt.
    Show {
        /// Schedule UUID from schedule list (not the creation request UUID).
        id: String,
    },
    /// Cancel pending delivery; cannot retract admission or guarantee cancellation if uncertain.
    Cancel {
        /// Schedule UUID from schedule list; cancellation is idempotent.
        id: String,
    },
    /// Inspect delivery attempts, errors and uncertainty for one schedule.
    #[command(
        after_help = "There are six attempts maximum; waits are 10/20/40/80/160 seconds after each failed attempt completes. Automatic retries preserve the same message ID. There is no manual retry/reset command. Admitted means accepted by OpenCode, not agent completion; unconfirmed must not be treated as rejection."
    )]
    Log {
        /// Schedule UUID from schedule list (not a request UUID).
        id: String,
        #[command(flatten)]
        page: Paging,
    },
}
async fn execute(cli: Cli) -> Result<Value, ApiError> {
    if let Command::Schedule {
        command: ScheduleCommand::Create { at, .. },
    } = &cli.command
    {
        // Only syntax is safe before replay lookup; clocks and past dates remain RPC-authoritative.
        opencode_scheduler::time::validate_syntax(at)?;
    }
    let paths = Paths::discover()?;
    let operation = match cli.command {
        Command::DaemonRun => {
            daemon::run(paths).await?;
            return Ok(json!({"ok":true}));
        }
        Command::Install { no_path } => {
            opencode_scheduler::install::install(&paths, !no_path).await?;
            if !cli.json && !no_path {
                eprintln!(
                    "Global command installed. Open a new terminal to use opencode-scheduler."
                );
            }
            return match runtime::discover_daemon(&paths).await? {
                Some(endpoint) => runtime::rpc(&endpoint, &Operation::Status(EmptyInput {})).await,
                None => Ok(
                    json!({"ok":true,"result":{"kind":"status","data":runtime::absent_status(&paths)}}),
                ),
            };
        }
        Command::Status
        | Command::Daemon {
            command: DaemonCommand::Status,
        } => {
            if !paths.root.exists() {
                return Ok(
                    json!({"ok":true,"result":{"kind":"status","data":runtime::observed_absent_status(&paths).await}}),
                );
            }
            return match runtime::discover_daemon(&paths).await? {
                Some(endpoint) => runtime::rpc(&endpoint, &Operation::Status(EmptyInput {})).await,
                None => Ok(
                    json!({"ok":true,"result":{"kind":"status","data":runtime::observed_absent_status(&paths).await}}),
                ),
            };
        }
        Command::Daemon {
            command: DaemonCommand::Start,
        } => {
            let endpoint = runtime::ensure_daemon(&paths).await?;
            return runtime::rpc(&endpoint, &Operation::Status(EmptyInput {})).await;
        }
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => {
            if !paths.root.exists() {
                return Ok(
                    json!({"ok":true,"result":{"kind":"stopping","data":{"accepted":true}}}),
                );
            }
            paths.prepare()?;
            let Some(endpoint) = runtime::discover_daemon(&paths).await? else {
                return Err(ApiError::coded("daemon_unavailable"));
            };
            let response = runtime::rpc(&endpoint, &Operation::DaemonStop(EmptyInput {})).await?;
            if response["ok"] == true {
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        if !paths.file("endpoint.json").exists() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await
                .map_err(|_| ApiError::coded("daemon_unavailable"))?;
            }
            return Ok(response);
        }
        Command::Schedule { command } => match command {
            ScheduleCommand::Create {
                session,
                session_title,
                at,
                prompt,
                delivery,
                request_id,
            } => {
                let request_id = request_id.unwrap_or_else(|| Uuid::new_v4().to_string());
                validate_uuid(&request_id)?;
                eprintln!(
                    "request_id: {request_id} (reuse with identical input if the response is lost)"
                );
                Operation::ScheduleCreate(CreateInput {
                    request_id,
                    target: match (session, session_title) {
                        (Some(id), None) => TargetSelector::Id(id),
                        (None, Some(title)) => TargetSelector::Title(title),
                        _ => return Err(ApiError::invalid()),
                    },
                    at,
                    prompt,
                    delivery: if delivery == "queue" {
                        DeliveryMode::Queue
                    } else {
                        DeliveryMode::Steer
                    },
                })
            }
            ScheduleCommand::List { page } => Operation::ScheduleList(PageInput {
                limit: page.limit,
                cursor: page.cursor,
            }),
            ScheduleCommand::Show { id } => Operation::ScheduleShow(IdInput { id }),
            ScheduleCommand::Cancel { id } => Operation::ScheduleCancel(IdInput { id }),
            ScheduleCommand::Log { id, page } => Operation::ScheduleLog(LogInput {
                id,
                limit: page.limit,
                cursor: page.cursor,
            }),
        },
    };
    if let Operation::ScheduleCreate(input) = &operation {
        input.validate()?;
    }
    let endpoint = runtime::ensure_daemon(&paths).await?;
    runtime::rpc(&endpoint, &operation).await
}
fn main() {
    #[cfg(windows)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().as_deref()
            == Some(std::ffi::OsStr::new("--internal-prepare-private-directory"))
        {
            let root = args.next();
            let success = root.is_some()
                && args.next().is_none()
                && opencode_scheduler::windows_private::prepare(std::path::Path::new(
                    &root.unwrap(),
                ))
                .is_ok();
            std::process::exit(if success { 0 } else { 1 });
        }
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Cannot create scheduler runtime")
        .block_on(run());
}

async fn run() {
    let cli = Cli::parse();
    let json_output = cli.json;
    let daemon_run = matches!(&cli.command, Command::DaemonRun);
    let mut value = match execute(cli).await {
        Ok(value) => value,
        Err(error) => {
            json!({"ok":false,"error":error.sanitized().unwrap_or_else(|_|ApiError::storage())})
        }
    };
    if value["ok"] == false {
        let error = serde_json::from_value::<ApiError>(value["error"].clone())
            .ok()
            .and_then(|error| error.sanitized().ok())
            .unwrap_or_else(ApiError::storage);
        value["error"] = json!(error);
    }
    let ok = value["ok"] == true;
    if daemon_run && ok && !json_output {
        return;
    }
    if json_output {
        println!("{value}")
    } else if !ok {
        eprintln!(
            "{}: {}",
            value["error"]["code"].as_str().unwrap_or("storage_failure"),
            value["error"]["message"]
                .as_str()
                .unwrap_or("Scheduler operation failed.")
        );
    } else {
        match opencode_scheduler::output::render(&value) {
            Ok(text) => print!("{text}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if !ok {
        std::process::exit(1)
    }
}
