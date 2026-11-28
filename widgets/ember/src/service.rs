use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use crate::backend::{Backend, BackendError, Completion, CompletionRequest, OpenAiBackend};
use crate::model::*;
use crate::{
    client, context, memory,
    store::{self, Lease, Store},
};

struct Finished {
    run_id: String,
    result: Result<Completion, BackendError>,
}

pub struct Service {
    store: Store,
    state: State,
    backend: Arc<dyn Backend>,
    available: bool,
    failed: Option<String>,
    active: HashMap<String, Arc<AtomicBool>>,
    finished_tx: mpsc::Sender<Finished>,
    finished_rx: mpsc::Receiver<Finished>,
}

impl Service {
    /// The caller owns service.lock before constructing this controller.
    pub fn open(root: PathBuf, backend: Arc<dyn Backend>, available: bool) -> Result<Self, String> {
        let store = Store::open(root)?;
        let mut state = store.load()?;
        let mut interrupted = false;
        for run in &mut state.runs {
            if run.status == RunStatus::Queued {
                run.status = RunStatus::Canceled;
                run.error = Some(
                    "前回の未開始の依頼は送信していません。内容を確認して新しく依頼してください"
                        .into(),
                );
                run.updated_at = now();
                interrupted = true;
            } else if matches!(run.status, RunStatus::Running | RunStatus::CancelRequested) {
                run.status = RunStatus::Uncertain;
                run.error = Some("前の実行サービスが終了しました。応答・費用の確定はできないため、自動再送していません".into());
                run.updated_at = now();
                interrupted = true;
            }
        }
        if interrupted {
            let old = state.revision;
            state.revision = old.checked_add(1).ok_or("保存版の上限です")?;
            store.commit(old, &state)?;
        }
        let (finished_tx, finished_rx) = mpsc::channel();
        Ok(Self {
            store,
            state,
            backend,
            available,
            failed: None,
            active: HashMap::new(),
            finished_tx,
            finished_rx,
        })
    }

    pub fn view(&self) -> View {
        View {
            state: self.state.clone(),
            backend_available: self.available,
            notice: self.failed.clone(),
        }
    }

