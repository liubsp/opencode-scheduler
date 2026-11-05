use clap::{Args, Parser, Subcommand};
use opencode_scheduler::{
    adapter::unavailable,
    daemon,
    runtime::{self, Paths},
    types::*,
};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Durable one-shot prompt scheduling for OpenCode V2")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Status,
    Install,
    #[command(hide = true)]
    DaemonRun,
}
#[derive(Subcommand)]
enum DaemonCommand {
    Start,
    Status,
    Stop,
}
#[derive(Args)]
struct Paging {
    #[arg(long, default_value_t = 50)]
    limit: u16,
    #[arg(long)]
    cursor: Option<String>,
}
#[derive(Subcommand)]
enum ScheduleCommand {
    Create {
        #[arg(
            long,
            required_unless_present = "session_title",
            conflicts_with = "session_title"
        )]
        session: Option<String>,
        #[arg(long, required_unless_present = "session", conflicts_with = "session")]
        session_title: Option<String>,
        #[arg(long)]
        at: String,
        #[arg(long)]
        prompt: String,
        #[arg(long,default_value="steer",value_parser=["steer","queue"])]
        delivery: String,
        #[arg(long)]
        request_id: Option<String>,
    },
    List {
        #[command(flatten)]
        page: Paging,
    },
    Show {
        id: String,
    },
    Cancel {
        id: String,
    },
    Log {
        id: String,
        #[command(flatten)]
        page: Paging,
    },
}
async fn execute(cli: Cli) -> Result<Value, ApiError> {
    let paths = Paths::discover()?;
    let operation = match cli.command {
        Command::DaemonRun => {
            daemon::run(paths).await?;
            return Ok(json!({"ok":true}));
        }
        Command::Install => {
            runtime::install(&paths)?;
            return Ok(
                json!({"ok":true,"result":{"kind":"status","data":runtime::absent_status(&paths)}}),
            );
        }
        Command::Status
        | Command::Daemon {
            command: DaemonCommand::Status,
        } => {
            if !paths.root.exists() {
                return Ok(
                    json!({"ok":true,"result":{"kind":"status","data":runtime::absent_status(&paths)}}),
                );
            }
            paths.prepare()?;
            return match runtime::discover_daemon(&paths).await? {
                Some(endpoint) => runtime::rpc(&endpoint, &Operation::Status(EmptyInput {})).await,
                None => Ok(
                    json!({"ok":true,"result":{"kind":"status","data":runtime::absent_status(&paths)}}),
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
                return Err(unavailable());
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
                .map_err(|_| unavailable())?;
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
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json_output = cli.json;
    let value = match execute(cli).await {
        Ok(value) => value,
        Err(error) => {
            json!({"ok":false,"error":error.sanitized().unwrap_or_else(|_|ApiError::storage())})
        }
    };
    let ok = value["ok"] == true;
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
    } else if value["result"]["kind"] == "created" {
        let data = &value["result"]["data"];
        let schedules = data["schedules"].as_array().cloned().unwrap_or_default();
        println!(
            "Created {} schedule(s); request {}",
            schedules.len(),
            data["request_id"].as_str().unwrap_or_default()
        );
        for s in schedules {
            println!(
                "{}  {}  {}  {}",
                s["id"].as_str().unwrap_or_default(),
                s["target"]["session_id"].as_str().unwrap_or_default(),
                s["time"]["display"].as_str().unwrap_or_default(),
                s["delivery"].as_str().unwrap_or_default()
            );
        }
        println!(
            "Six attempts maximum; retries after 10/20/40/80/160 seconds. Overdue work runs when the daemon resumes. Admission is not agent completion; unconfirmed means it may have been accepted."
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value["result"]["data"]).unwrap_or_default()
        );
    }
    if !ok {
        std::process::exit(1)
    }
}
