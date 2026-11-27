//! One writer for repository widget instances, snapshots and acknowledged UI
//! events. Native hosts only receive a private stdio channel for one instance.
//! No control-server token or executable from the repository reaches a host.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use noa_ipc::widget::{
    AckParams, CloseParams, EventsParams, HostMessage, HostRequest, MAX_ACTION_BYTES,
    MAX_BRIDGE_LINE_BYTES, MAX_DATA_BYTES, OpenParams, SetParams, WidgetCall, WidgetError,
    WidgetErrorKind, WidgetEvent, asset_frames, validate_size, validate_token,
};
use noa_widget::{Manifest, content_digest, discovery};
use parking_lot::Mutex;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ipc_bridge::IpcShared;

const SERVICE_WAIT: Duration = Duration::from_secs(4);
const MAX_INSTANCES: usize = 64;
const MAX_OPEN_INSTANCES: usize = 16;
const MAX_EVENTS: usize = 128;
const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_REQUESTS: usize = 1024;
const MAX_STORE_BYTES: usize = 1024 * 1024;

enum Message {
    Call(WidgetCall, Sender<Result<Value, WidgetError>>),
    Host(String, HostRequest),
    HostGone(String),
}

struct HandleInner {
    tx: Sender<Message>,
    stop: Arc<AtomicBool>,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        // Host reader threads hold channel senders, not this handle: their
        // pipes cannot accidentally keep the service alive after App exits.
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct WidgetHandle {
    inner: Arc<HandleInner>,
    opened: Arc<OnceLock<bool>>,
    settled: Receiver<()>,
}

pub(crate) fn start(root: PathBuf, shared: Arc<Mutex<IpcShared>>) -> io::Result<WidgetHandle> {
    let (tx, rx) = bounded(64);
    let stop = Arc::new(AtomicBool::new(false));
    let opened = Arc::new(OnceLock::new());
    let (settle, settled) = bounded::<()>(0);
    let worker_stop = stop.clone();
    let worker_opened = opened.clone();
    let worker_tx = tx.clone();
    thread::Builder::new()
        .name("noa-widgets".into())
        .spawn(move || {
            let store = Store::open(root, true, worker_tx);
            let _ = worker_opened.set(store.is_ok());
            drop(settle);
            let mut store = match store {
                Ok(store) => store,
                Err(err) => {
                    log::warn!("widgets: service disabled: {err}");
                    return;
                }
            };
            while !worker_stop.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Message::Call(call, reply)) => {
                        let panels = Arc::clone(&shared.lock().panels);
                        let result = store.call(call, &panels);
                        let _ = reply.send(result);
                    }
                    Ok(Message::Host(id, request)) => store.host_request(&id, request),
                    Ok(Message::HostGone(id)) => store.host_gone(&id),
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
                let panels = Arc::clone(&shared.lock().panels);
                store.reap(&panels);
            }
            // Store's Drop terminates and reaps the native children. Persisted
            // instances restore as inert snapshots, never as executable work.
        })?;
    Ok(WidgetHandle {
        inner: Arc::new(HandleInner { tx, stop }),
        opened,
        settled,
    })
}

impl WidgetHandle {
    pub(crate) fn opened(&self) -> bool {
        if self.opened.get().is_none() {
            let _ = self.settled.recv_timeout(SERVICE_WAIT);
        }
        self.opened.get().copied().unwrap_or(false)
    }

    pub(crate) fn call(&self, call: WidgetCall) -> Result<Value, WidgetError> {
        call.validate()?;
        let mutation = call.scope() == noa_ipc::Scope::Control;
        let (tx, rx) = bounded(1);
        match self.inner.tx.try_send(Message::Call(call, tx)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(error(WidgetErrorKind::Busy, "widget service is busy"));
            }
            Err(TrySendError::Disconnected(_)) => return Err(WidgetError::unsupported()),
        }
        match rx.recv_timeout(SERVICE_WAIT) {
            Ok(answer) => answer,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) if mutation => Err(error(
                WidgetErrorKind::OutcomeUnknown,
                "widget operation may have completed; retry the same operation, retaining its requestId when present",
            )),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(error(
                WidgetErrorKind::Busy,
                "widget service did not answer in time",
            )),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(WidgetError::unsupported())
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Remembered {
    fingerprint: String,
    result: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Instance {
    format_version: u32,
    instance_id: String,
    server_instance_id: String,
    worktree_root: PathBuf,
    repository_id: String,
    worktree_id: String,
    manifest: Manifest,
    definition_digest: String,
    /// Content digests of the pinned definition's assets. The bytes are not
    /// stored: a restored instance never relaunches a host that needs them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    asset_digests: BTreeMap<String, String>,
    definition_source: String,
    data: Value,
    data_revision: u64,
    owner_pane_id: Option<noa_ipc::WireId>,
    status: String,
    host_ready: bool,
    consumer_id: Option<String>,
    delivered_through: u64,
    acked_through: u64,
    last_seq: u64,
    events: Vec<WidgetEvent>,
    requests: BTreeMap<String, Remembered>,
    action_requests: BTreeMap<String, String>,
    open_request_id: String,
    open_fingerprint: String,
}

impl Instance {
    fn validate_stored(&self) -> Result<(), WidgetError> {
        if self.data_revision == 0
            || self.acked_through > self.delivered_through
            || self.delivered_through > self.last_seq
            || self.events.len() > MAX_EVENTS
            || self.requests.len() > MAX_REQUESTS + MAX_EVENTS + 1
            || self.action_requests.len() > MAX_REQUESTS
            || self.action_requests.len() as u64 != self.last_seq
            || !matches!(
                self.status.as_str(),
                "open" | "closed" | "restored" | "hostExited" | "ownerExited"
            )
        {
            return Err(io_error("invalid widget state counters or lifecycle"));
        }
        validate_token("requestId", &self.open_request_id)?;
        let mut seq = self.acked_through;
        for event in &self.events {
            seq += 1;
            if event.seq != seq
                || event.data_revision == 0
                || event.data_revision > self.data_revision
            {
                return Err(io_error("invalid widget event sequence"));
            }
            validate_size(&event.payload, MAX_ACTION_BYTES)?;
            self.manifest
                .validate_action(&event.action_id, &event.payload)
                .map_err(domain_error)?;
        }
        if seq != self.last_seq {
            return Err(io_error("widget state is missing unacknowledged events"));
        }
        Ok(())
    }

    fn snapshot(&self) -> Value {
        json!({
            "instanceId": self.instance_id, "serverInstanceId": self.server_instance_id,
            "definitionId": self.manifest.id, "definitionDigest": self.definition_digest,
            "definitionSource": self.definition_source, "worktreeId": self.worktree_id,
            "repositoryId": self.repository_id, "repo": self.worktree_root,
            "dataRevision": self.data_revision, "data": self.data,
            "manifest": self.manifest, "status": self.status, "hostReady": self.host_ready,
            "ownerPaneId": self.owner_pane_id, "consumerId": self.consumer_id,
            "lastSeq": self.last_seq, "ackedThrough": self.acked_through
        })
    }

    fn host_snapshot(&self) -> HostMessage {
        HostMessage::Snapshot {
            instance_id: self.instance_id.clone(),
            definition_digest: self.definition_digest.clone(),
            data_revision: self.data_revision,
            manifest: serde_json::to_value(&self.manifest).expect("validated manifest"),
            data: self.data.clone(),
        }
    }

    fn replay(&self, request_id: &str, fingerprint: &str) -> Result<Option<Value>, WidgetError> {
        match self.requests.get(request_id) {
            Some(old) if old.fingerprint == fingerprint => Ok(Some(old.result.clone())),
            Some(_) => Err(conflict("requestId was already used with another payload")),
            None => Ok(None),
        }
    }

    fn remember(
        &mut self,
        request_id: String,
        fingerprint: String,
        result: Value,
    ) -> Result<(), WidgetError> {
        // Closed instances retain space to acknowledge their bounded event
        // queue before forget, without permitting an unbounded dedup map.
        let limit = if self.status == "open" {
            MAX_REQUESTS
        } else {
            MAX_REQUESTS + MAX_EVENTS + 1
        };
        if self.requests.len() >= limit {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "instance request limit reached; close it and open a new instance",
            ));
        }
        self.requests.insert(
            request_id,
            Remembered {
                fingerprint,
                result,
            },
        );
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), WidgetError> {
        if self.status != "open" {
            return Err(conflict(
                "instance is no longer open; its snapshot and events remain readable",
            ));
        }
        Ok(())
    }
}

