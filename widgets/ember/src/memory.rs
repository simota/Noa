use crate::context::normalize_scope;
use crate::model::{MAX_MEMORIES, MemoryEntry, Scope, State, now};

pub fn save(state: &mut State, id: Option<&str>, scope: &Scope, text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > 4096 {
        return Err("記憶は空でない4 KiB以内の文章にしてください".into());
    }
    let scope = normalize_scope(scope)?;
    if let Some(id) = id {
        let entry = state
            .memories
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or("記憶がありません")?;
        entry.text = text.trim().into();
        entry.scope = scope;
        entry.revision = entry.revision.checked_add(1).ok_or("記憶の版の上限です")?;
        // Replies and summaries made with the prior preference are no longer
        // automatically sent after the user changes its meaning or scope.
        state.context_epoch = state
            .context_epoch
            .checked_add(1)
            .ok_or("文脈の版の上限です")?;
    } else {
        if state.memories.len() >= MAX_MEMORIES {
            return Err("記憶は200件までです。不要な記憶を整理してください".into());
        }
        let id = state.id("memory")?;
        state.memories.push(MemoryEntry {
            id,
            scope,
            text: text.trim().into(),
            revision: 1,
            created_at: now(),
        });
    }
    Ok(())
}

pub fn delete(state: &mut State, id: &str) -> Result<(), String> {
    let index = state
        .memories
        .iter()
        .position(|m| m.id == id)
        .ok_or("記憶がありません")?;
    state.memories.remove(index);
    state.context_epoch = state
        .context_epoch
        .checked_add(1)
        .ok_or("文脈の版の上限です")?;
    Ok(())
}
