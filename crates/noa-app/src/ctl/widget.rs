//! Repository widget commands. Validation and fixture previews are deliberately
//! dispatched before connecting to the control server.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use noa_ipc::widget::{
    AckParams, CloseParams, DescribeParams, EventsParams, InstanceParams, ListParams,
    MAX_EVENT_PAGE, OpenParams, OperationState, ReportParams, SetParams, WidgetCall,
};
use noa_ipc::{Client, ClientError, WireId};
use noa_widget::{Asset, MAX_DATA_BYTES, MAX_MANIFEST_BYTES, Manifest, assets, definition_digest};
use serde_json::{Value, json};

use super::{Args, EXIT_ERROR, EXIT_OK, Failure, parse_u64, print_json};

const HELP: &str = "Repository widgets (JSON output):
  widget schema
  widget validate FILE [--data-file FILE]
  widget preview FILE [--data-file FILE] [--snapshot FILE.png] [--size WxH]
  widget list [--repo DIR]
  widget describe ID [--repo DIR]
  widget open [ID | --definition-file FILE] [--repo DIR] [--data-file FILE]
              --request-id ID
  widget get INSTANCE
  widget set INSTANCE --data-file FILE --expected-revision N --request-id ID
  widget events INSTANCE [--consumer ID] [--after N] [--limit N] [--wait SECONDS]
  widget ack INSTANCE --through N [--consumer ID] --request-id ID
  widget report INSTANCE --event ID --expected-version N --state STATE
                [--consumer ID] [--message TEXT] --request-id ID
  widget close INSTANCE --request-id ID
  widget forget INSTANCE

FILE may be - for JSON on stdin, except in preview. A consumer id is required
outside a Noa pane. Events remain pending until acknowledged; process them in
order. Mutations require a stable request id: reuse it only for identical retries.
Forget removes a stopped, fully acknowledged instance and its retry history;
it is idempotent by instance id and does not need a separate request id.
Validation and preview work with the control server disabled.
Report states: running, waiting, completed, failed. Only the registered event
consumer may report a delivered event. A queue ack is never processing completion.
";

const INLINE_ASSETS: &str = "a definition with assets cannot be opened with --definition-file: \
     commit it as .noa-widgets/<id>/ and open it by id, so its images come from the package";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WidgetCommand {
    Help,
    Schema,
    Validate {
        file: PathBuf,
        data: Option<PathBuf>,
    },
    Preview {
        file: PathBuf,
        data: Option<PathBuf>,
        snapshot: Option<PathBuf>,
        size: Option<String>,
    },
    List {
        repo: PathBuf,
    },
    Describe {
        repo: PathBuf,
        id: String,
    },
    Open {
        repo: PathBuf,
        id: Option<String>,
        definition: Option<PathBuf>,
        data: Option<PathBuf>,
        request_id: String,
    },
    Get {
        instance: String,
    },
    Set {
        instance: String,
        data: PathBuf,
        revision: u64,
        request_id: String,
    },
    Events {
        instance: String,
        consumer: Option<String>,
        after: u64,
        limit: usize,
        wait: Duration,
    },
    Ack {
        instance: String,
        consumer: Option<String>,
        through: u64,
        request_id: String,
    },
    Report {
        instance: String,
        consumer: Option<String>,
        event: String,
        version: u64,
        state: OperationState,
        message: String,
        request_id: String,
    },
    Close {
        instance: String,
        request_id: String,
    },
    Forget {
        instance: String,
    },
}

impl WidgetCommand {
    pub(crate) fn needs_own_pane(&self) -> bool {
        matches!(
            self,
            Self::Open { .. }
                | Self::Events { consumer: None, .. }
                | Self::Ack { consumer: None, .. }
                | Self::Report { consumer: None, .. }
        )
    }

    pub(crate) fn is_local(&self) -> bool {
        matches!(
            self,
            Self::Help | Self::Schema | Self::Validate { .. } | Self::Preview { .. }
        )
    }
}