struct Host {
    child: Child,
    input: Sender<HostMessage>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Store {
    root: PathBuf,
    lock: File,
    instances: BTreeMap<String, Instance>,
    hosts: BTreeMap<String, Host>,
    launch_hosts: bool,
    tx: Sender<Message>,
    #[cfg(test)]
    fail_after_publish: bool,
    #[cfg(test)]
    host_program: Option<PathBuf>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.hosts.clear();
        // A concurrent Command::spawn can inherit this descriptor briefly
        // between fork and exec. Explicitly release the shared flock before
        // close, so an immediate restart is not held up by that child copy.
        let _ = self.lock.unlock();
    }
}

impl Store {
    fn open(root: PathBuf, launch_hosts: bool, tx: Sender<Message>) -> Result<Self, WidgetError> {
        fs::create_dir_all(&root).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = opts.open(root.join("store.lock")).map_err(io_error)?;
        lock.try_lock().map_err(|_| {
            error(
                WidgetErrorKind::Busy,
                "another Noa instance owns the widget store",
            )
        })?;
        let mut store = Self {
            root,
            lock,
            instances: BTreeMap::new(),
            hosts: BTreeMap::new(),
            launch_hosts,
            tx,
            #[cfg(test)]
            fail_after_publish: false,
            #[cfg(test)]
            host_program: None,
        };
        for entry in fs::read_dir(&store.root).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if entry.file_type().map_err(io_error)?.is_symlink() {
                return Err(io_error("symlink in widget state store"));
            }
            if store.instances.len() >= MAX_INSTANCES {
                return Err(error(
                    WidgetErrorKind::LimitReached,
                    "too many stored widget instances",
                ));
            }
            let bytes = read_bounded(&path, MAX_STORE_BYTES)?;
            let mut instance: Instance = serde_json::from_slice(&bytes).map_err(io_error)?;
            noa_ipc::widget::validate_instance(&instance.instance_id)?;
            if instance.format_version != 1
                || path.file_stem().and_then(|s| s.to_str()) != Some(instance.instance_id.as_str())
            {
                return Err(io_error("invalid widget state file identity or version"));
            }
            // Reload definitions only from the recorded snapshot. Repository
            // edits cannot change an existing instance's schema or actions.
            let canonical = serde_json::to_vec(&instance.manifest).map_err(io_error)?;
            instance.manifest = Manifest::from_slice(&canonical).map_err(domain_error)?;
            instance
                .manifest
                .validate_data(&instance.data)
                .map_err(domain_error)?;
            if noa_widget::definition_digest(&instance.manifest, &instance.asset_digests)
                .map_err(domain_error)?
                != instance.definition_digest
            {
                return Err(io_error("widget definition digest mismatch"));
            }
            instance.validate_stored()?;
            if instance.status == "open" {
                instance.status = "restored".into();
                instance.host_ready = false;
                store.persist(&instance)?;
            }
            store
                .instances
                .insert(instance.instance_id.clone(), instance);
        }
        Ok(store)
    }

    fn persist(&self, instance: &Instance) -> Result<(), WidgetError> {
        let bytes = serde_json::to_vec(instance).map_err(io_error)?;
        if bytes.len() > MAX_STORE_BYTES {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "widget state size limit reached",
            ));
        }
        let path = self.root.join(format!("{}.json", instance.instance_id));
        let temporary = self
            .root
            .join(format!(".{}.{}.tmp", instance.instance_id, random_hex()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> io::Result<()> {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        let _ = fs::remove_file(&temporary);
        result.map_err(io_error)?;
        #[cfg(test)]
        if self.fail_after_publish {
            return Err(error(
                WidgetErrorKind::OutcomeUnknown,
                "injected directory sync failure after publication",
            ));
        }
        // A rename has already published the new state. Do not pretend a
        // later directory-sync failure rolled it back; the caller must retry
        // the same request id, and our memory must retain its dedup result.
        File::open(&self.root).and_then(|dir| dir.sync_all()).map_err(|err| {
            error(WidgetErrorKind::OutcomeUnknown, format!("widget state was published but its directory did not sync: {err}; retry the same requestId"))
        })
    }

    fn save(&mut self, instance: Instance) -> Result<(), WidgetError> {
        let result = self.persist(&instance);
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|e| e.kind == WidgetErrorKind::OutcomeUnknown)
        {
            self.instances
                .insert(instance.instance_id.clone(), instance);
        }
        result
    }

    fn save_runtime(&mut self, instance: Instance) {
        // A process has already started or ended. A disk error cannot undo
        // that observation; keep the live truth even if its snapshot could
        // not be refreshed. A restart always makes old open state inert.
        if let Err(err) = self.persist(&instance) {
            log::warn!("widgets: runtime status not persisted: {err}");
        }
        self.instances
            .insert(instance.instance_id.clone(), instance);
    }

    fn instance(&self, id: &str) -> Result<&Instance, WidgetError> {
        self.instances
            .get(id)
            .ok_or_else(|| error(WidgetErrorKind::NotFound, "widget instance not found"))
    }

    fn call(&mut self, call: WidgetCall, panels: &[noa_ipc::Panel]) -> Result<Value, WidgetError> {
        call.validate()?;
        let fingerprint = fingerprint(call.method(), &call.params());
        let mutation = call.scope() == noa_ipc::Scope::Control;
        let result = match call {
            WidgetCall::List(p) => {
                let context = discovery::repo_context(Path::new(&p.repo)).map_err(domain_error)?;
                let definitions =
                    discovery::discover(&context.worktree_root).map_err(domain_error)?;
                let definitions: Vec<Value> = definitions.into_iter().map(|d| json!({
                    "id": d.manifest.id, "title": d.manifest.title, "description": d.manifest.description.chars().take(240).collect::<String>(),
                    "version": d.manifest.version, "digest": d.digest
                })).collect();
                let instances: Vec<Value> = self.instances.values().filter(|i| i.worktree_id == context.worktree_id).map(|i| json!({
                    "instanceId": i.instance_id, "definitionId": i.manifest.id,
                    "dataRevision": i.data_revision, "status": i.status, "hostReady": i.host_ready,
                    "definitionDigest": i.definition_digest, "serverInstanceId": i.server_instance_id
                })).collect();
                Ok(
                    json!({"repo":context.worktree_root, "worktreeId":context.worktree_id, "repositoryId":context.repository_id, "definitions":definitions, "instances":instances}),
                )
            }
            WidgetCall::Describe(p) => {
                let d = discovery::load(Path::new(&p.repo), &p.id).map_err(domain_error)?;
                Ok(json!({"manifest":d.manifest, "digest":d.digest, "path":d.path}))
            }
            WidgetCall::Open(p) => self.open_instance(p, fingerprint, panels),
            WidgetCall::Get(p) => Ok(self.instance(&p.instance_id)?.snapshot()),
            WidgetCall::Set(p) => self.set(p, fingerprint),
            WidgetCall::Events(p) => self.events(p),
            WidgetCall::Ack(p) => self.ack(p, fingerprint),
            WidgetCall::Close(p) => self.close(p, fingerprint),
            WidgetCall::Forget(p) => self.forget(&p.instance_id),
        };
        if result.is_ok() && mutation {
            File::open(&self.root)
                .and_then(|dir| dir.sync_all())
                .map_err(|err| {
                    error(
                        WidgetErrorKind::OutcomeUnknown,
                        format!(
                            "widget state durability is unknown: {err}; retry the same requestId"
                        ),
                    )
                })?;
        }
        result
    }

    fn open_instance(
        &mut self,
        p: OpenParams,
        fingerprint: String,
        panels: &[noa_ipc::Panel],
    ) -> Result<Value, WidgetError> {
        if let Some(old) = self
            .instances
            .values()
            .find(|i| i.open_request_id == p.request_id)
        {
            if old.open_fingerprint != fingerprint {
                return Err(conflict(
                    "requestId was already used for another open request",
                ));
            }
            let mut result = old.snapshot();
            result["created"] = json!(false);
            return Ok(result);
        }
        if self.instances.len() >= MAX_INSTANCES
            || self
                .instances
                .values()
                .filter(|i| i.status == "open")
                .count()
                >= MAX_OPEN_INSTANCES
        {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "widget instance limit reached (64 stored / 16 open); close and forget acknowledged instances",
            ));
        }
        if let Some(owner) = p.owner_pane_id
            && !panels
                .iter()
                .any(|pane| pane.pane_id == owner && pane.exit.is_none())
        {
            return Err(error(
                WidgetErrorKind::NotFound,
                "owner pane is absent or its process has exited",
            ));
        }
        let context = discovery::repo_context(Path::new(&p.repo)).map_err(domain_error)?;
        let (manifest, digest, assets, source) = if let Some(id) = p.id {
            let d = discovery::load(&context.worktree_root, &id).map_err(domain_error)?;
            (d.manifest, d.digest, d.assets, "repository")
        } else {
            let bytes = serde_json::to_vec(p.definition.as_ref().expect("validated open source"))
                .map_err(domain_error)?;
            let manifest = Manifest::from_slice(&bytes).map_err(domain_error)?;
            if manifest.assets.is_some() {
                // Asset paths are relative to a package directory, and an
                // inline definition has none to read them from.
                return Err(WidgetError::invalid(
                    "a definition with assets cannot be opened inline; open its repository package by id",
                ));
            }
            let digest = manifest.digest();
            (manifest, digest, Vec::new(), "inline")
        };
        let data = p.data.unwrap_or_else(|| manifest.initial_data.clone());
        validate_size(&data, MAX_DATA_BYTES)?;
        manifest.validate_data(&data).map_err(domain_error)?;
        let instance = Instance {
            format_version: 1,
            instance_id: format!("wi_{}", random_hex()),
            server_instance_id: noa_ipc::server::server_instance_id().into(),
            worktree_root: context.worktree_root,
            repository_id: context.repository_id,
            worktree_id: context.worktree_id,
            definition_digest: digest,
            asset_digests: noa_widget::assets::digests(&assets),
            definition_source: source.into(),
            manifest,
            data,
            data_revision: 1,
            owner_pane_id: p.owner_pane_id,
            status: "open".into(),
            host_ready: !self.launch_hosts,
            consumer_id: None,
            delivered_through: 0,
            acked_through: 0,
            last_seq: 0,
            events: Vec::new(),
            requests: BTreeMap::new(),
            action_requests: BTreeMap::new(),
            open_request_id: p.request_id,
            open_fingerprint: fingerprint,
        };
        check_host_message(&instance.host_snapshot())?;
        // Resolve only Noa's registered helper, never PATH or a manifest path.
        let host = if self.launch_hosts {
            let path = self.host_program()?;
            let frames = assets
                .iter()
                .flat_map(|asset| asset_frames(&asset.id, &asset.digest, &asset.bytes))
                .collect();
            Some(launch_host(
                &path,
                &instance.instance_id,
                frames,
                self.tx.clone(),
            )?)
        } else {
            None
        };
        let id = instance.instance_id.clone();
        let snapshot = instance.host_snapshot();
        let mut result = instance.snapshot();
        result["created"] = json!(true);
        let saved = self.save(instance);
        if committed(&saved)
            && let Some(host) = host
        {
            self.hosts.insert(id.clone(), host);
            self.send_host(&id, snapshot);
        }
        saved?;
        Ok(result)
    }

    fn set(&mut self, p: SetParams, fingerprint: String) -> Result<Value, WidgetError> {
        let mut instance = self.instance(&p.instance_id)?.clone();
        if let Some(replay) = instance.replay(&p.request_id, &fingerprint)? {
            return Ok(replay);
        }
        instance.ensure_open()?;
        // Keep one slot for an action receipt. Only this service enqueues
        // messages, so after this check the writer can only free more room.
        if self
            .hosts
            .get(&p.instance_id)
            .is_some_and(|host| host.input.len() >= 15)
        {
            return Err(error(
                WidgetErrorKind::Busy,
                "widget host has pending updates; retry this operation when it catches up",
            ));
        }
        if instance.data_revision != p.expected_revision {
            return Err(conflict(format!(
                "data revision conflict: current dataRevision is {}",
                instance.data_revision
            )));
        }
        instance
            .manifest
            .validate_data(&p.data)
            .map_err(domain_error)?;
        instance.data = p.data;
        instance.data_revision = instance
            .data_revision
            .checked_add(1)
            .ok_or_else(|| error(WidgetErrorKind::LimitReached, "revision limit reached"))?;
        let snapshot = instance.host_snapshot();
        check_host_message(&snapshot)?;
        let result =
            json!({"instanceId":instance.instance_id,"dataRevision":instance.data_revision});
        instance.remember(p.request_id, fingerprint, result.clone())?;
        let saved = self.save(instance);
        if committed(&saved) {
            self.send_host(&p.instance_id, snapshot);
        }
        saved?;
        Ok(result)
    }

    fn host_program(&self) -> Result<PathBuf, WidgetError> {
        #[cfg(test)]
        if let Some(path) = &self.host_program {
            return Ok(path.clone());
        }
        crate::helper_program::program_path(crate::helper_program::WIDGET_HOST).map_err(io_error)
    }

    fn events(&mut self, p: EventsParams) -> Result<Value, WidgetError> {
        let mut instance = self.instance(&p.instance_id)?.clone();
        if instance
            .consumer_id
            .as_deref()
            .is_some_and(|c| c != p.consumer_id)
        {
            return Err(conflict("another consumer owns this instance's events"));
        }
        if p.after_seq > instance.last_seq {
            return Err(WidgetError::invalid("afterSeq exceeds the latest event"));
        }
        if p.after_seq > instance.delivered_through {
            return Err(WidgetError::invalid(
                "afterSeq cannot skip events that have not been delivered",
            ));
        }
        let selected: Vec<WidgetEvent> = instance
            .events
            .iter()
            .filter(|e| e.seq > p.after_seq.max(instance.acked_through))
            .take(p.limit)
            .cloned()
            .collect();
        let next = selected
            .last()
            .map_or(p.after_seq.max(instance.acked_through), |e| e.seq);
        let changed = instance.consumer_id.is_none() || next > instance.delivered_through;
        instance.consumer_id = Some(p.consumer_id.clone());
        instance.delivered_through = instance.delivered_through.max(next);
        let result = json!({"instanceId":instance.instance_id,"consumerId":p.consumer_id,"events":selected,
            "nextSeq":next,"lastSeq":instance.last_seq,"ackedThrough":instance.acked_through,
            "hasMore":next < instance.last_seq,"status":instance.status});
        if changed {
            self.save(instance)?;
        }
        Ok(result)
    }

    fn ack(&mut self, p: AckParams, fingerprint: String) -> Result<Value, WidgetError> {
        let mut instance = self.instance(&p.instance_id)?.clone();
        if let Some(replay) = instance.replay(&p.request_id, &fingerprint)? {
            return Ok(replay);
        }
        if instance.consumer_id.as_deref() != Some(p.consumer_id.as_str()) {
            return Err(conflict(
                "only the registered event consumer may acknowledge events",
            ));
        }
        if p.through_seq > instance.delivered_through {
            return Err(WidgetError::invalid(
                "cannot acknowledge events that were not delivered",
            ));
        }
        instance.acked_through = instance.acked_through.max(p.through_seq);
        instance.events.retain(|e| e.seq > instance.acked_through);
        let result =
            json!({"instanceId":instance.instance_id,"ackedThrough":instance.acked_through});
        instance.remember(p.request_id, fingerprint, result.clone())?;
        self.save(instance)?;
        Ok(result)
    }

    fn close(&mut self, p: CloseParams, fingerprint: String) -> Result<Value, WidgetError> {
        let mut instance = self.instance(&p.instance_id)?.clone();
        if let Some(replay) = instance.replay(&p.request_id, &fingerprint)? {
            return Ok(replay);
        }
        instance.status = "closed".into();
        instance.host_ready = false;
        let result = json!({"instanceId":instance.instance_id,"closed":true});
        instance.remember(p.request_id, fingerprint, result.clone())?;
        let saved = self.save(instance);
        if committed(&saved) {
            self.stop_host(&p.instance_id);
        }
        saved?;
        Ok(result)
    }

    fn submit_action(
        &mut self,
        id: &str,
        request_id: String,
        action_id: String,
        revision: u64,
        payload: Value,
    ) -> Result<(), WidgetError> {
        validate_token("requestId", &request_id)?;
        validate_token("actionId", &action_id)?;
        validate_size(&payload, MAX_ACTION_BYTES)?;
        let key = fingerprint(
            "host.action",
            &json!({"actionId":action_id,"dataRevision":revision,"payload":payload}),
        );
        let mut instance = self.instance(id)?.clone();
        if let Some(previous) = instance.action_requests.get(&request_id) {
            if *previous != key {
                return Err(conflict("action requestId was reused with another payload"));
            }
            return File::open(&self.root)
                .and_then(|dir| dir.sync_all())
                .map_err(|err| {
                    error(
                        WidgetErrorKind::OutcomeUnknown,
                        format!(
                            "event may already be accepted, but its directory did not sync: {err}"
                        ),
                    )
                });
        }
        instance.ensure_open()?;
        if revision != instance.data_revision {
            return Err(conflict(
                "the displayed data changed; retry the action on the current view",
            ));
        }
        instance
            .manifest
            .validate_action(&action_id, &payload)
            .map_err(domain_error)?;
        if instance.action_requests.len() >= MAX_REQUESTS {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "instance action limit reached; close it and open a new instance",
            ));
        }
        if instance.events.len() >= MAX_EVENTS {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "widget event queue is full; the agent must acknowledge events",
            ));
        }
        let seq = instance.last_seq.checked_add(1).ok_or_else(|| {
            error(
                WidgetErrorKind::LimitReached,
                "event sequence limit reached",
            )
        })?;
        instance.events.push(WidgetEvent {
            event_id: format!("we_{}_{}", &id[3..], seq),
            seq,
            action_id,
            data_revision: revision,
            payload,
        });
        if serde_json::to_vec(&instance.events)
            .map_err(io_error)?
            .len()
            > MAX_EVENT_BYTES
        {
            return Err(error(
                WidgetErrorKind::LimitReached,
                "widget event bytes limit reached; no event was accepted",
            ));
        }
        instance.last_seq = seq;
        instance.action_requests.insert(request_id, key);
        self.save(instance)
    }

    fn forget(&mut self, id: &str) -> Result<Value, WidgetError> {
        if let Some(instance) = self.instances.get(id) {
            if instance.status == "open" || !instance.events.is_empty() {
                return Err(conflict(
                    "close the instance and acknowledge every event before forgetting it",
                ));
            }
            let path = self.root.join(format!("{id}.json"));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(io_error(err)),
            }
            self.instances.remove(id);
        }
        // Repeating this operation after an ambiguous reply remains safe.
        // Forget explicitly ends the stored open request's dedup lifetime.
        Ok(json!({"instanceId":id,"forgotten":true}))
    }

    fn host_request(&mut self, id: &str, request: HostRequest) {
        match request {
            HostRequest::Ready => {
                if let Some(mut instance) = self.instances.get(id).cloned()
                    && instance.status == "open"
                {
                    instance.host_ready = true;
                    self.save_runtime(instance);
                }
            }
            HostRequest::Action {
                request_id,
                action_id,
                data_revision,
                payload,
            } => {
                let result =
                    self.submit_action(id, request_id.clone(), action_id, data_revision, payload);
                let error_kind = result.as_ref().err().map(|e| e.kind_name().to_owned());
                self.send_host(
                    id,
                    HostMessage::ActionResult {
                        request_id,
                        ok: result.is_ok(),
                        error: result.err().map(|e| e.message),
                        error_kind,
                    },
                );
            }
            HostRequest::Closed => self.host_gone(id),
        }
    }

    fn send_host(&mut self, id: &str, message: HostMessage) {
        if let Some(host) = self.hosts.get(id)
            && let Err(err) = host.input.try_send(message)
        {
            log::warn!("widgets: host channel stopped for {id}: {err}");
            self.host_gone(id);
        }
    }

    fn stop_host(&mut self, id: &str) {
        if let Some(host) = self.hosts.remove(id) {
            let _ = host.input.try_send(HostMessage::Closed);
            drop(host);
        }
    }

    fn host_gone(&mut self, id: &str) {
        self.stop_host(id);
        if let Some(mut instance) = self.instances.get(id).cloned()
            && instance.status == "open"
        {
            instance.status = "hostExited".into();
            instance.host_ready = false;
            self.save_runtime(instance);
        }
    }

    fn reap(&mut self, panels: &[noa_ipc::Panel]) {
        let ended: Vec<String> = self
            .hosts
            .iter_mut()
            .filter_map(|(id, host)| host.child.try_wait().ok().flatten().map(|_| id.clone()))
            .collect();
        for id in ended {
            self.host_gone(&id);
        }
        let owner_gone: Vec<String> = self
            .instances
            .values()
            .filter(|i| i.status == "open")
            .filter(|i| {
                i.owner_pane_id.is_some_and(|owner| {
                    !panels
                        .iter()
                        .any(|p| p.pane_id == owner && p.exit.is_none())
                })
            })
            .map(|i| i.instance_id.clone())
            .collect();
        for id in owner_gone {
            let mut instance = self.instances[&id].clone();
            instance.status = "ownerExited".into();
            instance.host_ready = false;
            self.save_runtime(instance);
            self.stop_host(&id);
        }
    }
}

