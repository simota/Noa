//! Event-loop integration for the single display-only idle companion.

use super::*;
use crate::idle_ember::{Gate, Pose};

pub(super) fn is_user_activity(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { .. }
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Focused(true)
            | WindowEvent::Occluded(false)
            | WindowEvent::Ime(Ime::Preedit(..) | Ime::Commit(..))
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
    )
}

#[derive(Default)]
struct OverlayGate {
    command_palette: bool,
    theme_settings: bool,
    confirm_dialog: bool,
    search_prompt: bool,
    overview_visible: bool,
    process_monitor: bool,
    send_selection_picker: bool,
    remote_ui: bool,
    tab_title_prompt: bool,
    pane_drag: bool,
    active_split_drag: bool,
    sidebar_drag: bool,
    context_menu: bool,
    sidebar_rename: bool,
}

impl OverlayGate {
    fn active(&self) -> bool {
        self.command_palette
            || self.theme_settings
            || self.confirm_dialog
            || self.search_prompt
            || self.overview_visible
            || self.process_monitor
            || self.send_selection_picker
            || self.remote_ui
            || self.tab_title_prompt
            || self.pane_drag
            || self.active_split_drag
            || self.sidebar_drag
            || self.context_menu
            || self.sidebar_rename
    }
}

impl App {
    pub(super) fn any_overlay_active(&self, id: WindowId) -> bool {
        OverlayGate {
            command_palette: self
                .command_palette
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            theme_settings: self
                .theme_settings
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            confirm_dialog: self
                .confirm_dialog
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            search_prompt: self
                .search_prompt
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            overview_visible: self.overview_visible,
            process_monitor: self
                .process_monitor
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            send_selection_picker: self
                .send_selection_picker
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            remote_ui: self.remote_ui.as_ref().is_some_and(|s| s.window_id == id),
            tab_title_prompt: self
                .tab_title_prompt
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            pane_drag: self
                .overview_window
                .as_ref()
                .is_some_and(|s| s.pane_drag.is_some()),
            active_split_drag: self
                .windows
                .get(&id)
                .is_some_and(|s| s.active_split_drag.is_some()),
            sidebar_drag: self
                .windows
                .get(&id)
                .is_some_and(|s| s.sidebar_drag.is_some()),
            context_menu: self
                .context_menu
                .as_ref()
                .is_some_and(|s| s.window_id == id),
            sidebar_rename: self
                .sidebar_rename
                .as_ref()
                .is_some_and(|s| s.window_id == id),
        }
        .active()
    }

    fn idle_ember_gate(&self, id: WindowId) -> Option<Gate> {
        let state = self.windows.get(&id)?;
        let pane = state.focused_surface()?;
        Some(Gate {
            enabled_secs: self.config.ember_idle_after_secs,
            os_focused: self.os_focused == Some(id),
            occluded: state.occluded,
            is_main_window: !self.is_quick_terminal_window(id)
                && !self.is_scratch_terminal_window(id),
            overlay_active: self.any_overlay_active(id),
            ime_composing: pane.ime_state.preedit_active()
                || self
                    .modal_preedit
                    .as_ref()
                    .is_some_and(|p| p.window_id == id),
            focused_pane_last_output: pane.last_output_at,
        })
    }

    pub(super) fn clear_idle_ember(&mut self) {
        crate::idle_ember::clear_episode(&mut self.idle_ember, &mut self.idle_ember_deadline);
        self.idle_ember_geometry = None;
        self.clear_idle_ember_texture();
    }

    fn clear_idle_ember_texture(&mut self) {
        if let Some(gpu) = self.gpu.as_mut() {
            if gpu.idle_ember_texture.is_some()
                && let Some(card) = &gpu.idle_ember_card
            {
                card.pipeline.clear_texture_cache();
            }
            gpu.idle_ember_texture = None;
            gpu.idle_ember_pose = None;
            gpu.idle_ember_pixels.clear();
        }
    }