fn required(args: &mut Args, name: &str) -> Result<String, String> {
    args.value(name)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

fn one(args: &mut Args, what: &str) -> Result<String, String> {
    let positional = args.positionals()?;
    match positional.as_slice() {
        [value] if !value.is_empty() => Ok(value.clone()),
        _ => Err(format!("expected exactly one {what}")),
    }
}

fn repo(args: &mut Args) -> Result<PathBuf, String> {
    let value = args.value("--repo")?.unwrap_or_else(|| ".".into());
    if value.is_empty() {
        return Err("--repo must not be empty".into());
    }
    Ok(value.into())
}

pub(crate) fn parse(args: &[String]) -> Result<WidgetCommand, String> {
    let (sub, tail) = args
        .split_first()
        .ok_or("missing widget subcommand (use widget --help)")?;
    let mut args = Args::new(tail);
    let command = match sub.as_str() {
        "help" | "--help" | "-h" => WidgetCommand::Help,
        "schema" => WidgetCommand::Schema,
        "validate" => {
            let data = args.value("--data-file")?.map(PathBuf::from);
            let file = PathBuf::from(one(&mut args, "manifest file")?);
            reject_two_stdin(&file, data.as_deref())?;
            WidgetCommand::Validate { file, data }
        }
        "preview" => {
            let data = args.value("--data-file")?.map(PathBuf::from);
            let snapshot = args.value("--snapshot")?.map(PathBuf::from);
            let size = args.value("--size")?;
            let file = PathBuf::from(one(&mut args, "manifest file")?);
            if file == Path::new("-") || data.as_deref() == Some(Path::new("-")) {
                return Err("preview needs files; save stdin to a local draft first".into());
            }
            if let Some(size) = &size {
                validate_size(size)?;
            }
            if size.is_some() && snapshot.is_none() {
                return Err("--size requires --snapshot".into());
            }
            WidgetCommand::Preview {
                file,
                data,
                snapshot,
                size,
            }
        }
        "list" => WidgetCommand::List {
            repo: repo(&mut args)?,
        },
        "describe" => {
            let repo = repo(&mut args)?;
            WidgetCommand::Describe {
                repo,
                id: one(&mut args, "widget id")?,
            }
        }
        "open" => {
            let repo = repo(&mut args)?;
            let definition = args.value("--definition-file")?.map(PathBuf::from);
            let data = args.value("--data-file")?.map(PathBuf::from);
            let request_id = required(&mut args, "--request-id")?;
            let ids = args.positionals()?;
            let id = match (ids.as_slice(), &definition) {
                ([id], None) => Some(id.clone()),
                ([], Some(_)) => None,
                _ => return Err("open needs either a widget id or --definition-file FILE".into()),
            };
            if let Some(file) = &definition {
                reject_two_stdin(file, data.as_deref())?;
            }
            WidgetCommand::Open {
                repo,
                id,
                definition,
                data,
                request_id,
            }
        }
        "get" => WidgetCommand::Get {
            instance: one(&mut args, "instance id")?,
        },
        "set" => {
            let data = required(&mut args, "--data-file")?.into();
            let revision = parse_u64(
                "--expected-revision",
                &required(&mut args, "--expected-revision")?,
            )?;
            let request_id = required(&mut args, "--request-id")?;
            WidgetCommand::Set {
                instance: one(&mut args, "instance id")?,
                data,
                revision,
                request_id,
            }
        }
        "events" => {
            let consumer = args.value("--consumer")?;
            let after = args
                .value("--after")?
                .map(|v| parse_u64("--after", &v))
                .transpose()?
                .unwrap_or(0);
            let limit = args
                .value("--limit")?
                .map(|v| parse_u64("--limit", &v))
                .transpose()?
                .unwrap_or(32);
            if limit == 0 || limit > MAX_EVENT_PAGE as u64 {
                return Err(format!("--limit must be 1..{MAX_EVENT_PAGE}"));
            }
            let seconds = args
                .value("--wait")?
                .map(|v| parse_u64("--wait", v.strip_suffix('s').unwrap_or(&v)))
                .transpose()?
                .unwrap_or(0);
            if seconds > 300 {
                return Err("--wait must be 0..300 seconds".into());
            }
            WidgetCommand::Events {
                instance: one(&mut args, "instance id")?,
                consumer,
                after,
                limit: limit as usize,
                wait: Duration::from_secs(seconds),
            }
        }
        "ack" => {
            let consumer = args.value("--consumer")?;
            let through = parse_u64("--through", &required(&mut args, "--through")?)?;
            let request_id = required(&mut args, "--request-id")?;
            WidgetCommand::Ack {
                instance: one(&mut args, "instance id")?,
                consumer,
                through,
                request_id,
            }
        }
        "report" => {
            let consumer = args.value("--consumer")?;
            let event = required(&mut args, "--event")?;
            let version = parse_u64(
                "--expected-version",
                &required(&mut args, "--expected-version")?,
            )?;
            if version == 0 {
                return Err("--expected-version must be positive".into());
            }
            let state = match required(&mut args, "--state")?.as_str() {
                "running" => OperationState::Running,
                "waiting" => OperationState::Waiting,
                "completed" => OperationState::Completed,
                "failed" => OperationState::Failed,
                _ => return Err("--state needs running, waiting, completed or failed".into()),
            };
            let message = args.value("--message")?.unwrap_or_default();
            let request_id = required(&mut args, "--request-id")?;
            WidgetCommand::Report {
                instance: one(&mut args, "instance id")?,
                consumer,
                event,
                version,
                state,
                message,
                request_id,
            }
        }
        "close" => {
            let request_id = required(&mut args, "--request-id")?;
            WidgetCommand::Close {
                instance: one(&mut args, "instance id")?,
                request_id,
            }
        }
        "forget" => WidgetCommand::Forget {
            instance: one(&mut args, "instance id")?,
        },
        _ => return Err(format!("unknown widget subcommand {sub:?}")),
    };
    args.finish()?;
    Ok(command)
}

fn reject_two_stdin(file: &Path, data: Option<&Path>) -> Result<(), String> {
    if file == Path::new("-") && data == Some(Path::new("-")) {
        Err("manifest and data cannot both read stdin".into())
    } else {
        Ok(())
    }
}

fn validate_size(size: &str) -> Result<(), String> {
    let valid =
        size.split_once('x')
            .is_some_and(|(w, h)| match (w.parse::<u32>(), h.parse::<u32>()) {
                (Ok(w), Ok(h)) => {
                    (320..=4096).contains(&w)
                        && (320..=4096).contains(&h)
                        && u64::from(w) * u64::from(h) <= 8_000_000
                }
                _ => false,
            });
    if valid {
        Ok(())
    } else {
        Err("--size needs WxH, each dimension 320..4096, at most 8 million logical pixels".into())
    }
}

fn read_file(path: &Path, cap: usize) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::error(format!("read stdin: {e}")))?;
    } else {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| Failure::error(format!("read {}: {e}", path.display())))?;
        if !file
            .metadata()
            .map_err(|e| Failure::error(e.to_string()))?
            .is_file()
        {
            return Err(Failure::error("JSON input must be a regular file"));
        }
        file.take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::error(e.to_string()))?;
    }
    if bytes.len() > cap {
        return Err(Failure::error(format!(
            "{} exceeds {cap} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn read_data(path: &Path) -> Result<Value, Failure> {
    serde_json::from_slice(&read_file(path, MAX_DATA_BYTES)?)
        .map_err(|e| Failure::error(format!("invalid JSON data: {e}")))
}

fn read_definition(path: &Path) -> Result<Manifest, Failure> {
    Manifest::from_slice(&read_file(path, MAX_MANIFEST_BYTES)?)
        .map_err(|e| Failure::error(e.to_string()))
}

pub(crate) fn execute_local(command: WidgetCommand) -> Result<i32, Failure> {
    match command {
        WidgetCommand::Help => print!("{HELP}"),
        WidgetCommand::Schema => print_json(&noa_widget::schema_document()),
        WidgetCommand::Validate { file, data } => {
            let validated = read_definition(&file).and_then(|manifest| {
                if let Some(data) = data {
                    manifest
                        .validate_data(&read_data(&data)?)
                        .map_err(|e| Failure::error(e.to_string()))?;
                }
                Ok(manifest)
            });
            let validated = validated.and_then(|manifest| {
                let assets = package_assets(&file, &manifest)?;
                let digest = definition_digest(&manifest, &assets::digests(&assets))
                    .map_err(|e| Failure::error(e.to_string()))?;
                Ok((manifest, assets, digest))
            });
            match validated {
                Ok((manifest, assets, digest)) if assets.is_empty() => print_json(&json!({
                    "valid": true, "id": manifest.id, "definitionDigest": digest
                })),
                Ok((manifest, assets, digest)) => print_json(&json!({
                    "valid": true, "id": manifest.id, "definitionDigest": digest,
                    "assets": assets.iter().map(|a| json!({
                        "id": a.id, "format": a.format, "width": a.width, "height": a.height,
                        "bytes": a.bytes.len(), "digest": a.digest
                    })).collect::<Vec<_>>()
                })),
                Err(error) => {
                    print_json(&json!({"valid": false, "errors": [{
                        "code": "INVALID_DEFINITION", "message": error.message
                    }]}));
                    return Ok(EXIT_ERROR);
                }
            }
        }
        WidgetCommand::Preview {
            file,
            data,
            snapshot,
            size,
        } => {
            let manifest = read_definition(&file)?;
            if let Some(data) = &data {
                manifest
                    .validate_data(&read_data(data)?)
                    .map_err(|e| Failure::error(e.to_string()))?;
            }
            // Report asset problems here; the helper reads the same files.
            package_assets(&file, &manifest)?;
            let file = absolute(&file)?;
            let mut args = vec![format!("--manifest={}", file.display())];
            if let Some(data) = data {
                args.push(format!("--data={}", absolute(&data)?.display()));
            }
            if let Some(snapshot) = snapshot {
                let snapshot = absolute(&snapshot)?;
                args.push(format!("--snapshot={}", snapshot.display()));
                if let Some(size) = size {
                    args.push(format!("--size={size}"));
                }
                let path = crate::helper_program::program_path(crate::helper_program::WIDGET_HOST)
                    .map_err(Failure::unavailable)?;
                let status = ProcessCommand::new(path)
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .status()
                    .map_err(|e| Failure::error(e.to_string()))?;
                if !status.success() {
                    return Err(Failure::error(format!("widget preview failed: {status}")));
                }
                print_json(&json!({"preview": true, "snapshot": snapshot}));
            } else {
                crate::helper_program::open(crate::helper_program::WIDGET_HOST, &args, |error| {
                    eprintln!("Noa widget preview: {error}");
                })
                .map_err(Failure::unavailable)?;
                print_json(&json!({"preview": true, "opened": true}));
            }
        }
        _ => {
            return Err(Failure::error(
                "this widget command needs the control server",
            ));
        }
    }
    Ok(EXIT_OK)
}

/// Assets live beside the manifest in its package directory. Stdin has no
/// package directory, so a definition with assets cannot come from it.
fn package_assets(file: &Path, manifest: &Manifest) -> Result<Vec<Asset>, Failure> {
    if manifest.assets.is_none() {
        return Ok(Vec::new());
    }
    if file == Path::new("-") {
        return Err(Failure::error(
            "a definition with assets must be read from its package's widget.json, not stdin",
        ));
    }
    let file = absolute(file)?;
    let package = file
        .parent()
        .ok_or_else(|| Failure::error("manifest file has no package directory"))?;
    assets::load(package, manifest).map_err(|e| Failure::error(e.to_string()))
}

fn absolute(path: &Path) -> Result<PathBuf, Failure> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| Failure::error(e.to_string()))
    }
}