    fn save(&mut self, mut candidate: State) -> Result<(), String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        candidate.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("保存版の上限です")?;
        match self.store.commit(self.state.revision, &candidate) {
            Ok(()) => {
                self.state = candidate;
                Ok(())
            }
            Err(error) => {
                let message = format!("保存できないため新しい実行を停止しました: {error}");
                self.failed = Some(message.clone());
                for canceled in self.active.values() {
                    canceled.store(true, Ordering::Release);
                }
                Err(message)
            }
        }
    }

    pub fn command(&mut self, request_id: &str, intent: Command) -> Result<u64, String> {
        if !valid_id(request_id) {
            return Err("操作の受付番号が不正です".into());
        }
        let digest = store::digest(&serde_json::to_vec(&intent).map_err(|e| e.to_string())?);
        if let Some(receipt) = self
            .state
            .receipts
            .iter()
            .find(|r| r.request_id == request_id)
        {
            return if receipt.digest == digest {
                Ok(receipt.accepted_revision)
            } else {
                Err("同じ受付番号に異なる操作が指定されています".into())
            };
        }
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.state.receipts.len() >= MAX_RECEIPTS {
            return Err(
                "操作記録の上限に達しました。履歴をエクスポートして保存先を移行してください".into(),
            );
        }
        let mut candidate = self.state.clone();
        self.apply(&mut candidate, &intent)?;
        let accepted = self
            .state
            .revision
            .checked_add(1)
            .ok_or("保存版の上限です")?;
        candidate.receipts.push(Receipt {
            request_id: request_id.into(),
            digest,
            accepted_revision: accepted,
        });
        self.save(candidate)?;
        if let Command::CancelRun { run_id } = &intent
            && let Some(canceled) = self.active.get(run_id)
        {
            canceled.store(true, Ordering::Release);
        }
        Ok(accepted)
    }

    fn apply(&self, state: &mut State, intent: &Command) -> Result<(), String> {
        match intent {
            Command::Send {
                text,
                scope,
                attachments,
            } => self.enqueue(state, RunKind::Conversation, text, scope, attachments),
            Command::StartRun {
                kind,
                objective,
                scope,
                attachments,
            } => self.enqueue(state, *kind, objective, scope, attachments),
            Command::CancelRun { run_id } => {
                let run = state
                    .runs
                    .iter_mut()
                    .find(|r| r.id == *run_id)
                    .ok_or("依頼がありません")?;
                run.status = match run.status {
                    RunStatus::Queued => RunStatus::Canceled,
                    RunStatus::Running | RunStatus::CancelRequested => RunStatus::CancelRequested,
                    _ => run.status,
                };
                run.updated_at = now();
                Ok(())
            }
            Command::SaveMemory {
                id,
                expected_revision,
                scope,
                text,
            } => {
                if let Some(id) = id {
                    let entry = state
                        .memories
                        .iter()
                        .find(|m| m.id == *id)
                        .ok_or("記憶がありません")?;
                    if *expected_revision != Some(entry.revision) {
                        return Err("記憶が別の画面で更新されています。再読込してください".into());
                    }
                } else if expected_revision.is_some() {
                    return Err("新規の記憶に既存の版は指定できません".into());
                }
                memory::save(state, id.as_deref(), scope, text)
            }
            Command::DeleteMemory {
                id,
                expected_revision,
            } => {
                let entry = state
                    .memories
                    .iter()
                    .find(|m| m.id == *id)
                    .ok_or("記憶がありません")?;
                if *expected_revision != entry.revision {
                    return Err("記憶が別の画面で更新されています。再読込してください".into());
                }
                memory::delete(state, id)
            }
            Command::UpdateSettings { settings } => {
                settings.validate()?;
                state.settings = settings.clone();
                Ok(())
            }
            Command::ArchiveRun { run_id, archived } => {
                let run = state
                    .runs
                    .iter_mut()
                    .find(|r| r.id == *run_id)
                    .ok_or("依頼がありません")?;
                if !run.status.terminal() {
                    return Err("進行中の依頼は停止してから整理してください".into());
                }
                run.archived = *archived;
                Ok(())
            }
            Command::DeleteRun { run_id } => {
                let index = state
                    .runs
                    .iter()
                    .position(|r| r.id == *run_id)
                    .ok_or("依頼がありません")?;
                if !state.runs[index].status.terminal() {
                    return Err("進行中の依頼は削除できません".into());
                }
                state.runs.remove(index);
                state.messages.retain(|m| m.run_id != *run_id);
                state.context_epoch = state
                    .context_epoch
                    .checked_add(1)
                    .ok_or("文脈の版の上限です")?;
                Ok(())
            }
            Command::ClearConversation { scope } => {
                // Stored scopes are identities. Resolving the path again could
                // delete B's history after the old A directory became a link.
                scope.validate()?;
                if state
                    .runs
                    .iter()
                    .any(|r| &r.scope == scope && !r.status.terminal())
                {
                    return Err("この会話の進行中の依頼を停止してから削除してください".into());
                }
                state.messages.retain(|m| &m.scope != scope);
                state.runs.retain(|r| &r.scope != scope);
                state.context_epoch = state
                    .context_epoch
                    .checked_add(1)
                    .ok_or("文脈の版の上限です")?;
                Ok(())
            }
        }
    }

    fn enqueue(
        &self,
        state: &mut State,
        kind: RunKind,
        objective: &str,
        scope: &Scope,
        attachments: &[String],
    ) -> Result<(), String> {
        if !self.available {
            return Err("OpenAI API の接続設定が必要です。設定を確認して再接続してください".into());
        }
        if objective.trim().is_empty() || objective.len() > MAX_TEXT_BYTES {
            return Err("依頼は空でない64 KiB以内の文章にしてください".into());
        }
        if state.runs.len() >= MAX_RUNS || state.messages.len() + 2 > MAX_MESSAGES {
            return Err("履歴の件数上限です。完了した依頼を整理してください".into());
        }
        let snapshot = context::snapshot(scope, attachments, state.context_epoch)?;
        let id = state.id("run")?;
        let message_id = state.id("message")?;
        let time = now();
        state.messages.push(Message {
            id: message_id,
            scope: snapshot.scope.clone(),
            role: Role::User,
            content: objective.trim().into(),
            run_id: id.clone(),
            created_at: time,
            context_epoch: state.context_epoch,
        });
        state.runs.push(Run {
            id,
            kind,
            scope: snapshot.scope.clone(),
            objective: objective.trim().into(),
            status: RunStatus::Queued,
            created_at: time,
            updated_at: time,
            model: state.settings.model.clone(),
            context: snapshot,
            ..Run::default()
        });
        Ok(())
    }

    /// Poll completions and dispatch only after the corresponding state is durable.
    pub fn tick(&mut self) {
        while let Ok(finished) = self.finished_rx.try_recv() {
            self.active.remove(&finished.run_id);
            if self.failed.is_none() {
                let _ = self.complete(finished);
            }
        }
        if self.failed.is_some() {
            return;
        }
        loop {
            let conversations = self
                .state
                .runs
                .iter()
                .filter(|r| self.active.contains_key(&r.id) && r.kind == RunKind::Conversation)
                .count();
            let jobs = self.active.len().saturating_sub(conversations);
            let next = self
                .state
                .runs
                .iter()
                .find(|r| {
                    r.status == RunStatus::Queued
                        && if r.kind == RunKind::Conversation {
                            conversations < 1
                        } else {
                            jobs < 2
                        }
                })
                .map(|r| r.id.clone());
            let Some(id) = next else { break };
            if self.dispatch(&id).is_err() && self.failed.is_some() {
                break;
            }
        }
    }

    fn dispatch(&mut self, id: &str) -> Result<(), String> {
        let built = context::build(&self.state, id);
        let mut candidate = self.state.clone();
        let run = candidate
            .runs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or("依頼がありません")?;
        let (input, memory_ids) = match built {
            Ok(result) => result,
            Err(error) => {
                run.status = RunStatus::Failed;
                run.error = Some(error);
                return self.save(candidate);
            }
        };
        run.status = RunStatus::Running;
        run.updated_at = now();
        run.context.context_epoch = candidate.context_epoch;
        run.context.input_digest = Some(store::digest(input.as_bytes()));
        run.context.input = Some(input.clone());
        run.context.memory_ids = memory_ids;
        let request = CompletionRequest {
            model: run.model.clone(),
            instructions: context::instructions(run.kind),
            input,
        };
        self.save(candidate)?;
        let canceled = Arc::new(AtomicBool::new(false));
        self.active.insert(id.into(), canceled.clone());
        let backend = self.backend.clone();
        let tx = self.finished_tx.clone();
        let run_id = id.to_owned();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                backend.complete(&request, &canceled)
            }))
            .unwrap_or_else(|_| {
                Err(BackendError::Uncertain(
                    "応答処理が予期せず終了しました".into(),
                ))
            });
            let _ = tx.send(Finished { run_id, result });
        });
        Ok(())
    }

    fn complete(&mut self, finished: Finished) -> Result<(), String> {
        let mut candidate = self.state.clone();
        let index = candidate
            .runs
            .iter()
            .position(|r| r.id == finished.run_id)
            .ok_or("依頼がありません")?;
        let run = &mut candidate.runs[index];
        let result = if run.status == RunStatus::CancelRequested {
            Err(BackendError::Canceled)
        } else {
            finished.result
        };
        match result {
            Ok(result)
                if !result.text.trim().is_empty() && result.text.len() <= MAX_CONTEXT_BYTES =>
            {
                let artifact = if run.kind != RunKind::Conversation {
                    match self
                        .store
                        .write_artifact(&run.id, &run.objective, &result.text)
                    {
                        Ok(a) => Some(a),
                        Err(error) => {
                            self.failed = Some(format!(
                                "成果物を保存できないため新しい実行を停止しました: {error}"
                            ));
                            for cancel in self.active.values() {
                                cancel.store(true, Ordering::Release);
                            }
                            return Err(self.failed.clone().unwrap_or_default());
                        }
                    }
                } else {
                    None
                };
                run.status = RunStatus::Completed;
                run.answer = Some(result.text.clone());
                run.artifact = artifact;
                run.input_tokens = result.input_tokens;
                run.output_tokens = result.output_tokens;
                let scope = run.scope.clone();
                let run_id = run.id.clone();
                let epoch = run.context.context_epoch;
                let message_id = candidate.id("message")?;
                candidate.messages.push(Message {
                    id: message_id,
                    scope,
                    role: Role::Assistant,
                    content: result.text,
                    run_id,
                    created_at: now(),
                    context_epoch: epoch,
                });
            }
            Ok(_) => {
                run.status = RunStatus::Failed;
                run.error = Some("応答が空、または容量上限を超えています".into());
            }
            Err(BackendError::Canceled) => {
                run.status = RunStatus::Canceled;
            }
            Err(BackendError::Failed(error)) => {
                run.status = RunStatus::Failed;
                run.error = Some(error.chars().take(2000).collect());
            }
            Err(BackendError::Uncertain(error)) => {
                run.status = RunStatus::Uncertain;
                run.error = Some(error.chars().take(2000).collect());
            }
        }
        candidate.runs[index].updated_at = now();
        self.save(candidate)?;
        if self.state.settings.notifications
            && self.state.runs[index].kind != RunKind::Conversation
            && matches!(
                self.state.runs[index].status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Uncertain
            )
        {
            // Persist the claim before delivery. A lost notification is visible
            // in the inbox; a crash must not produce repeated OS notifications.
            let mut candidate = self.state.clone();
            candidate.runs[index].notified_at = Some(now());
            self.save(candidate)?;
            if let Err(error) = crate::notifications::completed(&self.state.runs[index]) {
                let mut candidate = self.state.clone();
                candidate.runs[index].notification_error = Some(error);
                self.save(candidate)?;
            }
        }
        Ok(())
    }

    pub fn artifact(&self, run_id: &str) -> Result<String, String> {
        let artifact = self
            .state
            .runs
            .iter()
            .find(|r| r.id == run_id)
            .and_then(|r| r.artifact.as_ref())
            .ok_or("この依頼には保存済み成果物がありません")?;
        self.store.artifact(artifact)
    }

    pub fn export(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())
    }

    pub fn prepare_handoff(&self, run_id: &str) -> Result<crate::handoff::PreparedHandoff, String> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let run = self
            .state
            .runs
            .iter()
            .find(|r| r.id == run_id)
            .ok_or("依頼が削除されたか見つかりません")?;
        if let Some(artifact) = &run.artifact {
            self.store.artifact(artifact)?;
        }
        // The actor serializes this write with deletion. A stale UI cannot
        // recreate a private handoff copy after the run has been removed.
        crate::handoff::prepare(self.store.root(), run)
    }

    fn shutdown(&mut self) {
        for cancel in self.active.values() {
            cancel.store(true, Ordering::Release);
        }
        if self.failed.is_some() {
            return;
        }
        let mut candidate = self.state.clone();
        let mut changed = false;
        for run in &mut candidate.runs {
            if run.status == RunStatus::Queued {
                run.status = RunStatus::Canceled;
                run.error = Some("Noa が終了したため未開始の依頼を送信していません".into());
                run.updated_at = now();
                changed = true;
            } else if matches!(run.status, RunStatus::Running | RunStatus::CancelRequested) {
                run.status = RunStatus::Uncertain;
                run.error =
                    Some("Noa が終了したため応答の受信を中断しました。自動再送していません".into());
                run.updated_at = now();
                changed = true;
            }
        }
        if changed {
            let _ = self.save(candidate);
        }
    }

    fn respond(&mut self, request: Request) -> Response {
        let mut response = Response {
            protocol_version: PROTOCOL_VERSION,
            accepted_revision: None,
            view: None,
            text: None,
            error: None,
            outcome_unknown: false,
        };
        if request.version() != PROTOCOL_VERSION {
            response.error = Some("通信バージョンが一致しません".into());
            return response;
        }
        let result = match request {
            Request::View { .. } => {
                response.view = Some(self.view());
                Ok(())
            }
            Request::RefreshBackend { .. } => {
                self.available = OpenAiBackend::available();
                response.view = Some(self.view());
                Ok(())
            }
            Request::Command {
                request_id, intent, ..
            } => {
                let settings = matches!(intent, Command::UpdateSettings { .. });
                self.command(&request_id, intent).map(|revision| {
                    response.accepted_revision = Some(revision);
                    if settings {
                        self.available = OpenAiBackend::available();
                    }
                    response.view = Some(self.view());
                })
            }
            Request::Artifact { run_id, .. } => self
                .artifact(&run_id)
                .map(|text| response.text = Some(text)),
            Request::PrepareHandoff { run_id, .. } => self
                .prepare_handoff(&run_id)
                .and_then(|prepared| serde_json::to_string(&prepared).map_err(|e| e.to_string()))
                .map(|text| response.text = Some(text)),
            Request::Export { .. } => self.export().map(|text| response.text = Some(text)),
        };
        if let Err(error) = result {
            response.outcome_unknown = self.failed.is_some();
            response.error = Some(error);
        }
        response
    }
}

