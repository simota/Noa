//! The image comparison's view scope and background job. It writes a
//! local HTML page; no native thumbnail texture is uploaded by this UI.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use noa_gpui_core::widgets::{Button, ControlSize};
use noa_gpui_core::{UiTree, WidgetId};
use noa_gpui_text::TextStore;
use verify::model::types::{EvidenceKey, Session, SessionId};
use verify::{preview, store};

use super::{Action, Open, Selected, Verify, evidence};

pub(super) struct Job {
    session: SessionId,
    reply_root: PathBuf,
    session_dir: PathBuf,
    answer: Receiver<Result<preview::Prepared, String>>,
}

pub(super) struct Offer {
    count: usize,
    busy: bool,
    available: bool,
    scope: String,
}

pub(super) fn control(
    tree: &mut UiTree,
    parent: WidgetId,
    offer: &Offer,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let label = if offer.busy {
        "比較を準備中…".to_owned()
    } else {
        format!("画像を比較（{}）…", offer.count)
    };
    let tooltip = if !offer.available {
        "保存された検証の画像をブラウザで比較します".to_owned()
    } else {
        format!(
            "{}の保存済み画像をブラウザで比較します（最大 {} 枚・合計 64 MiB）",
            offer.scope,
            preview::MAX_IMAGES,
        )
    };
    let id = tree.insert(
        Some(parent),
        Button::new(label)
            .control_size(ControlSize::Small)
            .tooltip(tooltip),
    );
    tree.set_enabled(id, offer.available && !offer.busy && offer.count > 0);
    actions.push((id, Action::PreviewImages));
}

fn image_keys(
    session: &Session,
    filters: &evidence::Filters,
    focus: Option<&evidence::Focus>,
    criterion: Option<&str>,
) -> Vec<EvidenceKey> {
    let items = match focus {
        Some(focus) => focus
            .keys
            .iter()
            .filter_map(|key| session.evidence_item(key))
            .collect(),
        None => evidence::shown(session, filters),
    };
    let mut keys = Vec::new();
    let mut seen = BTreeSet::new();
    for item in items {
        if item.stored.is_some()
            && item.media().is_some_and(|format| !format.is_video())
            && criterion.is_none_or(|id| evidence::relates(session, item, id))
            && seen.insert(&item.key)
        {
            keys.push(item.key.clone());
        }
    }
    keys
}

impl Verify {
    fn preview_criterion(&self) -> Option<&str> {
        match &self.selected {
            Some(Selected::Criterion(id)) => Some(id),
            _ => None,
        }
    }

    fn preview_keys(&self) -> Vec<EvidenceKey> {
        self.session.as_deref().map_or_else(Vec::new, |session| {
            image_keys(
                session,
                &self.evidence_filters,
                self.evidence_focus.as_ref(),
                self.preview_criterion(),
            )
        })
    }

    fn preview_scope(&self) -> String {
        let scope = self
            .evidence_focus
            .as_ref()
            .map_or("表示中の証跡", |focus| focus.title.as_str());
        match self.preview_criterion() {
            Some(id) => format!("{scope} · {id}"),
            None => scope.to_owned(),
        }
    }

    pub(super) fn preview_offer(&self) -> Offer {
        Offer {
            count: self.preview_keys().len(),
            busy: self.preview_job.is_some(),
            available: self.reply_root.is_some() && self.session_dir.is_some(),
            scope: self.preview_scope(),
        }
    }

    pub(super) fn preview_images(&mut self, text: &mut TextStore) {
        if self.preview_job.is_some() {
            return;
        }
        let (Some(session), Some(reply_root), Some(session_dir)) = (
            self.session.clone(),
            self.reply_root.clone(),
            self.session_dir.clone(),
        ) else {
            return self.toast("比較できる保存済み画像がありません");
        };
        let keys = self.preview_keys();
        if keys.is_empty() {
            return self.toast("今の条件に合う保存済み画像がありません");
        }
        let title = format!(
            "{} · {} · {}",
            session.id,
            session.manifest.label,
            self.preview_scope()
        );
        let (sender, answer) = mpsc::channel();
        let job = Job {
            session: session.id.clone(),
            reply_root: reply_root.clone(),
            session_dir: session_dir.clone(),
            answer,
        };
        let wake = self.wake.clone();
        let spawned = thread::Builder::new()
            .name("verify-preview".to_owned())
            .spawn(move || {
                let images: Vec<_> = keys
                    .iter()
                    .filter_map(|key| session.evidence_item(key))
                    .map(|item| {
                        let criteria = session
                            .criteria
                            .iter()
                            .filter(|criterion| evidence::relates(&session, item, &criterion.id))
                            .map(|criterion| criterion.id.as_str())
                            .collect();
                        preview::Image {
                            evidence: item,
                            criteria,
                        }
                    })
                    .collect();
                let result = preview::prepare(
                    &store::reply_base(&session),
                    &reply_root,
                    &session_dir,
                    &title,
                    &images,
                );
                let _ = sender.send(result);
                wake();
            });
        if let Err(why) = spawned {
            return self.toast(format!("画像比較の準備を開始できません: {why}"));
        }
        self.preview_job = Some(job);
        self.rebuild(text);
    }