/// `assets` are written before anything from the update channel, so the host
/// holds every image before the initial snapshot that pins them arrives.
/// They bypass the bounded channel, which only needs room for updates.
fn launch_host(
    program: &Path,
    instance_id: &str,
    assets: Vec<HostMessage>,
    tx: Sender<Message>,
) -> Result<Host, WidgetError> {
    let mut command = Command::new(program);
    command
        .arg("--bridge-stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The trusted renderer needs only display/session configuration. It does
    // not load Noa's config or credentials, and receives no controller env.
    command.env_clear();
    for name in [
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "PATH",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "TMPDIR",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command.spawn().map_err(io_error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io_error("widget host stdout unavailable"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io_error("widget host stdin unavailable"))?;
    let (input, messages) = bounded::<HostMessage>(16);
    let host = Host { child, input };
    let read_id = instance_id.to_owned();
    let read_tx = tx.clone();
    thread::Builder::new()
        .name("widget-host-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                match (&mut reader)
                    .take((MAX_BRIDGE_LINE_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) if bytes.len() > MAX_BRIDGE_LINE_BYTES || !bytes.ends_with(b"\n") => {
                        break;
                    }
                    Ok(_) => match serde_json::from_slice::<HostRequest>(&bytes) {
                        Ok(request) => {
                            if read_tx
                                .send(Message::Host(read_id.clone(), request))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(_) => break,
                    },
                }
            }
            let _ = read_tx.send(Message::HostGone(read_id));
        })
        .map_err(io_error)?;
    let write_id = instance_id.to_owned();
    thread::Builder::new()
        .name("widget-host-writer".into())
        .spawn(move || {
            for message in assets.into_iter().chain(messages) {
                let Ok(mut bytes) = serde_json::to_vec(&message) else {
                    break;
                };
                if bytes.len() + 1 > MAX_BRIDGE_LINE_BYTES {
                    break;
                }
                bytes.push(b'\n');
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
            let _ = tx.send(Message::HostGone(write_id));
        })
        .map_err(io_error)?;
    Ok(host)
}

fn check_host_message(message: &HostMessage) -> Result<(), WidgetError> {
    if serde_json::to_vec(message).map_err(io_error)?.len() + 1 > MAX_BRIDGE_LINE_BYTES {
        return Err(error(
            WidgetErrorKind::LimitReached,
            "widget snapshot exceeds the host channel size limit",
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, WidgetError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err(io_error("widget state must be a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > max {
        return Err(io_error("widget state exceeds its size limit"));
    }
    Ok(bytes)
}

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(method: &str, params: &Value) -> String {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k, canonical(v)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            other => other,
        }
    }
    content_digest(
        &serde_json::to_vec(&canonical(json!({"method":method,"params":params})))
            .expect("JSON payload"),
    )
}

fn committed(result: &Result<(), WidgetError>) -> bool {
    result.is_ok()
        || result
            .as_ref()
            .is_err_and(|err| err.kind == WidgetErrorKind::OutcomeUnknown)
}

fn error(kind: WidgetErrorKind, message: impl Into<String>) -> WidgetError {
    WidgetError::new(kind, message)
}
fn conflict(message: impl Into<String>) -> WidgetError {
    error(WidgetErrorKind::Conflict, message)
}
fn io_error(err: impl std::fmt::Display) -> WidgetError {
    error(WidgetErrorKind::IoError, err.to_string())
}
fn domain_error(err: impl std::fmt::Display) -> WidgetError {
    WidgetError::invalid(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::widget::{DescribeParams, InstanceParams, ListParams};

    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        state: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("noa-widget-test-{}", random_hex()));
            let repo = root.join("repo");
            let state = root.join("state");
            fs::create_dir_all(&repo).unwrap();
            assert!(
                Command::new("git")
                    .args(["init", "-q"])
                    .arg(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
            let definition = repo.join(".noa-widgets/status");
            fs::create_dir_all(&definition).unwrap();
            fs::write(
                definition.join("widget.json"),
                serde_json::to_vec(&manifest()).unwrap(),
            )
            .unwrap();
            Self { root, repo, state }
        }
        fn store(&self) -> Store {
            let (tx, _) = bounded(64);
            Store::open(self.state.clone(), false, tx).unwrap()
        }
        fn open(&self, request: &str) -> WidgetCall {
            WidgetCall::Open(OpenParams {
                repo: self.repo.to_string_lossy().into_owned(),
                id: Some("status".into()),
                definition: None,
                data: None,
                request_id: request.into(),
                owner_pane_id: None,
            })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn manifest() -> Value {
        json!({"apiVersion":"noa.widget/v1", "id":"status", "version":"1.0.0", "title":"Status",
            "description":"Test status", "dataSchema":{"type":"object","properties":{"summary":{"type":"string"}},
                "required":["summary"],"additionalProperties":false},
            "initialData":{"summary":"pending"},
            "view":{"type":"text","id":"summary","text":{"source":"data","pointer":"/summary"}},
            "actions":[{"id":"refresh","kind":"agent-event","payloadSchema":{"type":"object","additionalProperties":false}}]})
    }

    fn instance_id(value: &Value) -> String {
        value["instanceId"].as_str().unwrap().into()
    }

    fn events(id: &str, consumer: &str) -> WidgetCall {
        WidgetCall::Events(EventsParams {
            instance_id: id.into(),
            consumer_id: consumer.into(),
            after_seq: 0,
            limit: 32,
        })
    }

    #[test]
    fn updates_are_atomic_compare_and_swap_and_request_retries_do_not_apply_twice() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let opened = store.call(fixture.open("open-1"), &[]).unwrap();
        let id = instance_id(&opened);
        let duplicate = store.call(fixture.open("open-1"), &[]).unwrap();
        assert_eq!(duplicate["instanceId"], id);
        assert_eq!(duplicate["created"], false);
        let update = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 1,
            data: json!({"summary":"done"}),
            request_id: "set-1".into(),
        });
        assert_eq!(store.call(update.clone(), &[]).unwrap()["dataRevision"], 2);
        assert_eq!(store.call(update, &[]).unwrap()["dataRevision"], 2);
        let stale = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 1,
            data: json!({"summary":"wrong"}),
            request_id: "stale".into(),
        });
        assert_eq!(
            store.call(stale, &[]).unwrap_err().kind,
            WidgetErrorKind::Conflict
        );
        let reused = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 2,
            data: json!({"summary":"wrong"}),
            request_id: "set-1".into(),
        });
        assert_eq!(
            store.call(reused, &[]).unwrap_err().kind,
            WidgetErrorKind::Conflict
        );
        let invalid = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 2,
            data: json!({"summary":10}),
            request_id: "invalid".into(),
        });
        assert_eq!(
            store.call(invalid, &[]).unwrap_err().kind,
            WidgetErrorKind::InvalidParams
        );
        assert_eq!(store.instance(&id).unwrap().data, json!({"summary":"done"}));
        assert_eq!(store.instance(&id).unwrap().data_revision, 2);
    }

    #[test]
    fn events_survive_restart_have_one_consumer_and_acknowledgements_are_durable() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        store
            .submit_action(&id, "click-1".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        store
            .submit_action(&id, "click-1".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        assert_eq!(store.instance(&id).unwrap().last_seq, 1);
        let page = store.call(events(&id, "agent-a"), &[]).unwrap();
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
        assert_eq!(
            store.call(events(&id, "agent-b"), &[]).unwrap_err().kind,
            WidgetErrorKind::Conflict
        );
        drop(store);
        let mut restored = fixture.store();
        assert_eq!(restored.instance(&id).unwrap().status, "restored");
        assert!(restored.hosts.is_empty());
        assert_eq!(
            restored.call(events(&id, "agent-a"), &[]).unwrap()["events"],
            page["events"]
        );
        let ack = WidgetCall::Ack(AckParams {
            instance_id: id.clone(),
            consumer_id: "agent-a".into(),
            through_seq: 1,
            request_id: "ack-1".into(),
        });
        restored.call(ack.clone(), &[]).unwrap();
        restored.call(ack, &[]).unwrap();
        drop(restored);
        let mut restored = fixture.store();
        let page = restored.call(events(&id, "agent-a"), &[]).unwrap();
        assert_eq!(page["ackedThrough"], 1);
        assert!(page["events"].as_array().unwrap().is_empty());
        assert_eq!(
            restored.call(fixture.open("open"), &[]).unwrap()["instanceId"],
            id
        );
    }

    #[test]
    fn definitions_are_pinned_and_inline_open_does_not_write_the_repository() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let original = store.call(fixture.open("first"), &[]).unwrap();
        let mut modified = manifest();
        modified["title"] = json!("Updated title");
        fs::write(
            fixture.repo.join(".noa-widgets/status/widget.json"),
            serde_json::to_vec(&modified).unwrap(),
        )
        .unwrap();
        let second = store.call(fixture.open("second"), &[]).unwrap();
        assert_ne!(original["definitionDigest"], second["definitionDigest"]);
        let read = store
            .call(
                WidgetCall::Get(InstanceParams {
                    instance_id: instance_id(&original),
                }),
                &[],
            )
            .unwrap();
        assert_eq!(read["manifest"]["title"], "Status");
        let before = Command::new("git")
            .args([
                "-C",
                fixture.repo.to_str().unwrap(),
                "status",
                "--porcelain",
                "--untracked-files=all",
            ])
            .output()
            .unwrap()
            .stdout;
        let inline = WidgetCall::Open(OpenParams {
            repo: fixture.repo.to_string_lossy().into_owned(),
            id: None,
            definition: Some(manifest()),
            data: None,
            request_id: "inline".into(),
            owner_pane_id: None,
        });
        assert_eq!(
            store.call(inline, &[]).unwrap()["definitionSource"],
            "inline"
        );
        let after = Command::new("git")
            .args([
                "-C",
                fixture.repo.to_str().unwrap(),
                "status",
                "--porcelain",
                "--untracked-files=all",
            ])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(before, after);
    }

    #[test]
    fn stale_unknown_and_invalid_actions_do_not_enqueue_events_and_ack_cannot_skip_delivery() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        for (action, revision, payload) in [
            ("refresh", 0, json!({})),
            ("missing", 1, json!({})),
            ("refresh", 1, json!({"command":"sh"})),
        ] {
            assert!(
                store
                    .submit_action(&id, "refused".into(), action.into(), revision, payload)
                    .is_err()
            );
        }
        assert_eq!(store.instance(&id).unwrap().last_seq, 0);
        store
            .submit_action(&id, "valid".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        let ack = WidgetCall::Ack(AckParams {
            instance_id: id.clone(),
            consumer_id: "a".into(),
            through_seq: 1,
            request_id: "ack".into(),
        });
        assert!(store.call(ack, &[]).is_err());
        store.call(events(&id, "a"), &[]).unwrap();
        let ack = WidgetCall::Ack(AckParams {
            instance_id: id,
            consumer_id: "a".into(),
            through_seq: 2,
            request_id: "ack-future".into(),
        });
        assert!(store.call(ack, &[]).is_err());
    }

    #[test]
    fn two_instances_keep_independent_data_and_consumers() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let a = instance_id(&store.call(fixture.open("a"), &[]).unwrap());
        let b = instance_id(&store.call(fixture.open("b"), &[]).unwrap());
        store
            .call(
                WidgetCall::Set(SetParams {
                    instance_id: a.clone(),
                    expected_revision: 1,
                    data: json!({"summary":"a"}),
                    request_id: "set".into(),
                }),
                &[],
            )
            .unwrap();
        assert_eq!(store.instance(&b).unwrap().data_revision, 1);
        assert_eq!(store.instance(&b).unwrap().data["summary"], "pending");
        store.call(events(&a, "consumer-a"), &[]).unwrap();
        store.call(events(&b, "consumer-b"), &[]).unwrap();
        let listing = store
            .call(
                WidgetCall::List(ListParams {
                    repo: fixture.repo.to_string_lossy().into_owned(),
                }),
                &[],
            )
            .unwrap();
        assert_eq!(listing["instances"].as_array().unwrap().len(), 2);
        assert_eq!(listing["definitions"].as_array().unwrap().len(), 1);
        store
            .call(
                WidgetCall::Describe(DescribeParams {
                    repo: fixture.repo.to_string_lossy().into_owned(),
                    id: "status".into(),
                }),
                &[],
            )
            .unwrap();
    }

    #[test]
    fn a_full_event_queue_rejects_new_input_without_dropping_old_actions() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        for n in 0..MAX_EVENTS {
            store
                .submit_action(&id, format!("click-{n}"), "refresh".into(), 1, json!({}))
                .unwrap();
        }
        assert_eq!(
            store
                .submit_action(&id, "overflow".into(), "refresh".into(), 1, json!({}))
                .unwrap_err()
                .kind,
            WidgetErrorKind::LimitReached
        );
        let instance = store.instance(&id).unwrap();
        assert_eq!(instance.events.len(), MAX_EVENTS);
        assert_eq!(instance.events[0].seq, 1);
        assert_eq!(instance.last_seq, MAX_EVENTS as u64);
    }

    #[test]
    fn storage_failure_leaves_the_previous_state_and_dedup_cache_unchanged() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        // The actual directory remains owned by the lock; an impossible
        // child path gives a deterministic failure even when tests run as root.
        let valid_root = store.root.clone();
        let blocker = fixture.root.join("not-a-directory");
        fs::write(&blocker, b"x").unwrap();
        store.root = blocker.join("store");
        let update = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 1,
            data: json!({"summary":"new"}),
            request_id: "retry".into(),
        });
        assert_eq!(
            store.call(update.clone(), &[]).unwrap_err().kind,
            WidgetErrorKind::IoError
        );
        assert_eq!(store.instance(&id).unwrap().data_revision, 1);
        assert!(!store.instance(&id).unwrap().requests.contains_key("retry"));
        store.root = valid_root;
        assert_eq!(store.call(update, &[]).unwrap()["dataRevision"], 2);
    }

    #[test]
    fn the_store_has_one_writer_and_closed_instances_reject_updates() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let (tx, _) = bounded(1);
        assert!(Store::open(fixture.state.clone(), false, tx).is_err());
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        let close = WidgetCall::Close(CloseParams {
            instance_id: id.clone(),
            request_id: "close".into(),
        });
        store.call(close.clone(), &[]).unwrap();
        store.call(close, &[]).unwrap();
        assert!(
            store
                .call(
                    WidgetCall::Set(SetParams {
                        instance_id: id,
                        expected_revision: 1,
                        data: json!({"summary":"late"}),
                        request_id: "late".into()
                    }),
                    &[]
                )
                .is_err()
        );
    }

    #[test]
    fn a_consumer_cannot_jump_over_undelivered_events() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        for n in 0..2 {
            store
                .submit_action(&id, format!("click-{n}"), "refresh".into(), 1, json!({}))
                .unwrap();
        }
        let skipped = WidgetCall::Events(EventsParams {
            instance_id: id.clone(),
            consumer_id: "a".into(),
            after_seq: 1,
            limit: 1,
        });
        assert!(store.call(skipped.clone(), &[]).is_err());
        assert!(store.instance(&id).unwrap().consumer_id.is_none());
        let first = WidgetCall::Events(EventsParams {
            instance_id: id.clone(),
            consumer_id: "a".into(),
            after_seq: 0,
            limit: 1,
        });
        assert_eq!(store.call(first, &[]).unwrap()["nextSeq"], 1);
        assert_eq!(store.call(skipped, &[]).unwrap()["nextSeq"], 2);
    }

    #[test]
    fn forgetting_requires_closed_acknowledged_instances_and_releases_capacity() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        let forget = WidgetCall::Forget(InstanceParams {
            instance_id: id.clone(),
        });
        assert!(store.call(forget.clone(), &[]).is_err());
        store
            .submit_action(&id, "click".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        store
            .call(
                WidgetCall::Close(CloseParams {
                    instance_id: id.clone(),
                    request_id: "close".into(),
                }),
                &[],
            )
            .unwrap();
        assert!(store.call(forget.clone(), &[]).is_err());
        store.call(events(&id, "consumer"), &[]).unwrap();
        store
            .call(
                WidgetCall::Ack(AckParams {
                    instance_id: id.clone(),
                    consumer_id: "consumer".into(),
                    through_seq: 1,
                    request_id: "ack".into(),
                }),
                &[],
            )
            .unwrap();
        assert_eq!(store.call(forget.clone(), &[]).unwrap()["forgotten"], true);
        assert_eq!(store.call(forget, &[]).unwrap()["forgotten"], true);
        assert!(store.instances.is_empty());
        assert!(!fixture.state.join(format!("{id}.json")).exists());
        let replacement = store.call(fixture.open("open"), &[]).unwrap();
        assert_ne!(replacement["instanceId"], id);
    }

    #[test]
    fn published_but_unconfirmed_open_set_and_close_retain_the_host_and_dedup_state() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let fake_host = fixture.root.join("fake-host");
        fs::write(
            &fake_host,
            b"#!/bin/sh\nwhile IFS= read -r line; do :; done\n",
        )
        .unwrap();
        fs::set_permissions(&fake_host, fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = fixture.store();
        store.launch_hosts = true;
        store.host_program = Some(fake_host);
        store.fail_after_publish = true;
        assert_eq!(
            store.call(fixture.open("open"), &[]).unwrap_err().kind,
            WidgetErrorKind::OutcomeUnknown
        );
        let id = store.instances.keys().next().unwrap().clone();
        assert!(store.hosts.contains_key(&id));
        assert!(
            store
                .hosts
                .get_mut(&id)
                .unwrap()
                .child
                .try_wait()
                .unwrap()
                .is_none()
        );
        store.fail_after_publish = false;
        assert_eq!(
            store.call(fixture.open("open"), &[]).unwrap()["instanceId"],
            id
        );
        assert_eq!(store.hosts.len(), 1);
        let set = WidgetCall::Set(SetParams {
            instance_id: id.clone(),
            expected_revision: 1,
            data: json!({"summary":"published"}),
            request_id: "set".into(),
        });
        store.fail_after_publish = true;
        assert_eq!(
            store.call(set.clone(), &[]).unwrap_err().kind,
            WidgetErrorKind::OutcomeUnknown
        );
        assert_eq!(store.instance(&id).unwrap().data_revision, 2);
        assert!(store.hosts.contains_key(&id));
        store.fail_after_publish = false;
        assert_eq!(store.call(set, &[]).unwrap()["dataRevision"], 2);
        let close = WidgetCall::Close(CloseParams {
            instance_id: id.clone(),
            request_id: "close".into(),
        });
        store.fail_after_publish = true;
        assert_eq!(
            store.call(close.clone(), &[]).unwrap_err().kind,
            WidgetErrorKind::OutcomeUnknown
        );
        assert!(!store.hosts.contains_key(&id));
        store.fail_after_publish = false;
        assert_eq!(store.call(close, &[]).unwrap()["closed"], true);
    }

    #[test]
    fn stored_state_rejects_fifos_and_symlinks_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = Fixture::new();
        let fifo = fixture.root.join("state.json");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: c_path owns a terminated path until mkfifo returns.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert!(read_bounded(&fifo, MAX_STORE_BYTES).is_err());
        let file = fixture.root.join("regular");
        fs::write(&file, b"{}").unwrap();
        let link = fixture.root.join("link.json");
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert!(read_bounded(&link, MAX_STORE_BYTES).is_err());
    }

    #[test]
    fn a_host_exit_is_observed_even_when_persisting_its_status_fails() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        store
            .submit_action(&id, "click".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        let blocker = fixture.root.join("not-a-directory");
        fs::write(&blocker, b"x").unwrap();
        store.root = blocker.join("store");
        store.host_gone(&id);
        let snapshot = store.instance(&id).unwrap().snapshot();
        assert_eq!(snapshot["status"], "hostExited");
        assert_eq!(snapshot["hostReady"], false);
        assert_eq!(store.instance(&id).unwrap().events.len(), 1);
        let set = WidgetCall::Set(SetParams {
            instance_id: id,
            expected_revision: 1,
            data: json!({"summary":"late"}),
            request_id: "set".into(),
        });
        assert_eq!(
            store.call(set, &[]).unwrap_err().kind,
            WidgetErrorKind::Conflict
        );
    }

    #[test]
    fn a_held_open_exited_owner_is_as_inactive_as_a_removed_owner() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let mut pane: noa_ipc::Panel = serde_json::from_value(json!({
            "paneId":"4","windowId":"1","windowGroupId":"1","name":"agent","cwd":"/repo",
            "busy":false,"attention":false,"preview":[]
        }))
        .unwrap();
        let mut owned = fixture.open("owned");
        if let WidgetCall::Open(params) = &mut owned {
            params.owner_pane_id = Some(noa_ipc::WireId(4));
        }
        let id = instance_id(&store.call(owned, &[pane.clone()]).unwrap());
        store.reap(&[pane.clone()]);
        assert_eq!(store.instance(&id).unwrap().status, "open");
        pane.exit = Some(noa_ipc::PanelExit { code: Some(0) });
        store.reap(&[pane.clone()]);
        assert_eq!(store.instance(&id).unwrap().status, "ownerExited");
        let mut again = fixture.open("already-exited");
        if let WidgetCall::Open(params) = &mut again {
            params.owner_pane_id = Some(noa_ipc::WireId(4));
        }
        assert_eq!(
            store.call(again, &[pane]).unwrap_err().kind,
            WidgetErrorKind::NotFound
        );
    }

    #[test]
    fn an_unconfirmed_action_can_only_be_retried_under_the_same_id() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let id = instance_id(&store.call(fixture.open("open"), &[]).unwrap());
        store.fail_after_publish = true;
        let err = store
            .submit_action(&id, "click".into(), "refresh".into(), 1, json!({}))
            .unwrap_err();
        assert_eq!(err.kind_name(), "outcome_unknown");
        assert_eq!(store.instance(&id).unwrap().events.len(), 1);
        store.fail_after_publish = false;
        store
            .submit_action(&id, "click".into(), "refresh".into(), 1, json!({}))
            .unwrap();
        assert_eq!(store.instance(&id).unwrap().events.len(), 1);
        assert_eq!(store.instance(&id).unwrap().last_seq, 1);
    }

    const LOGO: &[u8] = include_bytes!("../../../../.noa-widgets/image-gallery/assets/logo.png");

    fn gallery_manifest() -> Value {
        json!({"apiVersion":"noa.widget/v1", "id":"gallery", "version":"1.0.0", "title":"Gallery",
            "description":"Packaged image", "assets":{"logo":{"path":"assets/logo.png"}},
            "dataSchema":{"type":"object","properties":{},"additionalProperties":false},
            "initialData":{},
            "view":{"type":"image","id":"logo","src":"logo","alt":"Logo"}})
    }

    impl Fixture {
        fn gallery(&self) -> PathBuf {
            let package = self.repo.join(".noa-widgets/gallery");
            fs::create_dir_all(package.join("assets")).unwrap();
            fs::write(
                package.join("widget.json"),
                serde_json::to_vec(&gallery_manifest()).unwrap(),
            )
            .unwrap();
            fs::write(package.join("assets/logo.png"), LOGO).unwrap();
            package
        }

        fn open_gallery(&self, request: &str) -> WidgetCall {
            let mut call = self.open(request);
            if let WidgetCall::Open(params) = &mut call {
                params.id = Some("gallery".into());
            }
            call
        }
    }

    #[test]
    fn packaged_assets_reach_the_host_before_the_snapshot_and_only_digests_persist() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let package = fixture.gallery();
        let received = fixture.root.join("host-input");
        let fake_host = fixture.root.join("fake-host");
        fs::write(
            &fake_host,
            format!("#!/bin/sh\nexec cat > '{}'\n", received.display()),
        )
        .unwrap();
        fs::set_permissions(&fake_host, fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = fixture.store();
        store.launch_hosts = true;
        store.host_program = Some(fake_host);
        let opened = store.call(fixture.open_gallery("open"), &[]).unwrap();
        let id = instance_id(&opened);
        let manifest =
            Manifest::from_slice(&serde_json::to_vec(&gallery_manifest()).unwrap()).unwrap();
        let digests = BTreeMap::from([("logo".to_owned(), content_digest(LOGO))]);
        let pinned = noa_widget::definition_digest(&manifest, &digests).unwrap();
        assert_eq!(opened["definitionDigest"], pinned);
        assert_ne!(pinned, manifest.digest());

        // The host's input: every asset frame, then the pinning snapshot.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let lines = loop {
            let text = fs::read_to_string(&received).unwrap_or_default();
            if text.contains("\"type\":\"snapshot\"") || std::time::Instant::now() > deadline {
                break text.lines().map(str::to_owned).collect::<Vec<_>>();
            }
            thread::sleep(Duration::from_millis(20));
        };
        let (snapshot, frames) = lines.split_last().expect("host input");
        let HostMessage::Snapshot {
            definition_digest, ..
        } = serde_json::from_str(snapshot).unwrap()
        else {
            panic!("the snapshot follows the asset frames");
        };
        assert_eq!(definition_digest, pinned);
        let mut bytes = Vec::new();
        for line in frames {
            let HostMessage::Asset {
                id, digest, data, ..
            } = serde_json::from_str(line).unwrap()
            else {
                panic!("only asset frames precede the snapshot");
            };
            assert_eq!(
                (id.as_str(), digest.as_str()),
                ("logo", digests["logo"].as_str())
            );
            bytes.extend(noa_ipc::widget::base64_decode(&data).unwrap());
        }
        assert_eq!(bytes, LOGO);
        assert!(
            !lines
                .iter()
                .any(|line| line.contains(&*package.to_string_lossy()))
        );

        // The state keeps the digest, never the image bytes.
        let state = fs::read_to_string(fixture.state.join(format!("{id}.json"))).unwrap();
        assert!(state.contains(&digests["logo"]));
        assert!(!state.contains(&noa_ipc::widget::base64_encode(&LOGO[..48])));
        drop(store);
        let restored = fixture.store();
        assert_eq!(restored.instance(&id).unwrap().status, "restored");
        assert_eq!(restored.instance(&id).unwrap().definition_digest, pinned);
        drop(restored);

        // One changed image byte is a different definition.
        let mut changed = LOGO.to_vec();
        *changed.last_mut().unwrap() ^= 1;
        fs::write(package.join("assets/logo.png"), changed).unwrap();
        let mut store = fixture.store();
        let reopened = store.call(fixture.open_gallery("reopen"), &[]).unwrap();
        assert_ne!(reopened["definitionDigest"], pinned);
    }

    #[test]
    fn inline_definitions_with_assets_are_rejected_clearly() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        let inline = WidgetCall::Open(OpenParams {
            repo: fixture.repo.to_string_lossy().into_owned(),
            id: None,
            definition: Some(gallery_manifest()),
            data: None,
            request_id: "inline".into(),
            owner_pane_id: None,
        });
        let error = store.call(inline, &[]).unwrap_err();
        assert_eq!(error.kind, WidgetErrorKind::InvalidParams);
        assert!(error.message.contains("cannot be opened inline"));
        assert!(store.instances.is_empty());
    }

    #[test]
    fn missing_or_linked_assets_fail_the_open_before_any_state_is_written() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let package = fixture.gallery();
        let mut store = fixture.store();
        fs::remove_file(package.join("assets/logo.png")).unwrap();
        assert_eq!(
            store
                .call(fixture.open_gallery("missing"), &[])
                .unwrap_err()
                .kind,
            WidgetErrorKind::InvalidParams
        );
        let outside = fixture.root.join("outside.png");
        fs::write(&outside, LOGO).unwrap();
        symlink(&outside, package.join("assets/logo.png")).unwrap();
        let error = store.call(fixture.open_gallery("linked"), &[]).unwrap_err();
        assert!(error.message.contains("symbolic link"), "{}", error.message);
        assert!(store.instances.is_empty());
    }
}