fn same_user(stream: &UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: getsockopt receives the size and address of credentials.
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut len,
            ) == 0
                && credentials.uid == libc::geteuid()
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: getpeereid initializes both uid/gid of this connected socket.
        unsafe {
            libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
        }
    }
}

fn alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: signal zero probes existence without delivering a signal.
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn run(root: PathBuf, backend: Arc<dyn Backend>) -> Result<(), String> {
    store::private_directory(&root)?;
    let Some(_lease) = Lease::acquire(&root.join("service.lock"))? else {
        return Ok(());
    };
    let socket = client::socket_path(&root)?;
    if let Ok(meta) = std::fs::symlink_metadata(&socket) {
        if !meta.file_type().is_socket() {
            return Err("通信先が既存の通常ファイルです。削除していません".into());
        }
        std::fs::remove_file(&socket).map_err(|e| e.to_string())?;
    }
    let mut service = Service::open(root, backend, OpenAiBackend::available())?;
    let listener = UnixListener::bind(&socket).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let owner = std::env::var("NOA_EMBER_OWNER_PID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    let mut last_client = Instant::now();
    loop {
        if owner.is_some_and(|pid| !alive(pid)) {
            service.shutdown();
            break;
        }
        service.tick();
        match listener.accept() {
            Ok((mut stream, _)) => {
                if !same_user(&stream) {
                    continue;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                if (&mut stream)
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .is_err()
                    || bytes.len() > 1024 * 1024
                {
                    continue;
                }
                let response = match serde_json::from_slice::<Request>(&bytes) {
                    Ok(request) => {
                        last_client = Instant::now();
                        service.respond(request)
                    }
                    Err(_) => Response {
                        protocol_version: PROTOCOL_VERSION,
                        accepted_revision: None,
                        view: None,
                        text: None,
                        error: Some("操作の形式を確認してください".into()),
                        outcome_unknown: false,
                    },
                };
                if let Ok(bytes) = serde_json::to_vec(&response) {
                    let _ = stream.write_all(&bytes);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                service.shutdown();
                return Err(e.to_string());
            }
        }
        if owner.is_none()
            && service.active.is_empty()
            && !service
                .state
                .runs
                .iter()
                .any(|r| r.status == RunStatus::Queued)
            && last_client.elapsed() >= Duration::from_secs(300)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(listener);
    let _ = std::fs::remove_file(socket);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::AtomicUsize};

    struct FakeBackend {
        calls: AtomicUsize,
        release: AtomicBool,
        inputs: Mutex<Vec<String>>,
    }

    impl FakeBackend {
        fn new(release: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                release: AtomicBool::new(release),
                inputs: Mutex::new(Vec::new()),
            })
        }
    }

    impl Backend for FakeBackend {
        fn complete(
            &self,
            request: &CompletionRequest,
            canceled: &AtomicBool,
        ) -> Result<Completion, BackendError> {
            self.inputs.lock().unwrap().push(request.input.clone());
            self.calls.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.release.load(Ordering::SeqCst) {
                if canceled.load(Ordering::Acquire) {
                    return Err(BackendError::Canceled);
                }
                assert!(Instant::now() < deadline, "test did not release backend");
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(Completion {
                text: "# A complete document\n\nThe requested result.".into(),
                input_tokens: Some(12),
                output_tokens: Some(9),
            })
        }
    }

    fn setup(release: bool) -> (tempfile::TempDir, Service, Arc<FakeBackend>) {
        let temp = tempfile::tempdir().unwrap();
        let backend = FakeBackend::new(release);
        let service =
            Service::open(temp.path().canonicalize().unwrap(), backend.clone(), true).unwrap();
        (temp, service, backend)
    }

    fn document() -> Command {
        Command::StartRun {
            kind: RunKind::Document,
            objective: "Write a document".into(),
            scope: Scope::Global,
            attachments: Vec::new(),
        }
    }

    fn until(service: &mut Service, predicate: impl Fn(&Service) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate(service) {
            service.tick();
            assert!(Instant::now() < deadline, "service condition timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn retrying_a_durable_command_cannot_duplicate_a_run_across_restart() {
        let (temp, mut service, backend) = setup(true);
        let revision = service.command("request-one", document()).unwrap();
        assert_eq!(
            service.command("request-one", document()).unwrap(),
            revision
        );
        assert_eq!(service.state.runs.len(), 1);
        assert!(
            service
                .command(
                    "request-one",
                    Command::Send {
                        text: "Different".into(),
                        scope: Scope::Global,
                        attachments: vec![]
                    }
                )
                .is_err()
        );
        drop(service);
        let mut service =
            Service::open(temp.path().canonicalize().unwrap(), backend.clone(), true).unwrap();
        assert_eq!(
            service.command("request-one", document()).unwrap(),
            revision
        );
        service.tick();
        assert_eq!(service.state.runs[0].status, RunStatus::Canceled);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        service.command("explicit-new-request", document()).unwrap();
        until(&mut service, |s| {
            s.state.runs[1].status == RunStatus::Completed
        });
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert!(
            service
                .artifact(&service.state.runs[1].id)
                .unwrap()
                .contains("complete document")
        );
    }

    #[test]
    fn foreground_remains_available_while_two_documents_run_and_cancel_is_acknowledged() {
        let (_temp, mut service, backend) = setup(false);
        service.command("doc-one", document()).unwrap();
        service.command("doc-two", document()).unwrap();
        service.command("doc-three", document()).unwrap();
        service
            .command(
                "chat-one",
                Command::Send {
                    text: "Another question".into(),
                    scope: Scope::Global,
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |_| backend.calls.load(Ordering::SeqCst) == 3);
        assert_eq!(service.active.len(), 3);
        assert_eq!(service.state.runs[2].status, RunStatus::Queued);
        let first = service.state.runs[0].id.clone();
        service
            .command(
                "cancel-one",
                Command::CancelRun {
                    run_id: first.clone(),
                },
            )
            .unwrap();
        assert_eq!(service.state.runs[0].status, RunStatus::CancelRequested);
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Canceled
        });
        until(&mut service, |_| backend.calls.load(Ordering::SeqCst) == 4);
        backend.release.store(true, Ordering::SeqCst);
        until(&mut service, |s| s.active.is_empty());
        assert!(service.state.runs[0].artifact.is_none());
        assert_eq!(service.state.runs[3].status, RunStatus::Completed);
    }

    #[test]
    fn restart_marks_dispatched_work_uncertain_without_resending_it() {
        let (temp, mut service, backend) = setup(false);
        service.command("dispatch-once", document()).unwrap();
        until(&mut service, |_| backend.calls.load(Ordering::SeqCst) == 1);
        drop(service);
        let mut reopened =
            Service::open(temp.path().canonicalize().unwrap(), backend.clone(), true).unwrap();
        assert_eq!(reopened.state.runs[0].status, RunStatus::Uncertain);
        reopened.tick();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        backend.release.store(true, Ordering::SeqCst);
    }

    #[test]
    fn failed_persistence_prevents_dispatch_and_preserves_the_invalid_bytes() {
        let (temp, mut service, backend) = setup(true);
        service.command("durable-queue", document()).unwrap();
        std::fs::write(temp.path().join("state.json"), b"corrupt manifest").unwrap();
        service.tick();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert!(service.view().notice.is_some());
        assert!(service.command("next-request", document()).is_err());
        assert_eq!(
            std::fs::read(temp.path().join("state.json")).unwrap(),
            b"corrupt manifest"
        );
    }

    #[test]
    fn attached_contents_are_frozen_and_deleting_a_run_removes_its_artifact() {
        let (temp, mut service, backend) = setup(true);
        let input = temp.path().canonicalize().unwrap().join("source.md");
        std::fs::write(&input, "original selected evidence").unwrap();
        service
            .command(
                "attached-run",
                Command::StartRun {
                    kind: RunKind::Research,
                    objective: "Explain the evidence".into(),
                    scope: Scope::Global,
                    attachments: vec![input.display().to_string()],
                },
            )
            .unwrap();
        std::fs::write(&input, "later unshared change").unwrap();
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Completed
        });
        let sent = backend.inputs.lock().unwrap()[0].clone();
        assert!(sent.contains("original selected evidence"));
        assert!(!sent.contains("later unshared change"));
        let run_id = service.state.runs[0].id.clone();
        let artifact = service.state.runs[0].artifact.clone().unwrap();
        service
            .command("remove-run", Command::DeleteRun { run_id })
            .unwrap();
        assert!(!temp.path().join(artifact.path).exists());
        assert!(service.state.messages.is_empty());
        assert_eq!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("snapshot-"))
                .count(),
            1
        );
    }

    #[test]
    fn memory_edits_use_revision_checks_and_deletion_affects_future_context() {
        let (_temp, mut service, backend) = setup(true);
        service
            .command(
                "save-memory",
                Command::SaveMemory {
                    id: None,
                    expected_revision: None,
                    scope: Scope::Global,
                    text: "Secret preference".into(),
                },
            )
            .unwrap();
        let id = service.state.memories[0].id.clone();
        service
            .command(
                "chat-with-memory",
                Command::Send {
                    text: "Hello".into(),
                    scope: Scope::Global,
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Completed
        });
        assert!(backend.inputs.lock().unwrap()[0].contains("Secret preference"));
        assert!(
            service
                .command(
                    "stale-edit",
                    Command::SaveMemory {
                        id: Some(id.clone()),
                        expected_revision: Some(0),
                        scope: Scope::Global,
                        text: "Wrong update".into()
                    }
                )
                .is_err()
        );
        service
            .command(
                "delete-memory",
                Command::DeleteMemory {
                    id,
                    expected_revision: 1,
                },
            )
            .unwrap();
        service
            .command(
                "chat-without-memory",
                Command::Send {
                    text: "Continue".into(),
                    scope: Scope::Global,
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |s| {
            s.state.runs[1].status == RunStatus::Completed
        });
        assert!(!backend.inputs.lock().unwrap()[1].contains("Secret preference"));
        assert!(!backend.inputs.lock().unwrap()[1].contains("The requested result"));
    }

    #[test]
    fn deleting_a_source_run_excludes_late_derived_answers_from_future_context() {
        let (_temp, mut service, backend) = setup(true);
        service.command("source-document", document()).unwrap();
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Completed
        });
        let source = service.state.runs[0].id.clone();
        backend.release.store(false, Ordering::SeqCst);
        service
            .command(
                "derived-answer",
                Command::Send {
                    text: "Explain the preceding result".into(),
                    scope: Scope::Global,
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |_| backend.calls.load(Ordering::SeqCst) == 2);
        assert!(backend.inputs.lock().unwrap()[1].contains("The requested result"));
        service
            .command("delete-source", Command::DeleteRun { run_id: source })
            .unwrap();
        backend.release.store(true, Ordering::SeqCst);
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Completed
        });
        service
            .command(
                "fresh-input",
                Command::Send {
                    text: "Start a fresh discussion".into(),
                    scope: Scope::Global,
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |s| {
            s.state.runs[1].status == RunStatus::Completed
        });
        assert!(!backend.inputs.lock().unwrap()[2].contains("The requested result"));
    }

    #[test]
    fn a_deleted_run_cannot_be_recreated_as_a_handoff_from_a_stale_view() {
        let (temp, mut service, _backend) = setup(true);
        let project = temp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&project).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(&project)
                .status()
                .unwrap()
                .success()
        );
        service
            .command(
                "project-document",
                Command::StartRun {
                    kind: RunKind::Document,
                    objective: "Plan a feature".into(),
                    scope: Scope::Project {
                        root: project.display().to_string(),
                    },
                    attachments: vec![],
                },
            )
            .unwrap();
        until(&mut service, |s| {
            s.state.runs[0].status == RunStatus::Completed
        });
        let id = service.state.runs[0].id.clone();
        let prepared = service.prepare_handoff(&id).unwrap();
        assert!(prepared.path.is_file());
        service
            .command(
                "remove-project-run",
                Command::DeleteRun { run_id: id.clone() },
            )
            .unwrap();
        assert!(!prepared.path.exists());
        assert!(service.prepare_handoff(&id).is_err());
    }

    #[test]
    fn clearing_a_project_uses_the_saved_identity_after_a_path_is_replaced() {
        let (temp, mut service, _) = setup(true);
        let root = temp.path().canonicalize().unwrap();
        let a = root.join("project-a");
        let b = root.join("project-b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let scope_a = Scope::Project {
            root: a.display().to_string(),
        };
        let scope_b = Scope::Project {
            root: b.display().to_string(),
        };
        for (id, scope) in [
            ("project-a-run", scope_a.clone()),
            ("project-b-run", scope_b.clone()),
        ] {
            service
                .command(
                    id,
                    Command::Send {
                        text: "Hello".into(),
                        scope,
                        attachments: vec![],
                    },
                )
                .unwrap();
        }
        until(&mut service, |s| {
            s.state.runs.iter().all(|r| r.status.terminal())
        });
        std::fs::remove_dir(&a).unwrap();
        std::os::unix::fs::symlink(&b, &a).unwrap();
        service
            .command("clear-a", Command::ClearConversation { scope: scope_a })
            .unwrap();
        assert_eq!(service.state.runs.len(), 1);
        assert_eq!(service.state.runs[0].scope, scope_b);
        std::fs::remove_dir(&b).unwrap();
        service
            .command(
                "clear-missing-b",
                Command::ClearConversation { scope: scope_b },
            )
            .unwrap();
        assert!(service.state.runs.is_empty());
    }
}
