//! Frame production family (moved verbatim from the former
//! single-file `renderer.rs`): `render`, layout pass, text measure,
//! SGR emission and the bench/snapshot accessors.
use super::*;

impl CliRenderer {
    /// `requestRender` → `loop()` tick: one deterministic frame.
    /// A destroyed renderer renders nothing (the `loop()` destroyed
    /// guard, renderer.ts:4674+).
    pub fn render(&mut self) -> u64 {
        if self.destroyed {
            return self.frame_id;
        }
        // 1. clear next with the background color
        self.next_render_buffer.clear(self.config.background_color);

        // 2. layout: root calculateLayout(width, height) —
        //    RootRenderable.render pass 1 (Renderable.ts:1798-1802, 1865-67)
        self.layout_pass();

        // pass 2: update layout + screen coords + inline size hooks
        // (effects drive re-layout frames exactly like upstream's
        // requestRender scheduling — the observed 0.5.14 settle sequence:
        // frame A lays out with both bars visible, the recalc hides the
        // horizontal bar, frame B re-lays the viewport one row taller)
        let mut effects = WalkEffects::default();
        self.root.inner.update_layout(
            &self.tree,
            (0.0, 0.0),
            (f32::INFINITY, f32::INFINITY),
            &mut effects,
        );
        self.apply_recalcs(&mut effects);
        self.apply_repairs(&mut effects);
        let mut guard = 0;
        while effects.any() && guard < 16 {
            self.layout_pass();
            effects = WalkEffects::default();
            self.root.inner.update_layout(
                &self.tree,
                (0.0, 0.0),
                (f32::INFINITY, f32::INFINITY),
                &mut effects,
            );
            if std::env::var("OT_DBG").is_ok() {
                let vp = self
                    .root
                    .inner
                    .find("scroll-box-viewport-0")
                    .map(|n| format!("({},{})", n.w, n.h))
                    .unwrap_or_default();
                eprintln!("[walk{}] vp={}", guard + 2, vp);
            }
            self.apply_recalcs(&mut effects);
            self.apply_repairs(&mut effects);
            guard += 1;
        }

        // pass 2.5: textarea prepare (renderSelf snapshot + renderCursor,
        // EditBufferRenderable.ts:991-1004) — the cursor is renderer
        // state, so it is collected before the paint walk
        let mut cursor_positions: Vec<(String, u32, u32, bool)> = Vec::new();
        let now_ms = self.performance_now();
        self.root.inner.prepare_paint(&mut cursor_positions, now_ms);

        // pass 3: paint into next buffer; the hit grid rebuilds in
        // paint order (addToHitGrid, Renderable.ts:1493)
        self.hit_grid.clear();
        let captured = self.captured.clone();
        Self::collect_hit_rects(&self.root.inner, captured.as_deref(), &mut self.hit_grid);
        self.root.inner.paint(&mut self.next_render_buffer);

        // renderCursor: each focused editor sets the terminal cursor;
        // the last one wins (upstream sets it during render order,
        // EditBufferRenderable.ts:1010-1022). A focused editor that
        // blurred/hidden set `setCursorPosition(0,0,false)` itself
        // (1030-1034); with no focused editor at all the port's only
        // cursor writers are editors (the textareas above, the input
        // in update_terminal_cursor below), so the cursor hides.
        for (id, x, y, visible) in &cursor_positions {
            if let Some(node) = self.find_node(&self.root.inner, id) {
                match &node.content {
                    Content::Textarea(state) => {
                        let (style, blinking, color) = state.cursor_style();
                        self.cursor_state.set_style(style, blinking, color);
                    }
                    // EmbeddedTerminal.ts:293-297 — bar→line,
                    // underline→underline, else block + the cursor color
                    Content::EmbeddedTerminal(state) if *visible => {
                        let cursor = state.engine.cursor();
                        let style = match cursor.style {
                            0 => crate::renderables::textarea::CursorStyle::Line,
                            2 => crate::renderables::textarea::CursorStyle::Underline,
                            _ => crate::renderables::textarea::CursorStyle::Block,
                        };
                        let (r, g, b) =
                            cursor.color.unwrap_or(crate::embedded_terminal::DEFAULT_FG);
                        self.cursor_state.set_style(
                            style,
                            cursor.blinking,
                            crate::color::Rgba::rgb(r, g, b),
                        );
                    }
                    _ => {}
                }
            }
            self.cursor_state.set_position(*x, *y, *visible);
        }
        if cursor_positions.is_empty() {
            self.cursor_state.visible = false;
        }
        // the Input family's cursor rides the same channel
        // (setCursorPosition, renderer.ts:4149-4155)
        self.update_terminal_cursor();

        // 3. SGR emission (diff current vs next) — before the swap
        let mut sgr = String::new();
        self.emit_frame_sgr(&mut sgr);
        self.sgr_history.push_str(&sgr);
        self.last_frame_sgr = sgr;

        // swap: current := next (renderer.zig syncs cell-by-cell; a frame
        // ends with current == next)
        core::mem::swap(
            &mut self.current_render_buffer,
            &mut self.next_render_buffer,
        );

        self.frame_id += 1;
        // FRAME event (renderer.ts:4761-4765)
        self.emit_event(CliRenderEvent::Frame {
            frame_id: self.frame_id,
        });
        self.frame_id
    }

