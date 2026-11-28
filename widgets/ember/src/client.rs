//! Short-lived socket requests let any reopened UI control the same service.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::model::{Command, MAX_STATE_BYTES, PROTOCOL_VERSION, Request, Response, View};

pub fn default_root() -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or("ユーザーのデータ保存先を確認できません")?;
    // A platform data directory may contain a system alias (for example /var
    // on macOS); resolve that existing prefix before the link-refusing store.
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    Ok(base
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("noa")
        .join("ember"))
}

pub fn request_id() -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    format!(
        "req-{}-{}-{}",
        crate::model::now(),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn socket_path(root: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("保存先には絶対パスが必要です".into());
    }
    let base = std::env::temp_dir()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no arguments and does not access user memory.
    let runtime_name = format!("noa-ember-{}", unsafe { libc::geteuid() });
    let mut runtime = base.join(&runtime_name);
    if runtime.as_os_str().len() + 30 > 103 {
        runtime = PathBuf::from("/tmp")
            .canonicalize()
            .map_err(|_| "短いユーザー専用のTMPDIRを設定してください")?
            .join(runtime_name);
    }
    crate::store::private_directory(&runtime)?;
    let hash = crate::store::digest(root.as_os_str().as_encoded_bytes());
    Ok(runtime.join(format!("{}.sock", &hash[..24])))
}

