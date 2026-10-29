use crate::kitty::{KittyError, KittyImage, TransmitStep};
use crate::kitty_placeholder::scan_row;
use crate::screen::{KittyParent, KittyPlacement, VisibleKittyPlacement};
use crate::sixel;
use noa_vt::{KittyAction, KittyDelete, KittyGraphicsCommand, SixelGraphicsCommand};
use std::collections::HashMap;

use super::Terminal;

struct ImagePlace {
    placement_id: u32,
    cell_x_off: u16,
    cell_y_off: u16,
    src: Option<[u32; 4]>,
    cols: u16,
    rows: u16,
    req_cols: u32,
    req_rows: u32,
    z: i32,
    cursor_no_move: bool,
    virtual_placement: bool,
    parent: Option<KittyParent>,
}

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
        let starts_retransmit = !self.kitty_images.transfer_in_progress()
            && matches!(
                cmd.action,
                KittyAction::Transmit | KittyAction::TransmitAndDisplay
            )
            && cmd.image_id != 0
            && self.kitty_images.contains(cmd.image_id);
        if starts_retransmit {
            // Kitty spec / Ghostty 1.4.0: re-transmitting an id deletes the
            // image and all its placements as soon as the transfer begins, so
            // an interrupted or failed one leaves nothing stale on screen.
            self.drop_kitty_placements_of(&[cmd.image_id]);
            self.kitty_images.remove(cmd.image_id);
        }
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
                self.enforce_kitty_quota();
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

    /// Apply an `a=a` animation-control command, replying on completion.
    pub(super) fn kitty_animate(&mut self, cmd: &KittyGraphicsCommand) {
        let result = self.kitty_images.animate(cmd);
        self.kitty_reply(
            cmd.image_id,
            cmd.image_number,
            cmd.image_id,
            cmd.placement_id,
            cmd.quiet,
            result,
        );
    }

    /// Apply an `a=c` frame-compose command, replying on completion.
    pub(super) fn kitty_compose(&mut self, cmd: &KittyGraphicsCommand) {
        let result = self.kitty_images.compose(cmd);
        self.kitty_reply(
            cmd.image_id,
            cmd.image_number,
            cmd.image_id,
            cmd.placement_id,
            cmd.quiet,
            result,
        );
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
    /// resolving the effective cell span and moving the cursor unless `C=1`
    /// or the placement is relative (`P=`).
    fn kitty_place(
        &mut self,
        ctrl: &KittyGraphicsCommand,
        image_id: u32,
    ) -> Result<(), KittyError> {
        let (img_w, img_h) = match self.kitty_images.get(image_id) {
            Some(img) => (img.width, img.height),
            None => return Err(KittyError::NoEnt),
        };
        let parent = (ctrl.parent_image_id != 0).then_some(KittyParent {
            image_id: ctrl.parent_image_id,
            placement_id: ctrl.parent_placement_id,
            x_off: ctrl.parent_x_off,
            y_off: ctrl.parent_y_off,
        });
        if let Some(parent) = parent {
            // A virtual placement may be a parent but never relative itself.
            if ctrl.virtual_placement {
                return Err(KittyError::Invalid);
            }
            self.active()
                .check_kitty_parent((image_id, ctrl.placement_id), parent)?;
        }
        let (cell_w, cell_h) = (self.cell_width_px, self.cell_height_px);
        if cell_w == 0 || cell_h == 0 {
            // Cell metrics arrive with the first resize; without them the cell
            // span is undefined.
            return Err(KittyError::Invalid);
        }

        // Intersect the source rectangle with the image before sizing, so a
        // crop reaching past the image is not stretched (Ghostty #13890).
        let src_x = ctrl.src_x.min(img_w);
        let src_y = ctrl.src_y.min(img_h);
        let src_w = if ctrl.src_w != 0 { ctrl.src_w } else { img_w }.min(img_w - src_x);
        let src_h = if ctrl.src_h != 0 { ctrl.src_h } else { img_h }.min(img_h - src_y);
        if src_w == 0 || src_h == 0 {
            return Err(KittyError::Invalid);
        }
        // Offsets stay within the first cell (Ghostty #13891).
        let cell_x_off = ctrl.cell_x_off.min(cell_w - 1);
        let cell_y_off = ctrl.cell_y_off.min(cell_h - 1);
        let (cols, rows) = if ctrl.columns != 0 && ctrl.rows != 0 {
            (ctrl.columns, ctrl.rows)
        } else {
            // The span covers the drawn size plus the offset into the first
            // cell (Ghostty's `gridSize`).
            let [w, h] = crate::screen::kitty_image_size(
                [src_w, src_h],
                [ctrl.columns, ctrl.rows],
                [f64::from(cell_w), f64::from(cell_h)],
                [f64::from(cell_x_off), f64::from(cell_y_off)],
            );
            let span =
                |px: f64, off: u32, cell: u32| (px.round() + f64::from(off)) / f64::from(cell);
            (
                span(w, cell_x_off, cell_w).ceil() as u32,
                span(h, cell_y_off, cell_h).ceil() as u32,
            )
        };
        let cols = cols.clamp(1, u16::MAX as u32) as u16;
        let rows = rows.clamp(1, u16::MAX as u32) as u16;
        // Always explicit (not `None` = whole image) so a region scroll can
        // clip it without the image's size.
        let src = Some([src_x, src_y, src_w, src_h]);
        let (cell_x_off, cell_y_off) = (cell_x_off as u16, cell_y_off as u16);

        self.place_image(
            image_id,
            ImagePlace {
                placement_id: ctrl.placement_id,
                cell_x_off,
                cell_y_off,
                src,
                cols,
                rows,
                req_cols: ctrl.columns,
                req_rows: ctrl.rows,
                z: ctrl.z_index,
                cursor_no_move: ctrl.cursor_no_move,
                virtual_placement: ctrl.virtual_placement,
                parent,
            },
        )
    }

    fn place_image(&mut self, image_id: u32, spec: ImagePlace) -> Result<(), KittyError> {
        let cell_h_px = self.cell_height_px;
        let screen = self.active_mut();
        let anchor_abs_row =
            screen.rows_evicted() + screen.scrollback_len() + screen.cursor.y as usize;
        let anchor_col = screen.cursor.x;
        screen.insert_kitty_placement(KittyPlacement {
            image_id,
            placement_id: spec.placement_id,
            anchor_abs_row,
            anchor_col,
            cell_x_off: spec.cell_x_off,
            cell_y_off: spec.cell_y_off,
            src: spec.src,
            cols: spec.cols,
            rows: spec.rows,
            req_cols: spec.req_cols,
            req_rows: spec.req_rows,
            cell_h_px,
            z: spec.z,
            is_virtual: spec.virtual_placement,
            parent: spec.parent,
        });

        // Kitty spec: the cursor never moves after a relative placement,
        // whatever `C=` says.
        if !spec.cursor_no_move && !spec.virtual_placement && spec.parent.is_none() {
            // Move to the image's last row, one column past its right edge.
            for _ in 1..spec.rows {
                screen.index();
            }
            let max_x = screen.cols.saturating_sub(1);
            screen.cursor.x = (anchor_col as usize + spec.cols as usize).min(max_x as usize) as u16;
            screen.cursor.pending_wrap = false;
        }
        Ok(())
    }

    /// Rasterize and display a SIXEL image at the current cursor using the
    /// existing image store / placement / render path.
    pub(super) fn sixel_graphics(&mut self, cmd: SixelGraphicsCommand) {
        let (cell_w, cell_h) = (self.cell_width_px, self.cell_height_px);
        if cell_w == 0 || cell_h == 0 {
            return;
        }

        let bg = self
            .colors
            .default_bg()
            .unwrap_or_else(|| self.colors.base_default_bg());
        let Ok(raster) = sixel::rasterize(&cmd, bg) else {
            return;
        };
        let cols = raster.width.div_ceil(cell_w).clamp(1, u16::MAX as u32) as u16;
        let rows = raster.height.div_ceil(cell_h).clamp(1, u16::MAX as u32) as u16;

        let Ok(image_id) = self
            .kitty_images
            .insert_rgba(raster.width, raster.height, raster.rgba)
        else {
            return;
        };

        if self
            .place_image(
                image_id,
                ImagePlace {
                    placement_id: 0,
                    cell_x_off: 0,
                    cell_y_off: 0,
                    src: Some([0, 0, raster.width, raster.height]),
                    cols,
                    rows,
                    // Drawn at its own pixel size, like a Kitty image without `c=`/`r=`.
                    req_cols: 0,
                    req_rows: 0,
                    z: 0,
                    cursor_no_move: false,
                    virtual_placement: false,
                    parent: None,
                },
            )
            .is_ok()
        {
            self.enforce_kitty_quota();
        }
    }

    /// Delete placements (and, for uppercase specifiers, image data) per `a=d`.
    pub(super) fn kitty_delete(&mut self, cmd: &KittyGraphicsCommand) {
        let Some(spec) = cmd.delete else {
            return;
        };
        if let KittyDelete::AnimationFrames { .. } = spec {
            // `a=d,d=f` drops an image's animation frames (keeping the root),
            // targeting the image by `i=` then `I=`.
            if cmd.image_id != 0 {
                self.kitty_images.delete_frames(cmd.image_id);
            } else if cmd.image_number != 0 {
                for id in self.kitty_images.ids_with_number(cmd.image_number) {
                    self.kitty_images.delete_frames(id);
                }
            }
            return;
        }
        let free = kitty_delete_frees(spec);
        let number_id = match spec {
            KittyDelete::ByNumber { .. } => self
                .kitty_images
                .get_by_number(cmd.image_number)
                .map(|image| image.id),
            _ => None,
        };
        let live_top = {
            let s = self.active();
            (s.rows_evicted() + s.scrollback_len()) as i64
        };
        let (cursor_abs, cursor_col) = {
            let s = self.active();
            (live_top + i64::from(s.cursor.y), i64::from(s.cursor.x))
        };
        // Cell coords in `a=d` are 1-based grid columns/rows; convert to
        // session-absolute for intersection tests.
        let target_col = i64::from(cmd.src_x.saturating_sub(1));
        let target_abs = live_top + i64::from(cmd.src_y.saturating_sub(1));
        let origins = self.kitty_placement_origins();
        let origin = |p: &KittyPlacement| origins.get(&p.key()).copied();
        let covers = |p: &KittyPlacement, row: i64, col: i64| {
            origin(p).is_some_and(|(top, left)| {
                (top..top + i64::from(p.rows)).contains(&row)
                    && (left..left + i64::from(p.cols)).contains(&col)
            })
        };

        let rows = i64::from(self.active().rows);
        let removed = self.active_mut().delete_kitty_placements(|p| match spec {
            // Only placements on the active screen: scrollback keeps its own.
            KittyDelete::All { .. } => {
                !p.is_virtual
                    && origin(p).is_some_and(|(top, _)| {
                        top < live_top + rows && top + i64::from(p.rows) > live_top
                    })
            }
            KittyDelete::ById { .. } => {
                p.image_id == cmd.image_id
                    && (cmd.placement_id == 0 || p.placement_id == cmd.placement_id)
            }
            KittyDelete::ByNumber { .. } => {
                number_id == Some(p.image_id)
                    && (cmd.placement_id == 0 || p.placement_id == cmd.placement_id)
            }
            KittyDelete::AtCursor { .. } => covers(p, cursor_abs, cursor_col),
            KittyDelete::AtCell { .. } => covers(p, target_abs, target_col),
            KittyDelete::AtCellZ { .. } => covers(p, target_abs, target_col) && p.z == cmd.z_index,
            KittyDelete::ByIdRange { .. } => p.image_id >= cmd.src_x && p.image_id <= cmd.src_y,
            KittyDelete::ByColumn { .. } => origin(p)
                .is_some_and(|(_, left)| (left..left + i64::from(p.cols)).contains(&target_col)),
            KittyDelete::ByRow { .. } => origin(p)
                .is_some_and(|(top, _)| (top..top + i64::from(p.rows)).contains(&target_abs)),
            KittyDelete::ByZ { .. } => p.z == cmd.z_index,
            KittyDelete::AnimationFrames { .. } => false,
        });

        if !free {
            return;
        }
        // The images whose placements were just removed — including relative
        // placements deleted along with their parent — *and* the images the
        // command named directly: an image that was transmitted but never
        // placed has no placement to remove, yet `d=I`/`d=N`/`d=R` must still
        // free its data. A named placement (`p=`) that matched nothing leaves
        // its image alone.
        let mut candidates = removed;
        match spec {
            KittyDelete::ById { .. } if cmd.image_id != 0 && cmd.placement_id == 0 => {
                candidates.push(cmd.image_id);
            }
            KittyDelete::ByNumber { .. } if cmd.placement_id == 0 => candidates.extend(number_id),
            KittyDelete::ByIdRange { .. } => candidates.extend(
                self.kitty_images
                    .ids()
                    .filter(|id| (cmd.src_x..=cmd.src_y).contains(id)),
            ),
            _ => {}
        }
        for id in candidates {
            if !self.image_referenced(id) {
                self.kitty_images.remove(id);
            }
        }
    }

    /// Session-absolute top-left `(row, col)` of each placement on the active
    /// screen as cell-based deletes see it: a relative placement where it is
    /// drawn (unclamped, so a negative offset can't pull it onto cells it
    /// doesn't cover), anything else at its anchor. A relative placement rooted
    /// at a virtual placement with no placeholder cells in the viewport is not
    /// drawn and has no entry.
    fn kitty_placement_origins(&self) -> HashMap<(u32, u32), (i64, i64)> {
        let screen = self.active();
        let virtual_origins = if screen.kitty_placements.iter().any(|p| p.is_virtual)
            && screen.kitty_placements.iter().any(|p| p.parent.is_some())
        {
            screen.visible_virtual_placement_origins()
        } else {
            HashMap::new()
        };
        screen
            .kitty_placements
            .iter()
            .filter_map(|p| {
                let origin = if p.parent.is_some() {
                    screen.relative_placement_origin(p, &virtual_origins)?
                } else {
                    (p.anchor_abs_row as i64, i64::from(p.anchor_col))
                };
                Some((p.key(), origin))
            })
            .collect()
    }

    /// Free every image no placement on either screen references.
    pub(super) fn reclaim_unreferenced_images(&mut self) {
        let referenced = self.referenced_image_ids();
        let unreferenced: Vec<u32> = self
            .kitty_images
            .ids()
            .filter(|id| !referenced.contains(id))
            .collect();
        for id in unreferenced {
            self.kitty_images.remove(id);
        }
        // Outside `Terminal::kitty_graphics`, which resyncs after each command.
        self.kitty_images.sync_animation_flag();
    }

    /// Whether any placement on either screen still references image `id`.
    fn image_referenced(&self, id: u32) -> bool {
        self.primary
            .kitty_placements
            .iter()
            .chain(self.alt.iter().flat_map(|s| s.kitty_placements.iter()))
            .any(|p| p.image_id == id)
    }

    /// Evict images over the byte budget, with their placements on both
    /// screens.
    fn enforce_kitty_quota(&mut self) {
        let evicted = self
            .kitty_images
            .enforce_quota(&self.referenced_image_ids());
        self.drop_kitty_placements_of(&evicted);
    }

    /// Delete every placement of the images `ids` on both screens.
    pub(super) fn drop_kitty_placements_of(&mut self, ids: &[u32]) {
        if ids.is_empty() {
            return;
        }
        self.primary
            .delete_kitty_placements(|p| ids.contains(&p.image_id));
        if let Some(alt) = &mut self.alt {
            alt.delete_kitty_placements(|p| ids.contains(&p.image_id));
        }
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
    ///
    /// Also returns the visible relative placements whose chain is rooted at a
    /// virtual placement: that root sits at the minimum x and y of its
    /// placeholder cells in the viewport — as in Ghostty, which positions it
    /// from the rows it renders, a root with none there draws no children.
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
                let Some(vp) = screen.virtual_placement_for(run.image_id, run.placement_id) else {
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
                    // A placeholder tile fills its cells.
                    req_cols: u32::from(run.cols),
                    req_rows: 1,
                    src: Some(src),
                    z: vp.z,
                });
            }
        }
        let rooted: Vec<&KittyPlacement> = screen
            .kitty_placements
            .iter()
            .filter(|p| {
                p.parent.is_some()
                    && screen
                        .kitty_relative_origin(p)
                        .is_some_and(|(root, _, _)| root.is_virtual)
            })
            .collect();
        if rooted.is_empty() {
            return out;
        }
        let origins = screen.visible_virtual_placement_origins();
        let view_top = (screen.rows_evicted() + screen.visible_row_base()) as i64;
        for p in rooted {
            if let Some((row, col)) = screen.relative_placement_origin(p, &origins) {
                out.extend(p.project(col, row - view_top, screen.rows));
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