    /// `applyScreenMode`'s layout band (renderer.ts:3160-3163:
    /// `this.width = nextGeometry.renderWidth; this.height =
    /// nextGeometry.renderHeight` then `this.root.resize(...)`) — the
    /// root tree lays out against the RENDER extents, which under
    /// split-footer is the footer band height
    /// (render-geometry.ts:31-39), never the full terminal. The port
    /// keeps the render extents in `config.width`/`height`
    /// (constructor renderer.ts:1075; [`Self::resize`]), so the pass
    /// reads them directly.
    fn layout_pass(&mut self) {
        let root_node = self.root.inner.node;
        let available = tp::Size {
            width: tp::AvailableSpace::Definite(self.config.width as f32),
            height: tp::AvailableSpace::Definite(self.config.height as f32),
        };
        self.tree
            .compute_layout_with_measure(root_node, available, Self::measure_text)
            .expect("taffy layout");
    }

    /// Apply size-change hook events in fire order — each is a
    /// `recalculateBarProps` run with the at-fire-time sizes
    /// (ScrollBox.ts:749-805).
    fn apply_recalcs(&mut self, effects: &mut WalkEffects) {
        let Self { root, tree, .. } = self;
        for event in std::mem::take(&mut effects.events) {
            scrollbox::apply_recalc(
                &mut root.inner,
                tree,
                &event.owner,
                event.stale_content_size,
                effects,
            );
        }
    }

    /// Mount a renderable under an arbitrary node (nested `add`).
    /// Returns false when `parent_id` is not in the tree. ScrollBox ids
    /// route into the content viewport (`ScrollBox.add`, ScrollBox.ts:514-516).
    pub fn mount_child(&mut self, parent_id: &str, child: Renderable) -> bool {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        crate::renderable::mount_under(&mut root.inner, &target, tree, child).is_none()
    }

    /// `parent.add(child, index)` for the reconciler's anchored inserts
    /// (upstream `_insertNode` → `parent.add(node, anchorIndex)`,
    /// reconciler.ts:98-111). ScrollBox ids route into the content
    /// viewport like [`CliRenderer::mount_child`]. Returns false when
    /// `parent_id` is not in the tree.
    pub fn insert_child_at(&mut self, parent_id: &str, index: usize, child: Renderable) -> bool {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        match root.inner.find_mut(&target) {
            Some(parent) => {
                parent.insert_at(tree, index, child);
                true
            }
            None => false,
        }
    }

    /// `parent.remove(child)` WITHOUT destruction — detach the subtree
    /// for a keyed MOVE (upstream `Renderable.add`'s same-parent
    /// branch unlinks first, Renderable.ts:1225-1228; the taffy subtree
    /// stays intact for re-attachment via
    /// [`CliRenderer::insert_child_at`]).
    pub fn detach_child(&mut self, parent_id: &str, child_id: &str) -> Option<Renderable> {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        let parent = root.inner.find_mut(&target)?;
        let pos = parent.children.iter().position(|c| c.id.0 == child_id)?;
        let _ = tree.remove_child_at_index(parent.node, pos);
        Some(parent.children.remove(pos))
    }