    pub(super) fn note_user_activity(&mut self) {
        let now = Instant::now();
        self.idle_ember_reset_at = now;
        self.idle_ember_deadline = None;
        let gate = self
            .idle_ember
            .as_ref()
            .and_then(|(id, _)| self.idle_ember_gate(*id));
        if let Some((id, ember)) = self.idle_ember.as_mut() {
            let id = *id;
            let was_visible = ember.visible();
            if let Some(gate) = &gate {
                ember.update_gate(gate);
            }
            ember.reset(now);
            if let Some(gate) = &gate {
                self.idle_ember_deadline = ember.tick(now, gate, crate::idle_ember::reduce_motion);
            }
            if was_visible {
                self.request_window_redraw(id);
            }
        }
        if self
            .idle_ember
            .as_ref()
            .is_none_or(|(_, ember)| !ember.visible())
        {
            self.clear_idle_ember_texture();
        }
    }

    pub(super) fn note_idle_ember_output(&mut self, id: WindowId, pane_id: PaneId) {
        let now = Instant::now();
        let Some(state) = self.windows.get_mut(&id) else {
            return;
        };
        if let Some(pane) = state.surfaces.get_mut(&pane_id) {
            pane.last_output_at = now;
        }
        if state.focused_pane != pane_id {
            return;
        }
        let gate = self.idle_ember_gate(id);
        if let Some((host, ember)) = self.idle_ember.as_mut()
            && *host == id
        {
            let visible = ember.visible();
            if let Some(gate) = &gate {
                ember.update_gate(gate);
            }
            ember.note_focused_output(now);
            if let Some(gate) = &gate {
                self.idle_ember_deadline = ember.tick(now, gate, crate::idle_ember::reduce_motion);
            }
            if visible {
                self.request_window_redraw(id);
            }
        }
        if self
            .idle_ember
            .as_ref()
            .is_none_or(|(_, ember)| !ember.visible())
        {
            self.clear_idle_ember_texture();
        }
    }

    fn idle_geometry(&self, id: WindowId) -> Option<(WindowId, PaneId, PaneRectApp)> {
        let s = self.windows.get(&id)?;
        Some((id, s.focused_pane, s.focused_surface()?.rect))
    }

    pub(super) fn tick_idle_ember(&mut self) -> Option<Instant> {
        if self.config.ember_idle_after_secs == 0 {
            self.clear_idle_ember();
            return None;
        }
        let now = Instant::now();
        let id = self.focused?;
        let gate = self.idle_ember_gate(id)?;
        if !gate.is_main_window {
            self.clear_idle_ember();
            return None;
        }
        let geometry = self.idle_geometry(id);
        if self.idle_ember_geometry.is_some() && self.idle_ember_geometry != geometry {
            self.note_user_activity();
        }
        if self.idle_ember.as_ref().is_none_or(|(host, _)| *host != id) {
            if let Some(old) =
                crate::idle_ember::select_host(&mut self.idle_ember, id, self.idle_ember_reset_at)
            {
                self.request_window_redraw(old);
            }
            self.idle_ember_deadline = None;
            self.clear_idle_ember_texture();
        }
        self.idle_ember_geometry = geometry;
        let (_, ember) = self.idle_ember.as_mut()?;
        let visible = ember.visible();
        let frame_due = self.idle_ember_deadline.is_some_and(|d| now >= d);
        let deadline = ember.tick(now, &gate, crate::idle_ember::reduce_motion);
        let visible_now = ember.visible();
        self.idle_ember_deadline = deadline;
        if needs_redraw(&gate, visible, visible_now, frame_due) {
            self.request_window_redraw(id);
        }
        if !visible_now {
            self.clear_idle_ember_texture();
        }
        deadline
    }