    /// Called after live snapshots are applied, so an old completion
    /// cannot open a page while a different session is arriving.
    pub(super) fn poll_preview(&mut self) {
        let Some(job) = &self.preview_job else {
            return;
        };
        if self.session.as_ref().map(|s| &s.id) != Some(&job.session)
            || self.reply_root.as_ref() != Some(&job.reply_root)
            || self.session_dir.as_ref() != Some(&job.session_dir)
        {
            self.preview_job = None;
            self.built = None;
            return;
        }
        let result = match job.answer.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("画像比較の準備が中断されました".to_owned()),
        };
        self.preview_job = None;
        self.built = None;
        match result.and_then(preview::Prepared::publish) {
            Ok(path) => self.open(Open::File(path)),
            Err(why) => self.toast(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use verify::model::types::{Evidence, EvidenceBody, Media, Stored};

    fn image(key: &str, criterion: &str) -> Evidence {
        Evidence {
            key: EvidenceKey::new(key),
            body: EvidenceBody::File {
                path: "screen.png".to_owned(),
                caption: None,
                media: Some(Media::Png),
                marks: vec![],
                shows: vec![criterion.to_owned()],
            },
            stored: Some(Stored {
                path: "screen.png".into(),
                bytes: 8,
                digest: 0,
            }),
            rerun: None,
            attached: None,
        }
    }

    #[test]
    fn selection_and_view_filters_define_the_comparison_without_mutating_them() {
        let mut session = crate::demo::session();
        let first = image("codex/r1/first", "AC-1");
        let second = image("claude/r2/second", "AC-2");
        let mut missing = image("codex/r1/missing", "AC-1");
        missing.stored = None;
        let mut video = image("codex/r1/video", "AC-1");
        if let EvidenceBody::File { media, .. } = &mut video.body {
            *media = Some(Media::Mp4);
        }
        session.evidence = vec![first.clone(), second.clone(), missing, video];
        let filters = evidence::Filters::default();
        assert_eq!(
            image_keys(&session, &filters, None, None),
            vec![first.key.clone(), second.key.clone()]
        );
        assert_eq!(
            image_keys(&session, &filters, None, Some("AC-1")),
            vec![first.key.clone()]
        );
        let filters = evidence::Filters {
            owner: Some("codex".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            image_keys(&session, &filters, None, None),
            vec![first.key.clone()]
        );
        let focus = evidence::Focus {
            title: "引用".to_owned(),
            keys: vec![second.key.clone(), second.key.clone()],
        };
        assert_eq!(
            image_keys(&session, &filters, Some(&focus), None),
            vec![second.key]
        );
        assert!(image_keys(&session, &filters, Some(&focus), Some("AC-1")).is_empty());
        assert_eq!(filters.owner.as_deref(), Some("codex"));
    }

    #[test]
    fn completion_opens_only_the_captured_session_and_leave_drops_the_receiver() {
        let mut screen = Verify::demo();
        let dir =
            std::env::temp_dir().join(format!("noa-verify-preview-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image-comparison.html");
        let prepare = |title: &str| {
            let bytes = b"\x89PNG\r\n\x1a\n";
            let mut item = image("codex/r1/image", "AC-1");
            item.stored.as_mut().unwrap().digest = verify::capture::digest_bytes(bytes);
            std::fs::write(dir.join("screen.png"), bytes).unwrap();
            preview::prepare(
                &dir,
                &dir,
                &dir,
                title,
                &[preview::Image {
                    evidence: &item,
                    criteria: vec!["AC-1"],
                }],
            )
            .unwrap()
        };
        screen.reply_root = Some(dir.clone());
        screen.session_dir = Some(dir.clone());
        let opened = Rc::new(RefCell::new(Vec::new()));
        let record = opened.clone();
        screen.opener = Box::new(move |request| {
            record.borrow_mut().push(request.clone());
            Ok(())
        });
        let enqueue = |screen: &mut Verify, id: SessionId| {
            let (sender, answer) = mpsc::channel();
            screen.preview_job = Some(Job {
                session: id,
                reply_root: dir.clone(),
                session_dir: dir.clone(),
                answer,
            });
            sender
        };
        let id = screen.session.as_ref().unwrap().id.clone();
        let sender = enqueue(&mut screen, id.clone());
        assert!(sender.send(Ok(prepare("first"))).is_ok());
        screen.poll_preview();
        assert_eq!(*opened.borrow(), vec![Open::File(path.clone())]);
        assert!(screen.preview_job.is_none());
        let sender = enqueue(&mut screen, SessionId::new("another-session"));
        assert!(sender.send(Ok(prepare("stale"))).is_ok());
        screen.poll_preview();
        assert_eq!(opened.borrow().len(), 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("<title>画像比較 — first</title>")
        );
        let sender = enqueue(&mut screen, id);
        screen.leave(&mut TextStore::new(Default::default()));
        assert!(sender.send(Ok(prepare("cancelled"))).is_err());
        assert!(screen.preview_job.is_none());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
