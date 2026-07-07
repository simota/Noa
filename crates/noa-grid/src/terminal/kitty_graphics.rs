use crate::kitty::{KittyError, KittyImage, TransmitStep};
use crate::kitty_placeholder::scan_row;
use crate::screen::{KittyPlacement, VisibleKittyPlacement};
use noa_vt::{KittyAction, KittyDelete, KittyGraphicsCommand};

use super::Terminal;

impl Terminal {
    /// Emit a Kitty graphics reply (`ESC _ G i=<id>[,I=..][,p=..];<body> ESC \`).
    pub(super) fn push_apc_response(&mut self, id: u32, number: u32, placement: u32, body: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1b_G");
        self.pending_writes.extend_from_slice(b"i=");
        self.pending_writes
            .extend_from_slice(id.to_string().as_bytes());
        if number != 0 {
            self.pending_writes.extend_from_slice(b",I=");
            self.pending_writes
                .extend_from_slice(number.to_string().as_bytes());
        }
        if placement != 0 {
            self.pending_writes.extend_from_slice(b",p=");
            self.pending_writes
                .extend_from_slice(placement.to_string().as_bytes());
        }
        self.pending_writes.push(b';');
        self.pending_writes.extend_from_slice(body);
        self.pending_writes.extend_from_slice(b"\x1b\\");
    }

    /// Decide whether to emit a Kitty graphics reply, honoring the quiet level
    /// (`q=`) and the "no reply when neither `i=` nor `I=` was given" rule, then
    /// emit it. `assigned_id` is the id the store actually used (may differ from
    /// `req_id` when auto-assigned).
    pub(super) fn kitty_reply(
        &mut self,
        req_id: u32,
        req_number: u32,
        assigned_id: u32,
        placement: u32,
        quiet: u8,
        result: Result<(), KittyError>,
    ) {
        if req_id == 0 && req_number == 0 {
            return;
        }
        let ok = result.is_ok();
        if quiet >= 2 || (quiet >= 1 && ok) {
            return;
        }
        let body: &[u8] = match &result {
            Ok(()) => b"OK",
            Err(e) => e.reply_body().as_bytes(),
        };
        let id = if assigned_id != 0 {
            assigned_id
        } else {
            req_id
        };
        self.push_apc_response(id, req_number, placement, body);
    }

    /// Feed a data-carrying Kitty graphics command (`a=t`/`a=T`/`a=q`) into the
    /// image store, then—for `a=T`—place it, replying on completion.
    pub(super) fn kitty_transmit(&mut self, cmd: KittyGraphicsCommand) {
        match self.kitty_images.transmit(&cmd) {
            TransmitStep::NeedMore => {}
            TransmitStep::Done(done) => {
                let ctrl = done.ctrl;
                let result = done.result.and_then(|id| {
                    if ctrl.action == KittyAction::TransmitAndDisplay {
                        self.kitty_place(&ctrl, id).map(|()| id)
                    } else {
                        Ok(id)
                    }
                });
                let assigned = *result.as_ref().unwrap_or(&ctrl.image_id);
                if result.is_ok() {
                    self.kitty_images
                        .enforce_quota(&self.referenced_image_ids());
                }
                self.kitty_reply(
                    ctrl.image_id,
                    ctrl.image_number,
                    assigned,
                    ctrl.placement_id,
                    ctrl.quiet,
                    result.map(|_| ()),
                );
            }
        }
    }

    /// Display a stored image (`a=p`), placing it on the active screen.
    pub(super) fn kitty_put(&mut self, cmd: &KittyGraphicsCommand) {
        let image_id = self.resolve_put_image(cmd);
        let result = match image_id {
            Some(id) => self.kitty_place(cmd, id),
            None => Err(KittyError::NoEnt),
        };
        let assigned = image_id.unwrap_or(cmd.image_id);
        self.kitty_reply(
            cmd.image_id,
            cmd.image_number,
            assigned,
            cmd.placement_id,
            cmd.quiet,
            result,
        );
    }

    /// Resolve the image an `a=p` command targets: `i=` id, else `I=` number.
    fn resolve_put_image(&self, cmd: &KittyGraphicsCommand) -> Option<u32> {
        if cmd.image_id != 0 {
            return self.kitty_images.get(cmd.image_id).map(|_| cmd.image_id);
        }
        if cmd.image_number != 0 {
            return self
                .kitty_images
                .get_by_number(cmd.image_number)
                .map(|img| img.id);
        }
        None
    }

