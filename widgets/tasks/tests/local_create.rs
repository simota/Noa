//! `local::create` publishes a task file whole (L1-2: "It writes via temp +
//! rename"; `skills/noa-tasks/SKILL.md`: "the widget never reads half a
//! file"): a reader listing `t-*.md` while tasks are made never sees an
//! empty or half-written one, and nothing but the tasks is left behind.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tasks::local::{self, LocalFile};
use tasks::model::types::State;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let path = std::env::temp_dir().join(format!("noa-tasks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Temp(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn ac10_create_publishes_the_whole_file_at_once_to_a_concurrent_reader() {
    let tmp = Temp::new("create-whole");
    let repo = tmp.0.clone();
    let dir = local::tasks_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    // Large enough that writing it in place takes many write calls' time.
    let body = "本文の行です。0123456789abcdef\n".repeat(25_000);
    let file = LocalFile {
        title: "大きなタスク".into(),
        state: State::Open,
        labels: vec!["big".into()],
        extra: Vec::new(),
        body,
    };
    let expected = local::render(&file);

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (dir, done, expected) = (dir.clone(), done.clone(), expected.clone());
        std::thread::spawn(move || {
            let mut seen = 0usize;
            while !done.load(Ordering::SeqCst) {
                for entry in fs::read_dir(&dir).unwrap().flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !(name.starts_with("t-") && name.ends_with(".md")) {
                        continue;
                    }
                    // Gone since it was listed is fine; a part of it is not.
                    if let Ok(text) = fs::read_to_string(entry.path()) {
                        assert!(
                            text == expected,
                            "{name}: read {} of {} bytes",
                            text.len(),
                            expected.len()
                        );
                        seen += 1;
                    }
                }
            }
            seen
        })
    };
    let mut ids = Vec::new();
    for n in 0..300 {
        ids.push(local::create(&repo, &file, 1_000 + n).unwrap());
    }
    done.store(true, Ordering::SeqCst);
    let seen = reader.join().expect("the reader saw only whole files");
    assert!(seen > 0, "the reader read the tasks");

    // Only the tasks are left: no temporary file.
    let mut want: Vec<String> = ids.iter().map(|id| format!("{id}.md")).collect();
    want.sort();
    assert_eq!(names(&dir), want);
    for id in &ids {
        assert_eq!(local::read(&repo, id).unwrap(), expected);
    }
}

#[test]
fn ac10_create_never_replaces_a_name_taken_by_a_file_or_a_link() {
    let tmp = Temp::new("create-taken");
    let repo = tmp.0.clone();
    let dir = local::tasks_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let file = LocalFile {
        title: "new".into(),
        state: State::Open,
        labels: Vec::new(),
        extra: Vec::new(),
        body: String::new(),
    };
    fs::write(dir.join("t-5.md"), "theirs").unwrap();
    let outside = repo.join("outside.md");
    std::os::unix::fs::symlink(&outside, dir.join("t-5-2.md")).unwrap();
    assert_eq!(local::create(&repo, &file, 5).unwrap(), "t-5-3");
    assert_eq!(fs::read_to_string(dir.join("t-5.md")).unwrap(), "theirs");
    assert!(!outside.exists(), "the link was not written through");
    assert_eq!(names(&dir), ["t-5-2.md", "t-5-3.md", "t-5.md"]);
}
