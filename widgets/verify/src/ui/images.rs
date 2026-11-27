//! One bounded decoder worker and a small cache for the visible evidence scope.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use noa_gpui_core::style::{Size as LayoutSize, length, percent};
use noa_gpui_core::widgets::{ImageScaling, Label, image_rect};
use noa_gpui_core::{PaintCx, Role, Style, UiTree, Widget, WidgetId};
use noa_gpui_scene::{Color, Size};

use verify::model::types::{Evidence, EvidenceKey, Media, Stored};
use verify::thumbnail::{self, Sample};

pub(super) const MAX_CACHE: usize = 16;
const MAX_DRAWN: usize = 24;

#[derive(Clone, PartialEq, Eq)]
struct Signature {
    key: EvidenceKey,
    stored: Stored,
    media: Media,
}

impl Signature {
    fn of(item: &Evidence) -> Option<Self> {
        let media = item.media().filter(|m| thumbnail::supported(*m))?;
        Some(Self {
            key: item.key.clone(),
            stored: item.stored.clone()?,
            media,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Context {
    base: PathBuf,
    reply: PathBuf,
}

struct Entry {
    signature: Signature,
    result: Option<Result<Arc<Sample>, String>>,
}
struct Job {
    generation: u64,
    context: Context,
    items: Vec<Evidence>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
struct Answer {
    generation: u64,
    key: EvidenceKey,
    result: Result<Arc<Sample>, String>,
}

#[derive(Default)]
pub(super) struct Cache {
    context: Option<Context>,
    scope: Vec<Signature>,
    entries: BTreeMap<EvidenceKey, Entry>,
    generation: Arc<AtomicU64>,
    sender: Option<mpsc::SyncSender<Job>>,
    receiver: Option<mpsc::Receiver<Answer>>,
    pending: Option<Job>,
    drawn: Cell<usize>,
}

impl Cache {
    pub(super) fn clear(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.context = None;
        self.scope.clear();
        self.entries.clear();
        self.pending = None;
    }

    pub(super) fn ensure(
        &mut self,
        base: &Path,
        reply: &Path,
        items: &[&Evidence],
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        let context = Context {
            base: base.to_owned(),
            reply: reply.to_owned(),
        };
        let mut wanted = Vec::new();
        for item in items {
            if let Some(signature) = Signature::of(item)
                && !wanted.iter().any(|s: &Signature| s.key == signature.key)
            {
                wanted.push(signature);
                if wanted.len() == MAX_CACHE {
                    break;
                }
            }
        }
        if self.context.as_ref() == Some(&context) && self.scope == wanted {
            return;
        }
        if self.context.as_ref() != Some(&context) {
            self.entries.clear();
        }
        self.context = Some(context.clone());
        self.scope = wanted;
        self.entries
            .retain(|_, entry| self.scope.contains(&entry.signature));
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut missing = Vec::new();
        for signature in &self.scope {
            let entry = self
                .entries
                .entry(signature.key.clone())
                .or_insert_with(|| Entry {
                    signature: signature.clone(),
                    result: None,
                });
            if entry.result.is_none()
                && let Some(item) = items.iter().find(|item| item.key == signature.key)
            {
                missing.push((*item).clone());
            }
        }
        if missing.is_empty() {
            return;
        }
        if self.sender.is_none() {
            let (sender, jobs) = mpsc::sync_channel::<Job>(1);
            let (answers, receiver) = mpsc::sync_channel(MAX_CACHE);
            let current = self.generation.clone();
            let spawned = std::thread::Builder::new()
                .name("verify-thumbnails".into())
                .spawn(move || {
                    while let Ok(job) = jobs.recv() {
                        for item in job.items {
                            if current.load(Ordering::SeqCst) != job.generation {
                                break;
                            }
                            let result =
                                thumbnail::read(&job.context.base, &job.context.reply, &item)
                                    .map(|image| Arc::new(thumbnail::sample(&image)));
                            if current.load(Ordering::SeqCst) != job.generation {
                                break;
                            }
                            if answers
                                .send(Answer {
                                    generation: job.generation,
                                    key: item.key,
                                    result,
                                })
                                .is_err()
                            {
                                return;
                            }
                            (job.wake)();
                        }
                        (job.wake)();
                    }
                });
            if let Err(error) = spawned {
                for entry in self.entries.values_mut() {
                    entry.result = Some(Err(format!("画像の読込を開始できません: {error}")));
                }
                return;
            }
            self.sender = Some(sender);
            self.receiver = Some(receiver);
        }
        self.pending = Some(Job {
            generation,
            context,
            items: missing,
            wake,
        });
        self.flush();
    }

    fn flush(&mut self) {
        let Some(job) = self.pending.take() else {
            return;
        };
        let Some(sender) = &self.sender else {
            return;
        };
        match sender.try_send(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => self.pending = Some(job),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                for entry in self.entries.values_mut() {
                    if entry.result.is_none() {
                        entry.result = Some(Err("画像の読込が中断されました".into()));
                    }
                }
            }
        }
    }

    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(answer) => {
                        if answer.generation == self.generation.load(Ordering::SeqCst)
                            && let Some(entry) = self.entries.get_mut(&answer.key)
                        {
                            entry.result = Some(answer.result);
                            changed = true;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        for entry in self.entries.values_mut() {
                            if entry.result.is_none() {
                                entry.result = Some(Err("画像の読込が中断されました".into()));
                                changed = true;
                            }
                        }
                        break;
                    }
                }
            }
        }
        self.flush();
        changed
    }

    pub(super) fn begin_build(&self) {
        self.drawn.set(0);
    }

    pub(super) fn ready(&self, key: &EvidenceKey) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| matches!(entry.result, Some(Ok(_))))
    }

    pub(super) fn insert(&self, tree: &mut UiTree, host: WidgetId, item: &Evidence) -> bool {
        let Some(media) = item.media().filter(|m| thumbnail::supported(*m)) else {
            return false;
        };
        let Some(entry) = self.entries.get(&item.key) else {
            return false;
        };
        match &entry.result {
            Some(Ok(sample)) if self.drawn.get() < MAX_DRAWN => {
                self.drawn.set(self.drawn.get() + 1);
                let picture = tree.insert(
                    Some(host),
                    Thumbnail {
                        sample: sample.clone(),
                    },
                );
                tree.set_accessible_label(
                    picture,
                    format!(
                        "{}: {}×{} の画像から作成した縮小プレビュー",
                        item.key, sample.original.0, sample.original.1
                    ),
                );
                tree.insert(
                    Some(host),
                    Label::new(format!(
                        "{} · {}×{} · 縮小表示",
                        media.name(),
                        sample.original.0,
                        sample.original.1
                    ))
                    .font_size(crate::appearance::font(10.0))
                    .wrap(true)
                    .max_lines(2)
                    .color(Role::Muted),
                );
            }
            Some(Ok(_)) => {
                tree.insert(
                    Some(host),
                    Label::new("表示上限 · 拡大で確認")
                        .font_size(crate::appearance::font(11.0))
                        .wrap(true),
                );
            }
            Some(Err(error)) => {
                tree.insert(
                    Some(host),
                    Label::new(format!("{} · プレビュー不可\n{error}", media.name()))
                        .font_size(crate::appearance::font(10.0))
                        .wrap(true)
                        .max_lines(4),
                );
            }
            None => {
                tree.insert(
                    Some(host),
                    Label::new(format!("{} · 画像を読み込み中…", media.name()))
                        .font_size(crate::appearance::font(11.0)),
                );
            }
        }
        true
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl super::Verify {
    pub(super) fn sync_thumbnails(&mut self) {
        let (Some(session), Some(reply)) = (&self.session, &self.reply_root) else {
            self.thumbnails.clear();
            return;
        };
        let items: Vec<_> = if let Some(focus) = &self.evidence_focus {
            focus
                .keys
                .iter()
                .filter_map(|key| session.evidence_item(key))
                .collect()
        } else if matches!(
            self.tab,
            super::CenterTab::Evidence | super::CenterTab::Gallery
        ) {
            super::evidence::shown(session, &self.evidence_filters)
        } else if let Some(super::Selected::Criterion(id)) = &self.selected {
            session
                .evidence
                .iter()
                .filter(|item| super::evidence::relates(session, item, id))
                .collect()
        } else {
            session.evidence.iter().collect()
        };
        self.thumbnails.ensure(
            &verify::store::reply_base(session),
            reply,
            &items,
            self.wake.clone(),
        );
    }
}

struct Thumbnail {
    sample: Arc<Sample>,
}

impl Widget for Thumbnail {
    fn style(&self) -> Style {
        Style {
            size: LayoutSize {
                width: percent(1.0),
                height: length(84.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let natural = Size::new(self.sample.size.0 as f32, self.sample.size.1 as f32);
        let area = image_rect(ImageScaling::Fit, natural, cx.bounds());
        let (sx, sy) = (area.width / natural.width, area.height / natural.height);
        for run in &self.sample.runs {
            let [r, g, b, a] = run.color;
            if a == 0 {
                continue;
            }
            cx.fill_rect(
                noa_gpui_scene::Rect::new(
                    area.x + run.x as f32 * sx,
                    area.y + run.y as f32 * sy,
                    run.width as f32 * sx,
                    sy,
                ),
                Color::rgba(
                    r as f32 / 255.0,
                    g as f32 / 255.0,
                    b as f32 / 255.0,
                    a as f32 / 255.0,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verify::model::types::EvidenceBody;

    #[test]
    fn cache_scope_is_bounded_and_invalidated_when_the_session_changes() {
        let items: Vec<_> = (0..MAX_CACHE + 10)
            .map(|n| Evidence {
                key: EvidenceKey::new(format!("user/{n}")),
                body: EvidenceBody::File {
                    path: format!("{n}.png"),
                    caption: None,
                    media: Some(Media::Png),
                    marks: vec![],
                    shows: vec![],
                },
                stored: Some(Stored {
                    path: format!("{n}.png").into(),
                    bytes: 1,
                    digest: 0,
                }),
                rerun: None,
                attached: None,
            })
            .collect();
        let refs: Vec<_> = items.iter().collect();
        let mut cache = Cache::default();
        cache.ensure(
            Path::new("/missing"),
            Path::new("/missing/a"),
            &refs,
            Arc::new(|| {}),
        );
        assert_eq!(cache.entries.len(), MAX_CACHE);
        let first = cache.generation.load(Ordering::SeqCst);
        cache.ensure(
            Path::new("/missing"),
            Path::new("/missing/a"),
            &refs,
            Arc::new(|| {}),
        );
        assert_eq!(
            cache.generation.load(Ordering::SeqCst),
            first,
            "stable scopes never re-enqueue or evict on every frame"
        );
        cache.ensure(
            Path::new("/missing"),
            Path::new("/missing/b"),
            &refs[..1],
            Arc::new(|| {}),
        );
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.generation.load(Ordering::SeqCst) > first);
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn repeated_references_do_not_exceed_the_drawing_budget() {
        let item = Evidence {
            key: EvidenceKey::new("user/one"),
            body: EvidenceBody::File {
                path: "one.png".into(),
                caption: None,
                media: Some(Media::Png),
                marks: vec![],
                shows: vec![],
            },
            stored: Some(Stored {
                path: "one.png".into(),
                bytes: 1,
                digest: 0,
            }),
            rerun: None,
            attached: None,
        };
        let mut cache = Cache::default();
        cache.entries.insert(
            item.key.clone(),
            Entry {
                signature: Signature::of(&item).unwrap(),
                result: Some(Ok(Arc::new(Sample {
                    original: (1, 1),
                    size: (1, 1),
                    runs: vec![thumbnail::Run {
                        x: 0,
                        y: 0,
                        width: 1,
                        color: [255; 4],
                    }],
                }))),
            },
        );
        let mut tree = UiTree::new();
        let root = tree.insert(None, noa_gpui_core::widgets::Container::column());
        for _ in 0..MAX_DRAWN + 20 {
            assert!(cache.insert(&mut tree, root, &item));
        }
        assert_eq!(cache.drawn.get(), MAX_DRAWN);
        assert_eq!(
            tree.children(root)
                .iter()
                .filter(|id| tree.get::<Thumbnail>(**id).is_some())
                .count(),
            MAX_DRAWN
        );
    }
}