    /// Create a placement of image `image_id` on the active screen from `ctrl`,
    /// resolving the effective cell span and moving the cursor unless `C=1`.
    fn kitty_place(
        &mut self,
        ctrl: &KittyGraphicsCommand,
        image_id: u32,
    ) -> Result<(), KittyError> {
        let (img_w, img_h) = match self.kitty_images.get(image_id) {
            Some(img) => (img.width, img.height),
            None => return Err(KittyError::NoEnt),
        };
        let (cell_w, cell_h) = (self.cell_width_px, self.cell_height_px);
        if cell_w == 0 || cell_h == 0 {
            // Cell metrics arrive with the first resize; without them the cell
            // span is undefined.
            return Err(KittyError::Invalid);
        }

        let src_w = if ctrl.src_w != 0 {
            ctrl.src_w
        } else {
            img_w.saturating_sub(ctrl.src_x)
        };
        let src_h = if ctrl.src_h != 0 {
            ctrl.src_h
        } else {
            img_h.saturating_sub(ctrl.src_y)
        };
        if src_w == 0 || src_h == 0 {
            return Err(KittyError::Invalid);
        }
        let cols = if ctrl.columns != 0 {
            ctrl.columns
        } else {
            src_w.div_ceil(cell_w)
        };
        let rows = if ctrl.rows != 0 {
            ctrl.rows
        } else {
            src_h.div_ceil(cell_h)
        };
        let cols = cols.clamp(1, u16::MAX as u32) as u16;
        let rows = rows.clamp(1, u16::MAX as u32) as u16;
        let cropped = ctrl.src_x != 0 || ctrl.src_y != 0 || ctrl.src_w != 0 || ctrl.src_h != 0;
        let src = cropped.then_some([ctrl.src_x, ctrl.src_y, src_w, src_h]);
        let cell_x_off = ctrl.cell_x_off.min(cell_w - 1) as u16;
        let cell_y_off = ctrl.cell_y_off.min(cell_h - 1) as u16;

        let screen = self.active_mut();
        let anchor_abs_row =
            screen.rows_evicted() + screen.scrollback_len() + screen.cursor.y as usize;
        let anchor_col = screen.cursor.x;
        screen.insert_kitty_placement(KittyPlacement {
            image_id,
            placement_id: ctrl.placement_id,
            anchor_abs_row,
            anchor_col,
            cell_x_off,
            cell_y_off,
            src,
            cols,
            rows,
            z: ctrl.z_index,
            is_virtual: ctrl.virtual_placement,
        });

        if !ctrl.cursor_no_move && !ctrl.virtual_placement {
            // Move to the image's last row, one column past its right edge.
            for _ in 1..rows {
                screen.index();
            }
            let max_x = screen.cols.saturating_sub(1);
            screen.cursor.x = (anchor_col as usize + cols as usize).min(max_x as usize) as u16;
            screen.cursor.pending_wrap = false;
        }
        Ok(())
    }

    /// Delete placements (and, for uppercase specifiers, image data) per `a=d`.
    pub(super) fn kitty_delete(&mut self, cmd: &KittyGraphicsCommand) {
        let Some(spec) = cmd.delete else {
            return;
        };
        if let KittyDelete::AnimationFrames { .. } = spec {
            self.kitty_reply(
                cmd.image_id,
                cmd.image_number,
                cmd.image_id,
                cmd.placement_id,
                cmd.quiet,
                Err(KittyError::Unsupported),
            );
            return;
        }
        let free = kitty_delete_frees(spec);
        let number_ids: Vec<u32> = match spec {
            KittyDelete::ByNumber { .. } => self.kitty_images.ids_with_number(cmd.image_number),
            _ => Vec::new(),
        };
        let (cursor_abs, cursor_col) = {
            let s = self.active();
            (
                s.rows_evicted() + s.scrollback_len() + s.cursor.y as usize,
                s.cursor.x,
            )
        };
        let live_top = {
            let s = self.active();
            s.rows_evicted() + s.scrollback_len()
        };
        // Cell coords in `a=d` are 1-based grid columns/rows; convert to
        // session-absolute for intersection tests.
        let target_col = cmd.src_x.saturating_sub(1) as u16;
        let target_abs = live_top + cmd.src_y.saturating_sub(1) as usize;

        let removed = self.active_mut().delete_kitty_placements(|p| match spec {
            KittyDelete::All { .. } => true,
            KittyDelete::ById { .. } => {
                p.image_id == cmd.image_id
                    && (cmd.placement_id == 0 || p.placement_id == cmd.placement_id)
            }
            KittyDelete::ByNumber { .. } => number_ids.contains(&p.image_id),
            KittyDelete::AtCursor { .. } => p.covers_abs(cursor_abs, cursor_col),
            KittyDelete::AtCell { .. } => p.covers_abs(target_abs, target_col),
            KittyDelete::AtCellZ { .. } => {
                p.covers_abs(target_abs, target_col) && p.z == cmd.z_index
            }
            KittyDelete::ByIdRange { .. } => p.image_id >= cmd.src_x && p.image_id <= cmd.src_y,
            KittyDelete::ByColumn { .. } => {
                target_col >= p.anchor_col && target_col < p.anchor_col.saturating_add(p.cols)
            }
            KittyDelete::ByRow { .. } => {
                target_abs >= p.anchor_abs_row && target_abs < p.anchor_abs_row + p.rows as usize
            }
            KittyDelete::ByZ { .. } => p.z == cmd.z_index,
            KittyDelete::AnimationFrames { .. } => false,
        });

        if free {
            for id in removed {
                if !self.image_referenced(id) {
                    self.kitty_images.remove(id);
                }
            }
        }
    }

