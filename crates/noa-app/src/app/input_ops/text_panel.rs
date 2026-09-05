use super::super::*;

impl App {
    pub(in crate::app) fn open_agent_workflow_guide(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let Some((window_id, pane_id)) = self.theme_settings.as_ref().and_then(|session| {
                self.windows
                    .get(&session.window_id)
                    .map(|state| (session.window_id, state.focused_pane))
            }) else {
                return;
            };
            if let Some(panel) = &self.text_panel {
                if !panel.can_close() {
                    return;
                }
                if let Some((pane, text)) = panel.draft() {
                    self.prompt_drafts.insert(pane, text);
                }
            }
            // Embed the guide so packaged apps work without a repository checkout.
            match crate::text_panel::TextPanel::open(
                "Agent Workflows",
                include_str!("../../../../../docs/AGENT_WORKFLOW.md"),
                crate::text_panel::TextPanelMode::Guide,
                window_id,
                pane_id,
                None,
                self.proxy.clone(),
            ) {
                Ok(panel) => self.text_panel = Some(panel),
                Err(err) => log::warn!("could not open agent workflow guide: {err}"),
            }
        }
    }

    pub(in crate::app) fn open_file_preview(
        &self,
        window_id: WindowId,
        path: std::path::PathBuf,
        line: Option<u32>,
    ) {
        let Some(pane_id) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_pane)
        else {
            return;
        };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let title = format!(
                "File Preview — {}{}",
                path.display(),
                line.map(|n| format!(":{n}")).unwrap_or_default()
            );
            let read = || -> std::io::Result<String> {
                use std::os::unix::fs::OpenOptionsExt;
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(std::io::Error::other(
                        "preview requires a regular text file",
                    ));
                }
                let mut bytes = Vec::new();
                file.take(crate::text_panel::TEXT_LIMIT as u64 + 1)
                    .read_to_end(&mut bytes)?;
                let truncated = bytes.len() > crate::text_panel::TEXT_LIMIT;
                if truncated {
                    bytes.truncate(crate::text_panel::TEXT_LIMIT);
                }
                let text = match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(err) if truncated && err.utf8_error().error_len().is_none() => {
                        let end = err.utf8_error().valid_up_to();
                        String::from_utf8(err.into_bytes()[..end].to_vec()).unwrap()
                    }
                    Err(_) => return Err(std::io::Error::other("file is not UTF-8 text")),
                };
                Ok(if truncated {
                    format!("[Preview limited to 1 MiB]\n\n{text}")
                } else {
                    text
                })
            };
            let text = read().unwrap_or_else(|err| format!("Could not preview this file: {err}"));
            let _ = proxy.send_event(UserEvent::FilePreview {
                window_id,
                pane_id,
                title,
                text,
            });
        });
    }

    pub(in crate::app) fn show_file_preview(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        title: String,
        text: String,
    ) {
        #[cfg(target_os = "macos")]
        if self.resolve_pane_window(window_id, pane_id).is_some() {
            if let Some(panel) = &self.text_panel {
                if !panel.can_close() {
                    return;
                }
                if let Some((pane, text)) = panel.draft() {
                    self.prompt_drafts.insert(pane, text);
                }
            }
            match crate::text_panel::TextPanel::open(
                &title,
                &text,
                crate::text_panel::TextPanelMode::File,
                window_id,
                pane_id,
                None,
                self.proxy.clone(),
            ) {
                Ok(panel) => self.text_panel = Some(panel),
                Err(err) => log::warn!("could not open file preview: {err}"),
            }
        }
    }

    pub(in crate::app) fn return_from_text_panel(&mut self, window_id: WindowId, pane_id: PaneId) {
        let Some(window_id) = self.resolve_pane_window(window_id, pane_id) else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.text_panel {
            panel.close();
        }
        self.focus_pane(window_id, pane_id);
        self.snap_pane_viewport_to_bottom(window_id, pane_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.focus_window();
        }
    }

    pub(in crate::app) fn open_text_panel(&mut self, editable: bool) {
        #[cfg(target_os = "macos")]
        {
            let command = if editable {
                AppCommand::ComposePrompt
            } else {
                AppCommand::ReadOutput
            };
            let Some((window_id, pane_id)) = self.resolve_pane_command_target(command) else {
                return;
            };
            if let Some(panel) = &self.text_panel {
                if !panel.can_close() {
                    return;
                }
                if let Some((pane, text)) = panel.draft() {
                    self.prompt_drafts.insert(pane, text);
                }
            }
            let id = Self::session_card_id(window_id, pane_id);
            let card = self.session_store.get(&id);
            let process = card.and_then(|card| card.process.clone());
            let context = card
                .map(|card| {
                    format!(
                        "{} · {} · {}",
                        card.display_name(),
                        card.branch.as_deref().unwrap_or(""),
                        card.cwd
                    )
                })
                .unwrap_or_else(|| "Terminal".to_string());
            let title = format!(
                "{} — {}",
                if editable {
                    "Compose Prompt"
                } else {
                    "Output Snapshot"
                },
                context
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect::<String>()
            );
            let text = if editable {
                self.prompt_drafts
                    .get(&pane_id)
                    .filter(|text| !text.is_empty())
                    .cloned()
                    .unwrap_or_else(|| {
                        self.windows
                            .get(&window_id)
                            .and_then(|state| state.surfaces.get(&pane_id))
                            .and_then(|surface| surface.terminal.lock().selected_text())
                            .map(|selection| quote_selection(&selection))
                            .unwrap_or_default()
                    })
            } else {
                let Some(surface) = self
                    .windows
                    .get(&window_id)
                    .and_then(|state| state.surfaces.get(&pane_id))
                else {
                    return;
                };
                let mut terminal = surface.terminal.lock();
                terminal.selected_text().unwrap_or_else(|| {
                    terminal
                        .scrollback_text_tail(crate::text_panel::TEXT_LIMIT - 128)
                        .map(|(text, truncated)| {
                            if truncated {
                                format!("[Earlier output omitted]\n\n{text}")
                            } else {
                                text
                            }
                        })
                        .unwrap_or_default()
                })
            };
            match crate::text_panel::TextPanel::open(
                &title,
                &text,
                if editable {
                    crate::text_panel::TextPanelMode::Compose
                } else {
                    crate::text_panel::TextPanelMode::Output
                },
                window_id,
                pane_id,
                process,
                self.proxy.clone(),
            ) {
                Ok(panel) => self.text_panel = Some(panel),
                Err(err) => log::warn!("could not open text panel: {err}"),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = editable;
            log::warn!("text panels require macOS");
        }
    }

    pub(in crate::app) fn handle_text_panel_input(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        process: Option<String>,
        text: String,
        paste: bool,
    ) {
        let Some(window_id) = self.resolve_pane_window(window_id, pane_id) else {
            return;
        };
        let text = crate::text_panel::bounded_text(&text);
        self.prompt_drafts.insert(pane_id, text.clone());
        if !paste || text.is_empty() {
            return;
        }
        let current_process = self
            .session_store
            .get(&Self::session_card_id(window_id, pane_id))
            .and_then(|card| card.process.clone());
        if process != current_process {
            #[cfg(target_os = "macos")]
            if let Some(panel) = &self.text_panel {
                panel.set_title(
                    "Destination process changed — close and reopen Compose Prompt to review",
                );
            }
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.text_panel {
            panel.close();
        }
        self.focus_pane(window_id, pane_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.focus_window();
        }
        self.paste_text_to_pane_with_confirm_window(window_id, window_id, pane_id, text, false);
    }
}

fn quote_selection(selection: &str) -> String {
    let text = crate::text_panel::bounded_text(selection);
    let quoted = text
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    crate::text_panel::bounded_text(&format!("{quoted}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_log_is_quoted_without_changing_line_contents() {
        assert_eq!(
            quote_selection("error: 日本語\n  at src/main.rs:42"),
            "> error: 日本語\n>   at src/main.rs:42\n\n"
        );
    }
}