fn repo_string(path: &Path) -> Result<String, Failure> {
    let context = noa_widget::discovery::repo_context(&absolute(path)?)
        .map_err(|e| Failure::error(e.to_string()))?;
    context
        .worktree_root
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure::error("repository path must be UTF-8"))
}

fn consumer_id(value: Option<String>, own: Option<u64>) -> Result<String, Failure> {
    value
        .or_else(|| own.map(|id| format!("pane:{id}")))
        .ok_or_else(|| {
            Failure::error(
                "--consumer ID is required outside a Noa pane; reuse it after reconnecting",
            )
        })
}

pub(crate) fn execute(
    client: &mut Client,
    own: Option<u64>,
    command: WidgetCommand,
) -> Result<i32, Failure> {
    if !client.supports_repo_widgets() {
        return Err(Failure::unavailable(
            "this Noa server does not support repository widgets",
        ));
    }
    let mut wait = Duration::ZERO;
    let call = match command {
        WidgetCommand::List { repo } => WidgetCall::List(ListParams {
            repo: repo_string(&repo)?,
        }),
        WidgetCommand::Describe { repo, id } => WidgetCall::Describe(DescribeParams {
            repo: repo_string(&repo)?,
            id,
        }),
        WidgetCommand::Open {
            repo,
            id,
            definition,
            data,
            request_id,
        } => {
            let definition = definition
                .map(|path| {
                    let manifest = read_definition(&path)?;
                    if manifest.assets.is_some() {
                        return Err(Failure::error(INLINE_ASSETS));
                    }
                    serde_json::to_value(manifest).map_err(|e| Failure::error(e.to_string()))
                })
                .transpose()?;
            WidgetCall::Open(OpenParams {
                repo: repo_string(&repo)?,
                id,
                definition,
                data: data.map(|path| read_data(&path)).transpose()?,
                request_id,
                owner_pane_id: own.map(WireId),
            })
        }
        WidgetCommand::Get { instance } => WidgetCall::Get(InstanceParams {
            instance_id: instance,
        }),
        WidgetCommand::Set {
            instance,
            data,
            revision,
            request_id,
        } => WidgetCall::Set(SetParams {
            instance_id: instance,
            expected_revision: revision,
            data: read_data(&data)?,
            request_id,
        }),
        WidgetCommand::Events {
            instance,
            consumer,
            after,
            limit,
            wait: timeout,
        } => {
            wait = timeout;
            WidgetCall::Events(EventsParams {
                instance_id: instance,
                consumer_id: consumer_id(consumer, own)?,
                after_seq: after,
                limit,
            })
        }
        WidgetCommand::Ack {
            instance,
            consumer,
            through,
            request_id,
        } => WidgetCall::Ack(AckParams {
            instance_id: instance,
            consumer_id: consumer_id(consumer, own)?,
            through_seq: through,
            request_id,
        }),
        WidgetCommand::Report {
            instance,
            consumer,
            event,
            version,
            state,
            message,
            request_id,
        } => {
            if !client.has_capability(noa_ipc::widget::CAPABILITY_WIDGET_OPERATIONS_V1) {
                return Err(Failure::unavailable(
                    "this Noa server does not support widget operation reports",
                ));
            }
            WidgetCall::Report(ReportParams {
                instance_id: instance,
                consumer_id: consumer_id(consumer, own)?,
                event_id: event,
                expected_version: version,
                state,
                message,
                request_id,
            })
        }
        WidgetCommand::Close {
            instance,
            request_id,
        } => WidgetCall::Close(CloseParams {
            instance_id: instance,
            request_id,
        }),
        WidgetCommand::Forget { instance } => WidgetCall::Forget(InstanceParams {
            instance_id: instance,
        }),
        local => return execute_local(local),
    };
    call.validate().map_err(|e| Failure::error(e.to_string()))?;
    let deadline = Instant::now() + wait;
    loop {
        let result = client
            .widget(&call)
            .map_err(|error| call_failure(error, call.request_id()))?;
        if wait.is_zero()
            || result["events"]
                .as_array()
                .is_none_or(|events| !events.is_empty())
            || result["status"]
                .as_str()
                .is_some_and(|status| status != "open")
            || Instant::now() >= deadline
        {
            print_json(&result);
            return Ok(EXIT_OK);
        }
        std::thread::sleep(
            Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn call_failure(error: ClientError, request_id: Option<&str>) -> Failure {
    let uncertain = matches!(
        error,
        ClientError::Rpc {
            code: super::RPC_OUTCOME_UNKNOWN,
            ..
        } | ClientError::Timeout(_)
            | ClientError::Transport(_)
            | ClientError::Closed(_)
            | ClientError::Protocol(_)
    );
    if uncertain && let Some(id) = request_id {
        return Failure::unavailable(format!(
            "widget result is unconfirmed ({error}); retry only the identical request with --request-id {id}"
        ));
    }
    Failure::from(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_list(args: &[&str]) -> Result<WidgetCommand, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn offline_commands_are_identified_before_connecting() {
        for args in [
            vec!["schema"],
            vec!["validate", "widget.json"],
            vec!["preview", "widget.json"],
        ] {
            assert!(parse_list(&args).unwrap().is_local());
        }
        assert!(!parse_list(&["list"]).unwrap().is_local());
    }

    #[test]
    fn open_has_one_definition_source_and_a_retry_key() {
        assert!(parse_list(&["open", "status", "--request-id", "open-1"]).is_ok());
        assert!(parse_list(&["open", "--definition-file=-", "--request-id=open-1"]).is_ok());
        for args in [
            vec!["open", "status"],
            vec![
                "open",
                "status",
                "--definition-file",
                "x",
                "--request-id",
                "r",
            ],
            vec![
                "open",
                "--definition-file",
                "-",
                "--data-file",
                "-",
                "--request-id",
                "r",
            ],
        ] {
            assert!(parse_list(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn updates_require_a_revision_and_events_have_bounded_waits() {
        assert!(parse_list(&["set", "wi_x", "--data-file", "-", "--request-id", "r"]).is_err());
        assert!(parse_list(&["events", "wi_x", "--wait", "30s", "--limit", "64"]).is_ok());
        assert!(parse_list(&["events", "wi_x", "--wait", "301"]).is_err());
        assert!(parse_list(&["events", "wi_x", "--limit", "65"]).is_err());
        assert!(consumer_id(None, None).is_err());
        assert_eq!(consumer_id(None, Some(42)).unwrap(), "pane:42");
        assert_eq!(
            parse_list(&["forget", "wi_x"]).unwrap(),
            WidgetCommand::Forget {
                instance: "wi_x".into()
            }
        );
    }

    #[test]
    fn preview_rejects_unrepeatable_stdin_and_invalid_sizes() {
        assert!(parse_list(&["preview", "-"]).is_err());
        assert!(parse_list(&["preview", "w.json", "--size", "800x600"]).is_err());
        assert!(
            parse_list(&[
                "preview",
                "w.json",
                "--snapshot",
                "x.png",
                "--size",
                "0x600"
            ])
            .is_err()
        );
        assert!(
            parse_list(&[
                "preview",
                "w.json",
                "--snapshot",
                "x.png",
                "--size",
                "4096x4096"
            ])
            .is_err()
        );
        assert!(
            parse_list(&[
                "preview",
                "w.json",
                "--snapshot",
                "x.png",
                "--size",
                "800x600"
            ])
            .is_ok()
        );
    }

    #[test]
    fn uncertain_writes_keep_the_retry_key_but_confirmed_refusals_do_not() {
        for error in [
            ClientError::Timeout("lost result".into()),
            ClientError::Rpc {
                code: super::super::RPC_OUTCOME_UNKNOWN,
                message: "directory sync failed".into(),
            },
        ] {
            assert!(
                call_failure(error, Some("update-42"))
                    .message
                    .contains("--request-id update-42")
            );
        }
        let conflict = ClientError::Rpc {
            code: noa_ipc::ErrorCode::WidgetConflict.code(),
            message: "revision conflict".into(),
        };
        assert!(
            !call_failure(conflict, Some("update-42"))
                .message
                .contains("retry")
        );
    }

    #[test]
    fn open_and_implicit_consumers_wait_for_the_owning_pane() {
        assert!(
            parse_list(&["open", "status", "--request-id", "r"])
                .unwrap()
                .needs_own_pane()
        );
        assert!(parse_list(&["events", "wi_x"]).unwrap().needs_own_pane());
        assert!(
            !parse_list(&["events", "wi_x", "--consumer", "stable"])
                .unwrap()
                .needs_own_pane()
        );
        assert!(!parse_list(&["get", "wi_x"]).unwrap().needs_own_pane());
    }

    #[test]
    fn assets_are_read_from_the_manifest_package_and_never_from_stdin() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.noa-widgets/image-gallery/widget.json");
        let manifest = read_definition(&file).unwrap();
        let loaded = package_assets(&file, &manifest).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_ne!(
            definition_digest(&manifest, &assets::digests(&loaded)).unwrap(),
            manifest.digest()
        );
        assert!(
            package_assets(Path::new("-"), &manifest)
                .unwrap_err()
                .message
                .contains("not stdin")
        );
        let summary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.noa-widgets/test-summary/widget.json");
        let plain = read_definition(&summary).unwrap();
        assert!(package_assets(Path::new("-"), &plain).unwrap().is_empty());
    }

    #[test]
    fn processing_report_requires_event_version_state_and_a_retry_key() {
        let args = [
            "report",
            "wi_x",
            "--event",
            "we_1",
            "--expected-version",
            "1",
            "--state",
            "running",
            "--request-id",
            "report-1",
        ];
        assert!(parse_list(&args).unwrap().needs_own_pane());
        let mut explicit = args.to_vec();
        explicit.extend(["--consumer", "producer", "--message", "Checking files"]);
        assert!(!parse_list(&explicit).unwrap().needs_own_pane());
        for missing in ["--event", "--expected-version", "--state", "--request-id"] {
            let mut incomplete = args.to_vec();
            let at = incomplete.iter().position(|arg| *arg == missing).unwrap();
            incomplete.drain(at..at + 2);
            assert!(parse_list(&incomplete).is_err(), "{missing}");
        }
        let mut invalid = args.to_vec();
        invalid[7] = "accepted";
        assert!(parse_list(&invalid).is_err());
        invalid[7] = "running";
        invalid[5] = "0";
        assert!(parse_list(&invalid).is_err());
    }
}