    /// `parent.remove(child)` + `destroyRecursively` (upstream
    /// `_removeNode`, reconciler.ts:113-141: detach, then destroy the
    /// detached subtree on the next tick — here synchronously, dropping
    /// the owned subtree and freeing its taffy nodes). ScrollBox ids
    /// route into the content viewport. Returns false when either id is
    /// missing.
    pub fn unmount_child(&mut self, parent_id: &str, child_id: &str) -> bool {
        let Some(node) = self.detach_child(parent_id, child_id) else {
            return false;
        };
        // destroyRecursively: free the subtree's taffy nodes (the node
        // is unlinked now, so `remove` takes the whole subtree)
        let _ = self.tree.remove(node.node);
        true
    }

    /// Live child ids under `parent_id` (ScrollBox ids route into the
    /// content viewport) — reconciler/test observability of child order.
    #[must_use]
    pub fn has_node(&self, id: &str) -> bool {
        self.root.inner.find(id).is_some()
    }

    #[must_use]
    pub fn child_ids(&self, parent_id: &str) -> Option<Vec<String>> {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        self.root
            .inner
            .find(&target)
            .map(|p| p.children.iter().map(|c| c.id.0.clone()).collect())
    }

    /// Rename a node's live id (`node.id = value`, reconciler.ts:260-263).
    /// Keeps ScrollBox content routing coherent.
    pub fn rename_node(&mut self, from: &str, to: &str) -> bool {
        if let Some(node) = self.root.inner.find_mut(from) {
            node.id.0 = to.to_owned();
        } else {
            return false;
        }
        if let Some(content) = self.content_of.get(from).cloned() {
            self.content_of.remove(from);
            self.content_of.insert(to.to_owned(), content);
        }
        true
    }

    /// Re-apply a node's style from the full layout options (upstream:
    /// each style prop setter writes the yoga node, e.g. Renderable.ts
    /// padding/flex setters; a wholesale `setStyle` is equivalent since
    /// [`crate::layout::taffy_style`] derives from the same options).
    /// Returns false when `id` is not in the tree.
    pub fn restyle_node(
        &mut self,
        id: &str,
        layout: crate::layout::LayoutOptions,
        width: Option<Dimension>,
        height: Option<Dimension>,
    ) -> bool {
        let Self { tree, root, .. } = self;
        let Some(node) = root.inner.find_mut(id) else {
            return false;
        };
        node.layout = layout;
        node.width = width.unwrap_or(Dimension::Auto);
        node.height = height.unwrap_or(Dimension::Auto);
        let style = crate::layout::taffy_style(&node.layout, node.width, node.height);
        let _ = tree.set_style(node.node, style);
        let _ = tree.mark_dirty(node.node);
        true
    }

    /// Construct a ScrollBox subtree without mounting (children added via
    /// [`CliRenderer::mount_child`] land in its content).
    #[must_use]
    pub fn make_scrollbox(
        &mut self,
        id: &str,
        opts: &mut scrollbox::ScrollBoxOptions,
    ) -> Renderable {
        let r = scrollbox::build_scrollbox(&mut self.tree, id, opts);
        let content_id = match &r.content {
            Content::ScrollBox(_, state) => state.content_id.clone(),
            _ => unreachable!("build_scrollbox yields ScrollBox root"),
        };
        self.content_of.insert(id.to_owned(), content_id);
        r
    }

