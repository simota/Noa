//! Socket requests, file previews and helper launches stay off the native UI loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ember::client::{Client, ClientError};
use ember::model::{Attachment, Command, Scope, View};

pub(super) enum Job {
    Command { request_id: String, intent: Command },
    PreviewFile { path: String, scope: Scope },
    Artifact { run_id: String, title: String },
    Project(String),
    PrepareHandoff(String),
    OpenHandoff(PathBuf),
    Export,
    Reconnect,
    Close,
}

pub(super) struct HandoffPreview {
    pub path: PathBuf,
    pub title: String,
    pub objective: String,
    pub repo: PathBuf,
}

pub(super) enum Update {
    Connected(View),
    View(View),
    Disconnected(String),
    Command {
        request_id: String,
        result: Result<View, ClientError>,
    },
    Attachment {
        scope: Scope,
        result: Result<Attachment, String>,
    },
    Text {
        title: String,
        result: Result<String, String>,
    },
    Project {
        initial: bool,
        result: Result<Scope, String>,
    },
    Handoff(Result<HandoffPreview, String>),
    HandoffOpened(Result<(), String>),
}

pub(super) struct Feed {
    jobs: Sender<Job>,
    updates: Receiver<Update>,
}

impl Feed {
    pub fn start(
        root: PathBuf,
        executable: PathBuf,
        repository: Option<PathBuf>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let (sender, updates) = mpsc::channel();
        std::thread::spawn(move || {
            let notify = |update| {
                let _ = sender.send(update);
                wake();
            };
            if let Some(repository) = repository {
                notify(Update::Project {
                    initial: true,
                    result: project(&repository),
                });
            }
            let mut client = match Client::connect_or_start(root.clone(), &executable) {
                Ok(client) => match client.view() {
                    Ok(view) => {
                        notify(Update::Connected(view));
                        Some(client)
                    }
                    Err(error) => {
                        notify(Update::Disconnected(error));
                        None
                    }
                },
                Err(error) => {
                    notify(Update::Disconnected(error));
                    None
                }
            };
            let mut prepared = None;
            let mut last_revision = None;
            loop {
                match receiver.recv_timeout(Duration::from_millis(900)) {
                    Ok(Job::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Job::Reconnect) => {
                        match Client::connect_or_start(root.clone(), &executable) {
                            Ok(next) => match next.view() {
                                Ok(view) => {
                                    last_revision = Some(view.state.revision);
                                    client = Some(next);
                                    notify(Update::Connected(view));
                                }
                                Err(error) => notify(Update::Disconnected(error)),
                            },
                            Err(error) => notify(Update::Disconnected(error)),
                        }
                    }
                    Ok(Job::Command { request_id, intent }) => {
                        let result = client
                            .as_ref()
                            .ok_or_else(|| ClientError {
                                message: disconnected(),
                                outcome_unknown: false,
                            })
                            .and_then(|client| client.request_checked(&request_id, intent));
                        if let Ok(view) = &result {
                            last_revision = Some(view.state.revision);
                        }
                        notify(Update::Command { request_id, result });
                    }
                    Ok(Job::PreviewFile { path, scope }) => {
                        let result = ember::context::preview_file(&path, &scope);
                        notify(Update::Attachment { scope, result });
                    }
                    Ok(Job::Artifact { run_id, title }) => {
                        let result = client
                            .as_ref()
                            .ok_or_else(disconnected)
                            .and_then(|client| client.artifact(&run_id));
                        notify(Update::Text { title, result });
                    }
                    Ok(Job::Project(path)) => notify(Update::Project {
                        initial: false,
                        result: project(Path::new(&path)),
                    }),
                    Ok(Job::PrepareHandoff(run_id)) => {
                        prepared = None;
                        let result = client
                            .as_ref()
                            .ok_or_else(disconnected)
                            .and_then(|client| client.prepare_handoff(&run_id));
                        match result {
                            Ok(value) => {
                                let preview = HandoffPreview {
                                    path: value.path.clone(),
                                    title: value.title.clone(),
                                    objective: value.objective.clone(),
                                    repo: value.repo.clone(),
                                };
                                prepared = Some(value);
                                notify(Update::Handoff(Ok(preview)));
                            }
                            Err(error) => notify(Update::Handoff(Err(error))),
                        }
                    }
                    Ok(Job::OpenHandoff(expected)) => {
                        let result = prepared
                            .as_ref()
                            .filter(|value| value.path == expected)
                            .ok_or_else(|| {
                                "引継ぎの内容が変わりました。もう一度準備してください".to_string()
                            })
                            .and_then(ember::handoff::open);
                        notify(Update::HandoffOpened(result));
                    }
                    Ok(Job::Export) => {
                        let result = client
                            .as_ref()
                            .ok_or_else(disconnected)
                            .and_then(Client::export);
                        notify(Update::Text {
                            title: "エンバーの保存データ（コピーできます）".into(),
                            result,
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(current) = &client {
                            match current.view() {
                                Ok(view) if Some(view.state.revision) != last_revision => {
                                    last_revision = Some(view.state.revision);
                                    notify(Update::View(view));
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    client = None;
                                    notify(Update::Disconnected(error));
                                }
                            }
                        }
                    }
                }
            }
        });
        Self { jobs, updates }
    }

    pub fn send(&self, job: Job) -> Result<(), String> {
        self.jobs.send(job).map_err(|_| disconnected())
    }

    pub fn drain(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        // Detaching this view does not stop a service-owned request.
        let _ = self.jobs.send(Job::Close);
    }
}

fn disconnected() -> String {
    "エンバーのサービスに接続できません。再接続してください".into()
}

fn project(path: &Path) -> Result<Scope, String> {
    let root = path
        .canonicalize()
        .map_err(|error| format!("プロジェクトを開けません: {error}"))?;
    if !root.is_dir() {
        return Err("プロジェクトにはフォルダーを指定してください".into());
    }
    let root = root
        .to_str()
        .ok_or("プロジェクトのパスはUTF-8で指定してください")?
        .to_owned();
    let scope = Scope::Project { root };
    scope.validate()?;
    Ok(scope)
}