    pub(super) fn idle_ember_for_draw(&mut self, id: WindowId) -> Option<Pose> {
        if self.config.ember_idle_after_secs == 0
            || self.idle_ember.as_ref().is_none_or(|(host, _)| *host != id)
        {
            return None;
        }
        let gate = self.idle_ember_gate(id)?;
        let geometry = self.idle_geometry(id);
        if geometry != self.idle_ember_geometry {
            self.note_user_activity();
            return None;
        }
        let (host, ember) = self.idle_ember.as_mut()?;
        if *host != id {
            return None;
        }
        if !gate.allows_display() {
            ember.tick(Instant::now(), &gate, crate::idle_ember::reduce_motion);
            self.idle_ember_deadline = None;
            self.clear_idle_ember_texture();
            return None;
        }
        ember.visible().then(|| ember.pose(Instant::now()))
    }
}

fn needs_redraw(gate: &Gate, visible: bool, visible_now: bool, frame_due: bool) -> bool {
    (visible && !visible_now)
        || (gate.os_focused
            && !gate.occluded
            && (visible != visible_now || (visible_now && frame_due)))
}

fn sprite_format(target: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if target.is_srgb() {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

fn animated_y(rect: PaneRectApp, pane: PaneRectApp, dy: f32) -> u32 {
    ((rect.y as f32 + dy * rect.h as f32).round() as u32).min(pane.bottom().saturating_sub(rect.h))
}

pub(super) fn draw(
    gpu: &mut GpuState,
    state: &WindowState,
    target: &wgpu::TextureView,
    surface_size: PixelSize,
    pose: Pose,
) {
    let Some(pane) = state.focused_surface() else {
        return;
    };
    if pane.grid_size.cols < 20 || pane.grid_size.rows < 8 {
        return;
    }
    let metrics = gpu.fonts.get(state.font_px).metrics();
    let Some(rect) = crate::idle_ember::placement(
        pane.rect,
        metrics.cell_w,
        metrics.cell_h,
        state.window.scale_factor() as f32,
    ) else {
        return;
    };
    let size = PixelSize {
        w: rect.w.clamp(1, crate::ember_sprite::MAX_PIXELS),
        h: rect.h.clamp(1, crate::ember_sprite::MAX_PIXELS),
    };
    if gpu
        .idle_ember_card
        .as_ref()
        .is_none_or(|p| p.format != state.surface_config.format)
    {
        gpu.idle_ember_card = Some(OverviewChromeCardPipeline {
            format: state.surface_config.format,
            glass: false,
            pipeline: CardPipeline::new(
                &gpu.device,
                state.surface_config.format,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
        });
    }
    let format = sprite_format(state.surface_config.format);
    if gpu
        .idle_ember_texture
        .as_ref()
        .is_none_or(|(s, f, _, _)| *s != size || *f != format)
    {
        if let Some(card) = &gpu.idle_ember_card {
            card.pipeline.clear_texture_cache();
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("idle-ember"),
            size: wgpu::Extent3d {
                width: size.w.max(1),
                height: size.h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.idle_ember_texture = Some((size, format, texture, view));
        gpu.idle_ember_pose = None;
    }
    let (_, _, texture, view) = gpu.idle_ember_texture.as_ref().expect("created above");
    if gpu.idle_ember_pose != Some(pose) {
        crate::ember_sprite::raster(
            &mut gpu.idle_ember_pixels,
            size.w,
            size.h,
            metrics.cell_h * size.h as f32 / rect.h as f32,
            pose,
        );
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &gpu.idle_ember_pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.w * 4),
                rows_per_image: Some(size.h),
            },
            wgpu::Extent3d {
                width: size.w,
                height: size.h,
                depth_or_array_layers: 1,
            },
        );
        gpu.idle_ember_pose = Some(pose);
    }
    let style = CardStyle {
        background: [0.0; 4],
        border_color: [0.0; 4],
        focus_color: [0.0; 4],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: 0.0,
    };
    let placement = CardTexturePlacement {
        texture_view: view,
        x: rect.x,
        y: animated_y(rect, pane.rect, pose.dy),
        w: rect.w,
        h: rect.h,
        selected: false,
    };
    gpu.idle_ember_card
        .as_ref()
        .expect("created above")
        .pipeline
        .overlay_texture_cards_with_opacity(
            &gpu.device,
            &gpu.queue,
            target,
            surface_size,
            &style,
            &[placement],
            pose.alpha,
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_loss_mid_fade_requests_clear_redraw() {
        let now = Instant::now();
        let mut gate = Gate {
            enabled_secs: 10,
            os_focused: true,
            occluded: false,
            is_main_window: true,
            overlay_active: false,
            ime_composing: false,
            focused_pane_last_output: now,
        };
        let mut ember = crate::idle_ember::IdleEmber::new(now);
        let start = now + Duration::from_secs(10);
        ember.tick(start, &gate, || false);
        ember.reset(start + Duration::from_secs(1));
        ember.tick(start + Duration::from_millis(1100), &gate, || false);
        assert!(matches!(
            ember.state,
            crate::idle_ember::State::Dismissing { .. }
        ));
        let visible = ember.visible();
        gate.os_focused = false;
        assert_eq!(
            ember.tick(start + Duration::from_millis(1150), &gate, || false),
            None
        );
        assert_eq!(ember.state, crate::idle_ember::State::Waiting);
        assert!(needs_redraw(&gate, visible, ember.visible(), false));
        assert!(!needs_redraw(
            &gate,
            ember.visible(),
            ember.visible(),
            false
        ));
    }

    #[test]
    fn ember_gate_blocks_every_modal() {
        assert!(!OverlayGate::default().active());
        macro_rules! assert_blocks {
            ($($field:ident),+ $(,)?) => {
                $(assert!(OverlayGate { $field: true, ..Default::default() }.active(),
                    stringify!($field));)+
            };
        }
        assert_blocks!(
            command_palette,
            theme_settings,
            confirm_dialog,
            search_prompt,
            overview_visible,
            process_monitor,
            send_selection_picker,
            remote_ui,
            tab_title_prompt,
            pane_drag,
            active_split_drag,
            sidebar_drag,
            context_menu,
            sidebar_rename
        );
    }

    #[test]
    fn sprite_format_matches_surface_transfer_function() {
        assert_eq!(
            sprite_format(wgpu::TextureFormat::Bgra8Unorm),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            sprite_format(wgpu::TextureFormat::Bgra8UnormSrgb),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn animated_card_stays_inside_pane() {
        let pane = PaneRectApp::new(100, 200, 800, 600);
        let rect = crate::idle_ember::placement(pane, 10.0, 20.0, 1.0).unwrap();
        assert_eq!(animated_y(rect, pane, 0.25), pane.bottom() - rect.h);
        for ms in 0..=4800 {
            let y = animated_y(rect, pane, crate::idle_ember::pose(ms as f32 / 1000.0).dy);
            assert!(y >= pane.y && y + rect.h <= pane.bottom());
        }
    }

    #[test]
    fn reset_events_pass_through_pre_dispatch() {
        let device_id = winit::event::DeviceId::dummy();
        assert!(is_user_activity(&WindowEvent::MouseInput {
            device_id,
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }));
        assert!(is_user_activity(&WindowEvent::MouseWheel {
            device_id,
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: winit::event::TouchPhase::Moved,
        }));
        assert!(is_user_activity(&WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(1.0, 2.0),
        }));
        assert!(is_user_activity(&WindowEvent::Ime(Ime::Preedit(
            "a".into(),
            None
        ))));
        assert!(is_user_activity(&WindowEvent::Ime(Ime::Commit("a".into()))));
        assert!(is_user_activity(&WindowEvent::Focused(true)));
        assert!(is_user_activity(&WindowEvent::Occluded(false)));
        assert!(is_user_activity(&WindowEvent::Resized(PhysicalSize::new(
            800, 600
        ))));
        assert!(!is_user_activity(&WindowEvent::RedrawRequested));
        assert!(!is_user_activity(&WindowEvent::Focused(false)));
        assert!(!is_user_activity(&WindowEvent::Occluded(true)));
    }
}