#[derive(Clone, Debug)]
pub struct Client {
    root: PathBuf,
    socket: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ClientError {
    pub message: String,
    pub outcome_unknown: bool,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl Client {
    pub fn connect(root: PathBuf) -> Result<Self, String> {
        let socket = socket_path(&root)?;
        let client = Self { root, socket };
        client.refresh()?;
        Ok(client)
    }

    pub fn connect_or_start(root: PathBuf, executable: &Path) -> Result<Self, String> {
        let client = Self {
            socket: socket_path(&root)?,
            root,
        };
        if client.view().is_ok() {
            client.refresh()?;
            return Ok(client);
        }
        let mut child = ProcessCommand::new(executable)
            .arg("--service")
            .arg(format!("--data-dir={}", client.root.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("エンバーを起動できません: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            if client.view().is_ok() {
                // Keep reaping the helper after this UI exits; neither waiting
                // here nor dropping Child should block an active run.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(client);
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if client.view().is_ok() {
                    return Ok(client);
                }
                if status.success() {
                    // A concurrent helper may have lost the lifetime lease
                    // before its winner finishes loading state and binding.
                    std::thread::sleep(Duration::from_millis(80));
                    continue;
                }
                return Err(format!(
                    "エンバーを開始できませんでした（{status}）。保存データや権限を確認してください"
                ));
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Err("エンバーの起動確認がタイムアウトしました。再接続してください".into())
    }

    fn call(&self, request: &Request) -> Result<Response, String> {
        self.call_checked(request).map_err(|e| e.message)
    }

    fn call_checked(&self, request: &Request) -> Result<Response, ClientError> {
        let stream = UnixStream::connect(&self.socket).map_err(|_| ClientError {
            message: "エンバーへ接続できません。再接続してください".into(),
            outcome_unknown: false,
        })?;
        Self::exchange(stream, request)
    }

    fn exchange(mut stream: UnixStream, request: &Request) -> Result<Response, ClientError> {
        let refused = |message: String| ClientError {
            message,
            outcome_unknown: false,
        };
        let unknown = |message: String| ClientError {
            message,
            outcome_unknown: matches!(request, Request::Command { .. }),
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|e| refused(e.to_string()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| refused(e.to_string()))?;
        let bytes = serde_json::to_vec(request).map_err(|e| refused(e.to_string()))?;
        if bytes.len() > 1024 * 1024 {
            return Err(refused("送信内容が大きすぎます".into()));
        }
        stream.write_all(&bytes).map_err(|_| {
            unknown("操作の受け付けを確認できません。同じ受付番号で再確認してください".into())
        })?;
        stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|e| unknown(e.to_string()))?;
        let mut bytes = Vec::new();
        stream
            .take((MAX_STATE_BYTES * 2 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                unknown("操作の結果を確認できません。同じ受付番号で再確認してください".into())
            })?;
        if bytes.len() > MAX_STATE_BYTES * 2 {
            return Err(unknown("応答が容量上限を超えています".into()));
        }
        let response: Response = serde_json::from_slice(&bytes)
            .map_err(|_| unknown("エンバーの応答を確認できません".into()))?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(unknown("エンバーの通信バージョンが一致しません".into()));
        }
        if let Some(error) = response.error {
            return Err(ClientError {
                message: error,
                outcome_unknown: response.outcome_unknown,
            });
        }
        Ok(response)
    }

    pub fn view(&self) -> Result<View, String> {
        self.call(&Request::View {
            protocol_version: PROTOCOL_VERSION,
        })?
        .view
        .ok_or("状態が応答にありません".into())
    }

    pub fn refresh(&self) -> Result<View, String> {
        self.call(&Request::RefreshBackend {
            protocol_version: PROTOCOL_VERSION,
        })?
        .view
        .ok_or("状態が応答にありません".into())
    }

    pub fn command(&self, intent: Command) -> Result<View, String> {
        self.request(&request_id(), intent)
    }

    pub fn request(&self, request_id: &str, intent: Command) -> Result<View, String> {
        self.request_checked(request_id, intent)
            .map_err(|e| e.message)
    }

    pub fn request_checked(&self, request_id: &str, intent: Command) -> Result<View, ClientError> {
        self.call_checked(&Request::Command {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            intent,
        })?
        .view
        .ok_or(ClientError {
            message: "状態が応答にありません".into(),
            outcome_unknown: true,
        })
    }

    pub fn artifact(&self, run_id: &str) -> Result<String, String> {
        self.call(&Request::Artifact {
            protocol_version: PROTOCOL_VERSION,
            run_id: run_id.into(),
        })?
        .text
        .ok_or("成果物が応答にありません".into())
    }

    pub fn export(&self) -> Result<String, String> {
        self.call(&Request::Export {
            protocol_version: PROTOCOL_VERSION,
        })?
        .text
        .ok_or("履歴が応答にありません".into())
    }

    pub fn prepare_handoff(&self, run_id: &str) -> Result<crate::handoff::PreparedHandoff, String> {
        let text = self
            .call(&Request::PrepareHandoff {
                protocol_version: PROTOCOL_VERSION,
                run_id: run_id.into(),
            })?
            .text
            .ok_or("引き継ぎ内容が応答にありません")?;
        serde_json::from_str(&text).map_err(|_| "引き継ぎ内容を確認できません".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, BackendError, Completion, CompletionRequest};
    use std::sync::{Arc, atomic::AtomicBool};

    struct NoDispatch;
    impl Backend for NoDispatch {
        fn complete(
            &self,
            _: &CompletionRequest,
            _: &AtomicBool,
        ) -> Result<Completion, BackendError> {
            panic!("viewing or acknowledging an existing request must not call a provider")
        }
    }

    #[test]
    fn a_lost_ack_is_uncertain_and_a_new_transport_can_resend_the_same_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (first_client, first_server) = UnixStream::pair().unwrap();
        let (second_client, second_server) = UnixStream::pair().unwrap();
        let server_root = root;
        let worker = std::thread::spawn(move || {
            let mut service =
                crate::service::Service::open(server_root, Arc::new(NoDispatch), true).unwrap();
            for (attempt, mut stream) in [first_server, second_server].into_iter().enumerate() {
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).unwrap();
                let Request::Command {
                    request_id, intent, ..
                } = serde_json::from_slice(&bytes).unwrap()
                else {
                    panic!("expected command")
                };
                let revision = service.command(&request_id, intent).unwrap();
                if attempt == 1 {
                    let response = Response {
                        protocol_version: PROTOCOL_VERSION,
                        accepted_revision: Some(revision),
                        view: Some(service.view()),
                        text: None,
                        error: None,
                        outcome_unknown: false,
                    };
                    stream
                        .write_all(&serde_json::to_vec(&response).unwrap())
                        .unwrap();
                }
            }
            assert_eq!(service.view().state.runs.len(), 1);
        });
        let intent = Command::Send {
            text: "One message".into(),
            scope: crate::model::Scope::Global,
            attachments: vec![],
        };
        let request = Request::Command {
            protocol_version: PROTOCOL_VERSION,
            request_id: "same-request".into(),
            intent,
        };
        let error = Client::exchange(first_client, &request).unwrap_err();
        assert!(error.outcome_unknown);
        let view = Client::exchange(second_client, &request)
            .unwrap()
            .view
            .unwrap();
        assert_eq!(view.state.runs.len(), 1);
        worker.join().unwrap();
    }

    #[test]
    fn failure_before_connecting_is_known_not_to_have_executed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let client = Client {
            socket: socket_path(&root).unwrap(),
            root,
        };
        let error = client
            .request_checked(
                "not-sent",
                Command::UpdateSettings {
                    settings: crate::model::Settings::default(),
                },
            )
            .unwrap_err();
        assert!(!error.outcome_unknown);
    }
}