    /// `scrollBox.scrollTop = value` (ScrollBox.ts:168-171) followed by a
    /// settle render — the harness's scripted-mutation step.
    pub fn set_scroll_top(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_top(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    pub fn set_scroll_left(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_left(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollBy(delta, unit)` (ScrollBox.ts:404-413).
    pub fn scroll_by(
        &mut self,
        owner: &str,
        dx: f32,
        dy: f32,
        unit: crate::renderables::scrollbar::ScrollUnit,
    ) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_by(&mut root.inner, tree, owner, dx, dy, unit, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollTo({x, y})` (ScrollBox.ts:464-473).
    pub fn scroll_to(&mut self, owner: &str, x: f32, y: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_to(&mut root.inner, tree, owner, x, y, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollChildIntoView(childId)` (ScrollBox.ts:415-462).
    pub fn scroll_child_into_view(&mut self, owner: &str, child_id: &str) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_child_into_view(&mut root.inner, tree, owner, child_id, &mut effects);
        let _ = effects;
        self.render();
    }

    /// Wheel-event seam (`onMouseEvent` scroll branch, ScrollBox.ts:542-584).
    pub fn scroll_event(&mut self, owner: &str, event: &scrollbox::ScrollEvent) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::on_scroll_event(&mut root.inner, tree, owner, event, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `renderer.resize(w, h)` (renderer.ts:4073-4076 → processResize →
    /// applyScreenMode's geometry re-run, renderer.ts:3015-3019): new
    /// buffers, root re-sized; cached node sizes PERSIST (upstream
    /// `_widthValue` keeps the last size across resizes, Renderable.ts:1096-1119)
    /// so only genuinely-resized nodes fire their size hooks.
    ///
    /// `w`/`h` are TERMINAL extents: under split-footer the render
    /// surface re-derives from
    /// [`render_geometry::calculate_render_geometry`] (renderWidth/
    /// renderHeight/ renderOffset, render-geometry.ts:31-39) exactly
    /// like `applyScreenMode`'s `this.width = nextGeometry.renderWidth`
    /// (renderer.ts:3160-3161); every other mode is the identity.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.terminal_width == width && self.terminal_height == height {
            return;
        }
        self.terminal_width = width;
        self.terminal_height = height;
        let geometry = render_geometry::calculate_render_geometry(
            self.screen_mode,
            width,
            height,
            self.footer_height,
        );
        self.render_offset = geometry.render_offset;
        let (width, height) = (geometry.render_width, geometry.render_height);
        self.config.width = width;
        self.config.height = height;
        self.next_render_buffer = OptimizedBuffer::new(width, height);
        self.current_render_buffer = OptimizedBuffer::new(width, height);
        // RootRenderable size is fixed at construction (Renderable.ts:1743+)
        let node = self.root.inner.node;
        let mut style = self.tree.style(node).expect("root style").clone();
        style.size.width = tp::Dimension::length(width as f32);
        style.size.height = tp::Dimension::length(height as f32);
        let _ = self.tree.set_style(node, style);
        let _ = self.tree.mark_dirty(node);
        self.render();
    }

    /// The taffy measure function: Text leaves measure their wrapped
    /// content; Editor leaves measure through the EditorView target
    /// (native-renderable.zig:63-88); everything else taffy defaults.
    fn measure_text(
        known_dimensions: tp::Size<Option<f32>>,
        available_space: tp::Size<tp::AvailableSpace>,
        _node_id: tp::NodeId,
        ctx: Option<&mut MeasureCtx>,
        _style: &tp::Style,
    ) -> tp::Size<f32> {
        let Some(ctx) = ctx else {
            return tp::Size::ZERO;
        };
        if ctx.kind == MeasureKind::Text {
            // perf lane: borrow the measure target — cloning the whole
            // TextDocument per taffy measure call dominated layout cost
            // on text-heavy trees (measure only reads it)
            let default_text;
            let text = match &ctx.text {
                Some(t) => t,
                None => {
                    default_text = crate::renderables::text::MeasuredText::default();
                    &default_text
                }
            };
            let avail_w = match available_space.width {
                tp::AvailableSpace::Definite(w) => w,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let (w, h) = crate::renderables::text::TextRenderableState::measure(
                text,
                known_dimensions.width,
                avail_w,
            );
            return tp::Size {
                width: w,
                height: h,
            };
        }
        if ctx.kind == MeasureKind::Editor {
            let default_editor;
            let editor = match &ctx.editor {
                Some(e) => e,
                None => {
                    default_editor = crate::renderables::textarea::MeasuredEditor::default();
                    &default_editor
                }
            };
            let avail_w = match available_space.width {
                tp::AvailableSpace::Definite(w) => w,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let avail_h = match available_space.height {
                tp::AvailableSpace::Definite(h) => h,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let (w, h) = TextareaState::measure(
                &editor.doc,
                editor.wrap_mode,
                known_dimensions.width,
                avail_w,
                avail_h,
            );
            return tp::Size {
                width: w,
                height: h,
            };
        }
        if ctx.kind == MeasureKind::Table {
            // TextTableRenderable.setupMeasureFunc (TextTable.ts:469-499)
            let default_table;
            let table = match &ctx.table {
                Some(t) => t,
                None => {
                    default_table = crate::renderables::text_table::MeasuredTable::default();
                    &default_table
                }
            };
            let avail_w = match available_space.width {
                tp::AvailableSpace::Definite(w) => Some(w),
                tp::AvailableSpace::MaxContent | tp::AvailableSpace::MinContent => None,
            };
            let (w, h) = table.core.measure(known_dimensions.width, avail_w);
            return tp::Size {
                width: w,
                height: h,
            };
        }
        if ctx.kind == MeasureKind::Gutter {
            // GutterRenderable.setupMeasureFunc (LineNumberRenderable.ts:106-125):
            // content-derived dimensions — calculateWidth × the target's
            // virtual line count — ignoring the incoming constraints.
            let empty = crate::renderables::line_number::GutterMeasure::default();
            let measure = ctx.gutter.as_ref().unwrap_or(&empty);
            let (w, h) = measure.measure();
            return tp::Size {
                width: w,
                height: h,
            };
        }
        tp::Size::ZERO
    }

    /// The SGR frame writer — `prepareRenderFrameWithWriter` simplified to
    /// the full-repaint first frame + diffed subsequent frames with
    /// row-equality fast path (renderer.zig:2650-2838).
    fn emit_frame_sgr(&mut self, out: &mut String) {
        let width = self.config.width;
        let height = self.config.height;
        // The split-footer row offset: every emitted row moves down by
        // `renderOffset` terminal rows so the footer-band tree paints
        // into the bottom live region (renderer.zig:2718:
        // `moveToOutput(writer, x + 1, y + 1 + self.renderOffset)` —
        // the native emitter holds the offset set via
        // `lib.setRenderOffset`, renderer.ts:3136-3137). Zero for every
        // non-split mode — byte-identical headless frames.
        let row_offset = self.render_offset;
        let force = self.frame_id == 0;
        let mut current_fg: Option<Rgba> = None;
        let mut current_bg: Option<Rgba> = None;
        let mut current_attrs: Option<u32> = None;
        let mut current_link_id: u32 = 0;
        let mut run_start: i64 = -1;
        let mut run_length: u32 = 0;

        for y in 0..height {
            // row-equality fast path (renderer.zig:2655-2662)
            if !force {
                let mut equal = true;
                for x in 0..width {
                    match (
                        self.current_render_buffer.get(x, y),
                        self.next_render_buffer.get(x, y),
                    ) {
                        (Some(a), Some(b)) if a == b => {}
                        _ => {
                            equal = false;
                            break;
                        }
                    }
                }
                if equal {
                    continue;
                }
            }
            for x in 0..width {
                let (Some(cur), Some(next)) = (
                    self.current_render_buffer.get(x, y),
                    self.next_render_buffer.get(x, y),
                ) else {
                    continue;
                };
                if !force && cur == next {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                        run_start = -1;
                        run_length = 0;
                    }
                    continue;
                }
                let fg_match = current_fg.is_some_and(|c| c == next.fg);
                // ----- OSC 8 hyperlink branch (A3; renderer.zig:2750-2765) -----
                // Upstream computes `linkId` under a `caps.hyperlinks`
                // gate (`hyperlinksEnabled`, renderer.zig:2751); this
                // port has no capability store on the renderer, so the
                // gate collapses to link-id presence — a link id only
                // exists after a consumer rasterized one, which it only
                // does once the probe reported `Capabilities.hyperlinks`.
                // Cells without links (every existing scenario) take
                // the `0` path and emit nothing — byte-identical.
                let link_id = crate::ansi::get_link_id(next.attributes);
                if link_id != current_link_id {
                    if current_link_id != 0 {
                        // close the previous link (renderer.zig:2754)
                        out.push_str("\x1b]8;;\x1b\\");
                    }
                    current_link_id = link_id;
                    if current_link_id != 0 {
                        // open: `id={id};{url}` (renderer.zig:2760-2762) —
                        // id→URL via the renderer-side pool (upstream:
                        // `nextRenderBuffer.link_pool`, the global LinkPool)
                        if let Some(url) = self.link_pool.get(&current_link_id) {
                            out.push_str("\x1b]8;id=");
                            out.push_str(&current_link_id.to_string());
                            out.push(';');
                            out.push_str(url);
                            out.push_str("\x1b\\");
                        } else {
                            // link not found — treat as no link
                            // (renderer.zig:2763-2764)
                            current_link_id = 0;
                        }
                    }
                }
                let bg_match = current_bg.is_some_and(|c| c == next.bg);
                let same_attrs = fg_match && bg_match && current_attrs == Some(next.attributes);
                if !same_attrs || run_start == -1 {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                    }
                    run_start = i64::from(x);
                    run_length = 0;
                    current_fg = Some(next.fg);
                    current_bg = Some(next.bg);
                    current_attrs = Some(next.attributes);
                    crate::ansi::move_to(out, x + 1, y + 1 + row_offset);
                    crate::ansi::emit_color(out, next.fg, false);
                    crate::ansi::emit_color(out, next.bg, true);
                    crate::ansi::TextAttributes::apply_attributes(out, next.attributes);
                }
                // print the cell's text
                if crate::buffer::is_continuation_char(next.char) {
                    // no bytes; cursor advances implicitly
                } else if crate::buffer::is_grapheme_char(next.char) {
                    out.push_str(
                        self.next_render_buffer
                            .grapheme_bytes(crate::buffer::grapheme_id_from_char(next.char)),
                    );
                } else if let Some(c) = char::from_u32(next.char) {
                    out.push(c);
                }
                run_length += 1;
            }
        }
        if run_length > 0 {
            out.push_str(crate::ansi::ansi_consts::RESET);
        }
        // frame-tail link close (renderer.zig:2920-2925): the open link
        // closes BEFORE the final reset; a link close alone still marks
        // the frame started upstream (renderer.zig:2921-2924).
        let mut link_tail = false;
        if current_link_id != 0 {
            out.push_str("\x1b]8;;\x1b\\");
            link_tail = true;
        }
        // the cursor emission tail (renderer.zig:2932-3016) — `frame_
        // started` mirrors whether any cell bytes were written
        let frame_started = run_length > 0 || link_tail;
        // cursor rows shift by the same renderOffset (renderer.zig:2972:
        // `const cursorY = cursorPos.y + self.renderOffset`)
        self.cursor_state.emit(out, frame_started, row_offset);
    }

    /// SGR bytes of the last rendered frame (harness observability).
    #[must_use]
    pub fn last_frame_sgr(&self) -> &str {
        &self.last_frame_sgr
    }

    // -- bench-rig hooks (perf lane) ---------------------------------------
    //
    // The perf rig (`tools/bench`) measures the SGR frame writer in
    // isolation. These expose the internal writer + its forced-full-
    // repaint path WITHOUT adding public API surface beyond the rig:
    // doc-hidden, zero behavior change for every other caller.

    /// [`Self::emit_frame_sgr`] exposed for the perf rig (doc-hidden —
    /// not part of the ported API surface).
    #[doc(hidden)]
    pub fn bench_emit_frame_sgr(&mut self, out: &mut String) {
        self.emit_frame_sgr(out);
    }

    /// Reset the frame counter so the NEXT emission takes the forced
    /// full-repaint path (`frame_id == 0`, renderer.zig first-frame).
    /// Perf-rig only (doc-hidden): a real render() after this would
    /// simply emit one full frame then resume diffing.
    #[doc(hidden)]
    pub fn bench_force_full_frame(&mut self) {
        self.frame_id = 0;
    }

    /// The FULL SGR emission across all rendered frames — what a terminal
    /// consumed (the harness's vt-replay validates this end-to-end).
    #[must_use]
    pub fn full_sgr_stream(&self) -> &str {
        &self.sgr_history
    }

    /// Alias of [`Self::full_sgr_stream`] (the textarea lane's original
    /// name; kept for the lane's callers).
    #[must_use]
    pub fn sgr_stream(&self) -> &str {
        &self.sgr_history
    }

    #[must_use]
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Text-leaf scroll setters — `scrollY`/`scrollX` on
    /// TextBufferRenderable (TextBufferRenderable.ts:176-196): clamped
    /// against the viewport, then `updateViewportOffset` (218-220).
    pub fn text_set_scroll(&mut self, id: &str, horizontal: bool, value: f32) {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return;
        };
        if let Content::Text(state) = &mut node.content {
            let v = value.max(0.0) as u32;
            if horizontal {
                state.set_scroll_x(v);
            } else {
                state.set_scroll_y(v);
            }
        }
    }

    /// Update a text leaf's measure context after content changes
    /// (`content` setter → updateTextBuffer, Text.ts:75-83).
    pub fn refresh_text_measure(&mut self, id: &str) {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return;
        };
        if let Content::Text(state) = &mut node.content {
            state.invalidate_vlines();
            let m = state.measured();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.text = Some(m);
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    /// Update a textarea leaf's measure context after content changes
    /// (`content-changed` → `yogaNode.markDirty`, EditBufferRenderable.ts:203-210).
    pub fn refresh_textarea_measure(&mut self, id: &str) {
        let Some(node) = self.find_node(&self.root.inner, id) else {
            return;
        };
        if let Content::Textarea(state) = &node.content {
            let m = state.measured();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.editor = Some(m);
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    /// The terminal cursor state (upstream `renderer.cursor` /
    /// `setCursorPosition` on the RenderContext).
    pub fn set_cursor_position(&mut self, x: u32, y: u32, visible: bool) {
        self.cursor_state.set_position(x, y, visible);
    }

    pub fn set_cursor_style(
        &mut self,
        style: crate::renderables::textarea::CursorStyle,
        blinking: bool,
        color: Rgba,
    ) {
        self.cursor_state.set_style(style, blinking, color);
    }

    pub fn cursor_position(&self) -> (u32, u32, bool) {
        (
            self.cursor_state.x,
            self.cursor_state.y,
            self.cursor_state.visible,
        )
    }

    // -- textarea actions + selection delivery ---------------------------------

    /// Execute one of the 36 edit-buffer actions on a mounted textarea
    /// (the `withFocusedEditor` host path, edit-buffer-bindings.ts:433-440).
    /// Delivers the pending global-selection update to the anchor
    /// renderable afterwards (upstream notifies inside updateSelection;
    /// deferred here — see `TextareaState::update_selection_for_movement`).
    pub fn textarea_action(&mut self, id: &str, action: &str) -> bool {
        let mut dirty = false;
        let result = self.with_textarea_mut(id, |state, pos, selection| {
            let mut ctx = crate::renderables::textarea::ActionCtx {
                selection,
                node_x: pos.0,
                node_y: pos.1,
                selection_dirty: false,
            };
            let result = state.execute_action(action, &mut ctx);
            dirty = ctx.selection_dirty;
            result
        });
        if result.is_some_and(|_| dirty) {
            self.deliver_selection();
        }
        result.unwrap_or(false)
    }

    /// `handleKeyPress` on a mounted textarea.
    pub fn textarea_key_press(&mut self, id: &str, key: &crate::events::KeyEvent) -> bool {
        let mut dirty = false;
        let result = self.with_textarea_mut(id, |state, pos, selection| {
            let mut ctx = crate::renderables::textarea::ActionCtx {
                selection,
                node_x: pos.0,
                node_y: pos.1,
                selection_dirty: false,
            };
            let result = state.handle_key_press(key, &mut ctx);
            dirty = ctx.selection_dirty;
            result
        });
        if result.is_some_and(|_| dirty) {
            self.deliver_selection();
        }
        result.unwrap_or(false)
    }

    /// Borrow a textarea's state with the selection moved out (the
    /// tree and the selection are both renderer-owned; moving the
    /// selection out keeps the borrows disjoint).
    pub(crate) fn with_textarea_mut(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut TextareaState, (i32, i32), &mut Option<RendererSelection>) -> bool,
    ) -> Option<bool> {
        let mut selection = std::mem::take(&mut self.selection);
        let Self { root, .. } = self;
        let node = find_node_mut(&mut root.inner, id)?;
        let pos = (node.x as i32, node.y as i32);
        let Content::Textarea(state) = &mut node.content else {
            return None;
        };
        let result = f(state, pos, &mut selection);
        self.selection = selection;
        Some(result)
    }

    /// Mutate a mounted textarea's state (scenario/harness surface).
    pub fn textarea_state_mut(&mut self, id: &str, f: impl FnOnce(&mut TextareaState)) -> bool {
        self.with_textarea_mut(id, |state, _pos, _selection| {
            f(state);
            true
        })
        .unwrap_or(false)
    }

    /// Read a mounted textarea's state (scenario/harness surface).
    pub fn textarea_state<R>(
        &mut self,
        id: &str,
        f: impl FnOnce(&TextareaState) -> R,
    ) -> Option<R> {
        let mut out = None;
        self.with_textarea_mut(id, |state, _pos, _selection| {
            out = Some(f(state));
            true
        });
        out
    }
}