    /// Whether any placement on either screen still references image `id`.
    fn image_referenced(&self, id: u32) -> bool {
        self.primary
            .kitty_placements
            .iter()
            .chain(self.alt.iter().flat_map(|s| s.kitty_placements.iter()))
            .any(|p| p.image_id == id)
    }

    /// Image ids kept alive by a placement on either screen (spared by the quota
    /// sweep).
    fn referenced_image_ids(&self) -> std::collections::HashSet<u32> {
        self.primary
            .kitty_placements
            .iter()
            .chain(self.alt.iter().flat_map(|s| s.kitty_placements.iter()))
            .map(|p| p.image_id)
            .collect()
    }

    /// Placements on the active screen projected into the current viewport,
    /// sorted by z ascending. The renderer pairs each with [`Terminal::kitty_image`].
    pub fn kitty_visible_placements(&self) -> Vec<VisibleKittyPlacement> {
        self.active().visible_kitty_placements()
    }

    /// Placements synthesized from Unicode placeholder cells (`U+10EEEE`) that
    /// reference a virtual placement (`U=1`) on the active screen, projected into
    /// the current viewport. Each returned placement covers one fused run of
    /// placeholder cells and carries the image source sub-rectangle for that run.
    ///
    /// Returns empty unless the active screen holds a virtual placement, so the
    /// common no-image path pays only one `any` scan. Each run is matched to a
    /// virtual placement by image id (and placement id when the placeholder
    /// encodes one); the virtual placement supplies the image's cell grid
    /// (`cols`×`rows`), any crop, and the z-index, from which the run's source
    /// rectangle is carved.
    pub fn kitty_placeholder_placements(&self) -> Vec<VisibleKittyPlacement> {
        let screen = self.active();
        if !screen.kitty_placements.iter().any(|p| p.is_virtual) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for y in 0..screen.rows {
            let Some(row) = screen.visible_row(y) else {
                continue;
            };
            for run in scan_row(&row.cells) {
                let Some(vp) = screen.kitty_placements.iter().find(|p| {
                    p.is_virtual
                        && p.image_id == run.image_id
                        && (run.placement_id == 0 || p.placement_id == run.placement_id)
                }) else {
                    continue;
                };
                let Some(img) = self.kitty_images.get(run.image_id) else {
                    continue;
                };
                // The virtual placement spreads its (optionally cropped) image
                // across a `cols`×`rows` cell grid; this run covers image row
                // `virt_row`, columns `[virt_col_start, +cols)` of that grid.
                let base = vp.src.unwrap_or([0, 0, img.width, img.height]);
                let cell_w = f64::from(base[2]) / f64::from(vp.cols.max(1));
                let cell_h = f64::from(base[3]) / f64::from(vp.rows.max(1));
                let sx = f64::from(base[0]) + f64::from(run.virt_col_start) * cell_w;
                let sy = f64::from(base[1]) + f64::from(run.virt_row) * cell_h;
                let sw = f64::from(run.cols) * cell_w;
                let src = [
                    sx.round() as u32,
                    sy.round() as u32,
                    (sw.round() as u32).max(1),
                    (cell_h.round() as u32).max(1),
                ];
                out.push(VisibleKittyPlacement {
                    image_id: run.image_id,
                    placement_id: run.placement_id,
                    grid_x: i32::from(run.screen_x),
                    grid_y: i32::from(y),
                    cell_x_off: 0,
                    cell_y_off: 0,
                    cols: run.cols,
                    rows: 1,
                    src: Some(src),
                    z: vp.z,
                });
            }
        }
        out
    }

    /// A stored image by id (for the renderer to upload/sample).
    pub fn kitty_image(&self, id: u32) -> Option<&KittyImage> {
        self.kitty_images.get(id)
    }
}

/// Whether an `a=d` specifier is the uppercase form that also frees image data.
fn kitty_delete_frees(spec: KittyDelete) -> bool {
    match spec {
        KittyDelete::All { free }
        | KittyDelete::ById { free }
        | KittyDelete::ByNumber { free }
        | KittyDelete::AtCursor { free }
        | KittyDelete::AnimationFrames { free }
        | KittyDelete::AtCell { free }
        | KittyDelete::AtCellZ { free }
        | KittyDelete::ByIdRange { free }
        | KittyDelete::ByColumn { free }
        | KittyDelete::ByRow { free }
        | KittyDelete::ByZ { free } => free,
    }
}
