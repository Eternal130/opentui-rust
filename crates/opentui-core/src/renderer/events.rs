//! `CliRenderEvent` family, theme-wait types, hit-grid plumbing and
//! the mouse/selection/focus/key-feed `CliRenderer` methods (moved
//! verbatim from the former single-file `renderer.rs`).
use super::*;

/// `CliRenderEvents` (renderer.ts:750-767) — the renderer's own event
/// stream (the slice the input lane emits).
///
/// GAP REGISTER (parity vs the 16-value upstream enum, renderer.ts:750-767):
/// still unported are `RENDER_ERROR`, `HANDLER_ERROR`, `SELECTION`,
/// `DEBUG_OVERLAY_TOGGLE`, `DESTROY`, `MEMORY_SNAPSHOT` —
/// deferred with their upstream features. `THEME_MODE`/`PALETTE` have
/// A4 variants; the `PALETTE` emitter (TerminalPaletteDetector) rides
/// the palette consumer lane. `EXTERNAL_OUTPUT` landed with the
/// split-footer scrollback runtime base (D1b-core).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliRenderEvent {
    /// `FRAME` — `{ frameId }` (renderer.ts:4761-4765).
    Frame {
        frame_id: u64,
    },
    /// `FOCUSED_RENDERABLE` — (current, previous) by renderable id
    /// (renderer.ts:1423).
    FocusedRenderable {
        current: Option<String>,
        previous: Option<String>,
    },
    /// `FOCUSED_EDITOR` — (current, previous) by editor id
    /// (renderer.ts:1419-1421); editors are the EditBuffer family
    /// (Input counts).
    FocusedEditor {
        current: Option<String>,
        previous: Option<String>,
    },
    /// `FOCUS` / `BLUR` — terminal focus sequences `[I` / `[O`
    /// (renderer.ts:3446-3471).
    TerminalFocus,
    TerminalBlur,
    /// `RESIZE` — `(this.width, this.height)` after `processResize`
    /// (renderer.ts:3198; enum value renderer.ts:751).
    Resize {
        width: u32,
        height: u32,
    },
    /// `CAPABILITIES` — the `TerminalCapabilities` snapshot after a
    /// capability response was processed (renderer.ts:3415; enum
    /// value renderer.ts:761).
    Capabilities(TerminalCapabilities),
    /// `THEME_MODE` — the light/dark mode after a theme-color
    /// reduction changed it (upstream payload is the bare mode
    /// string: `this.emit(CliRenderEvents.THEME_MODE,
    /// result.changedMode)`, renderer.ts:1214; enum value
    /// renderer.ts:760).
    ThemeMode {
        mode: ThemeMode,
    },
    /// `PALETTE` — the detected `TerminalColors` (renderer.ts:5187;
    /// enum value renderer.ts:761). Variant only for enum parity: the
    /// TerminalPaletteDetector family stays deferred, so nothing emits
    /// it yet (see `TerminalPalette` docs).
    Palette(TerminalPalette),
    /// `EXTERNAL_OUTPUT` — a scrollback commit was enqueued (the
    /// `enqueueSplitCommit` emit, renderer.ts:2608-2613; enum value
    /// renderer.ts:755). The payload is the commit itself
    /// (`CliRendererExternalOutputEvent`, renderer.ts:252-257) —
    /// emitted only when at least one listener is registered, exactly
    /// like upstream's `listenerCount > 0` gate (renderer.ts:2610).
    ExternalOutput(crate::scrollback::CliRendererExternalOutputEvent),
}

/// The `waitForThemeMode` result (renderer-theme-mode.ts:62-83):
/// resolved now, or a registered waiter pending its mode/timeout/
/// destroy resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeModeWait {
    /// Resolved at registration (mode known, renderer destroyed, or
    /// timeout 0 — renderer-theme-mode.ts:62-66). `None` = "not
    /// known" (upstream's `null`).
    Resolved(Option<ThemeMode>),
    /// Registered — poll with [`CliRenderer::poll_theme_mode_wait`].
    Pending { id: u64 },
}

/// A theme wait's poll outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeWaitPoll {
    /// `waiter.resolve(mode)` fired (renderer-theme-mode.ts:76-83,
    /// 89-100, 223-235).
    Resolved(Option<ThemeMode>),
    /// Still waiting.
    Pending,
}

/// One hit-grid rect, in paint order (later rects win).
#[derive(Debug, Clone)]
pub(crate) struct HitRect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    id: String,
}

/// A per-renderable mouse handler (the `onMouseDown`/`processMouseEvent`
/// side of the tree — Renderable.ts mouse event plumbing).
pub type MouseEventHandler = Box<dyn FnMut(&mut MouseEvent) + Send>;

pub(crate) type RendererEventListener = Box<dyn FnMut(&CliRenderEvent) + Send>;

/// The renderer-owned terminal session handle (`terminal-backend`
/// feature) — the object-safe slice of
/// [`crate::terminal::Terminal`] the lifecycle methods need. The
/// renderer "holds the Phase A Terminal session" through this trait so
/// mock ttys and PTY slaves substitute exactly like the pump tests.

impl CliRenderer {
    /// `notifySelectablesOfSelectionChange` (renderer.ts:5054-5077) —
    /// deliver the global selection (or its absence) to the anchor
    /// renderable: convert to local coordinates
    /// (`convertGlobalToLocalSelection`, lib/selection.ts:167-184) and
    /// run `onSelectionChanged` (EditBufferRenderable.ts:519-580).
    pub(crate) fn deliver_selection(&mut self) {
        let sel = self.selection.clone();
        let Some(sel) = sel else {
            return;
        };
        let anchor_id = sel.anchor_renderable.clone();
        let is_start = sel.is_start;
        let is_dragging = sel.is_dragging;
        // absolute anchor = anchor renderable's current position + the
        // stored relative offset (SelectionAnchor.x, lib/selection.ts:18-24)
        let Some(node) = self.find_node(&self.root.inner, &anchor_id) else {
            return;
        };
        let (nx, ny) = (node.x as i32, node.y as i32);
        let anchor_abs = (nx + sel.anchor_relative.0, ny + sel.anchor_relative.1);
        let local = Some((
            sel.anchor_relative.0,
            sel.anchor_relative.1,
            sel.focus.0 - nx,
            sel.focus.1 - ny,
            sel.behavior,
        ));
        let _ = anchor_abs;
        self.with_textarea_mut(&anchor_id, move |state, _pos, _selection| {
            state.on_selection_changed(local, is_start, is_dragging);
            true
        });
    }

    /// `clearSelection` (renderer.ts:4943-4946): null-notify the touched
    /// renderable, then drop the state.
    pub fn clear_selection(&mut self) {
        if let Some(sel) = self.selection.take() {
            let anchor_id = sel.anchor_renderable.clone();
            self.with_textarea_mut(&anchor_id, |state, _pos, _selection| {
                state.on_selection_changed(None, false, false);
                true
            });
        }
    }

    /// `hasSelection` (renderer.ts:4935).
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    pub(crate) fn find_node<'a>(&self, root: &'a Renderable, id: &str) -> Option<&'a Renderable> {
        if root.id.0 == id {
            return Some(root);
        }
        root.children.iter().find_map(|c| self.find_node(c, id))
    }

    pub(crate) fn find_node_mut<'a>(root: &'a mut Renderable, id: &str) -> Option<&'a mut Renderable> {
        if root.id.0 == id {
            return Some(root);
        }
        root.children
            .iter_mut()
            .find_map(|c| Self::find_node_mut(c, id))
    }

    // -----------------------------------------------------------------
    // Hit grid + mouse dispatch — renderer.ts:1448-1453, 3602-3817
    // -----------------------------------------------------------------

    /// `addToHitGrid` walk: every VISIBLE renderable adds its rect in
    /// paint order (Renderable.ts:1481-1493). The captured renderable
    /// is skipped (renderer.ts:1450-1452).
    pub(crate) fn collect_hit_rects(node: &Renderable, captured: Option<&str>, grid: &mut Vec<HitRect>) {
        if !node.visible {
            return;
        }
        if captured != Some(node.id.0.as_str()) {
            grid.push(HitRect {
                x: node.screen_x,
                y: node.screen_y,
                w: node.w.max(0.0) as u32,
                h: node.h.max(0.0) as u32,
                id: node.id.0.clone(),
            });
        }
        for child in &node.children {
            Self::collect_hit_rects(child, captured, grid);
        }
    }

    /// `hitTest` (renderer.ts:3869-3871): the top-most rect containing
    /// the point (later paint order wins).
    #[must_use]
    pub fn hit_test(&self, x: i32, y: i32) -> Option<String> {
        self.hit_grid
            .iter()
            .rev()
            .find(|r| x >= r.x && x < r.x + r.w as i32 && y >= r.y && y < r.y + r.h as i32)
            .map(|r| r.id.clone())
    }

    /// `onMouseDown` and friends — register a handler for a
    /// renderable id (`Renderable.processMouseEvent` +
    /// `renderable.on("mouse-down")`).
    pub fn on_mouse<F>(&mut self, id: &str, handler: F)
    where
        F: FnMut(&mut MouseEvent) + Send + 'static,
    {
        self.mouse_handlers.insert(id.to_owned(), Box::new(handler));
    }

    /// `sendMouseEvent` (renderer.ts:3623-3630): deliver to the
    /// target's handler.
    fn send_mouse_event(&mut self, target: &str, event: &mut MouseEvent) {
        // The embedded terminal's `setupMouse` wrappers
        // (EmbeddedTerminal.ts:313-335): forward before user callbacks;
        // encoded output (or a viewport scroll) prevents default.
        if self
            .find_node(&self.root.inner, target)
            .is_some_and(|n| matches!(n.content, Content::EmbeddedTerminal(_)))
        {
            self.embedded_terminal_forward_mouse(target, event);
        }
        if let Some(handler) = self.mouse_handlers.get_mut(target) {
            handler(event);
        }
    }

    /// `forwardMouse` (EmbeddedTerminal.ts:337-361) — left-press focuses,
    /// encode + send; an unencoded wheel event becomes a ±3 scroll.
    fn embedded_terminal_forward_mouse(&mut self, target: &str, event: &mut MouseEvent) {
        use crate::parse_mouse::MouseEventType as T;
        let is_left_down = event.event_type == T::Down && event.button == 0;
        let (screen_x, screen_y, consumed) = {
            let Some(node) = Self::find_node_mut(&mut self.root.inner, target) else {
                return;
            };
            let (screen_x, screen_y) = (node.screen_x, node.screen_y);
            let Content::EmbeddedTerminal(state) = &mut node.content else {
                return;
            };
            let forward = crate::renderables::embedded_terminal::mouse_event_for_terminal(
                event, screen_x, screen_y,
            );
            let (consumed, _scroll) = state.forward_mouse(&forward);
            (screen_x, screen_y, consumed)
        };
        if is_left_down {
            // `this.focus()` (:339) — defers the renderer-level focus to
            // the dispatch tail (the port's node borrow scopes first)
            self.pending_focus = Some(target.to_owned());
            let _ = (screen_x, screen_y);
        }
        if consumed {
            event.prevent_default();
            event.stop_propagation();
        }
    }

    /// `dispatchMouseEvent` (renderer.ts:3602-3621): deliver, then the
    /// autoFocus walk — the nearest focusable ancestor of the target
    /// receives focus on an un-prevented left down.
    fn dispatch_mouse_event(&mut self, target: &str, attributes: &RawMouseEvent) {
        let source = self.captured.clone();
        let mut event = MouseEvent::new(Some(target), attributes, source.as_deref(), false);
        self.send_mouse_event(target, &mut event);

        // the embedded terminal's inline `focus()` (forwardMouse,
        // EmbeddedTerminal.ts:339) — flushed before the auto-focus walk
        if let Some(id) = self.pending_focus.take() {
            self.focus_renderable(&id);
        }

        if self.config.auto_focus
            && event.event_type == crate::parse_mouse::MouseEventType::Down
            && event.button == MouseButton::Left as i32
            && !event.default_prevented()
        {
            // walk target → parents for the first focusable
            // (renderer.ts:3609-3618)
            if let Some(path) = self.ancestor_path(target) {
                for id in path {
                    if let Some(node) = self.find_node(&self.root.inner, &id) {
                        if node.focusable {
                            self.focus_renderable(&id);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// The target id followed by its ancestors (target first, root last).
    fn ancestor_path(&self, target: &str) -> Option<Vec<String>> {
        fn walk(node: &Renderable, target: &str, path: &mut Vec<String>) -> bool {
            if node.id.0 == target {
                return true;
            }
            for child in &node.children {
                if walk(child, target, path) {
                    path.insert(0, node.id.0.clone());
                    return true;
                }
            }
            false
        }
        let mut path = vec![target.to_owned()];
        if walk(&self.root.inner, target, &mut path) {
            Some(path)
        } else {
            None
        }
    }

    /// `processSingleMouseEvent` (renderer.ts:3632-3817) — the
    /// selection/console/split-footer branches are the textarea lane;
    /// the core routing (scroll fallback, over/out, drag capture,
    /// drop) is ported order-exact.
    pub fn process_mouse_event(&mut self, event: RawMouseEvent) -> bool {
        // pointer tracking (renderer.ts:3640-3643)
        self.latest_pointer = (event.x, event.y);
        self.has_pointer = true;

        if event.event_type == crate::parse_mouse::MouseEventType::Scroll {
            // scroll → hit target, falling back to the focused
            // renderable (renderer.ts:3659-3674)
            let maybe = self.hit_test(event.x, event.y);
            let fallback = self
                .focused
                .clone()
                .filter(|id| self.find_node(&self.root.inner, id).is_some());
            let scroll_target = maybe.or(fallback);
            if let Some(target) = scroll_target {
                let mut evt = MouseEvent::new(Some(&target), &event, None, false);
                self.send_mouse_event(&target, &mut evt);
                if let Some(id) = self.pending_focus.take() {
                    self.focus_renderable(&id);
                }
            }
            return true;
        }

        let maybe_id = self.hit_test(event.x, event.y);
        let same_element = maybe_id == self.last_over;
        // hit tracking updates even when nothing is hit (None == None)
        self.last_over = maybe_id.clone();
        let maybe_id = maybe_id.filter(|id| self.find_node(&self.root.inner, id).is_some());

        // over/out on target change for move/drag (renderer.ts:3742-3763)
        if !same_element
            && (event.event_type == crate::parse_mouse::MouseEventType::Drag
                || event.event_type == crate::parse_mouse::MouseEventType::Move)
        {
            let prev = self.last_over_hover.take();
            if let Some(prev_id) = prev {
                if Some(&prev_id) != self.captured.as_ref() {
                    let mut out = MouseEvent::new(Some(&prev_id), &event, None, false)
                        .with_type(crate::parse_mouse::MouseEventType::Out);
                    self.send_mouse_event(&prev_id, &mut out);
                }
            }
            if let Some(id) = &maybe_id {
                let mut over = MouseEvent::new(Some(id), &event, self.captured.as_deref(), false)
                    .with_type(crate::parse_mouse::MouseEventType::Over);
                self.send_mouse_event(id, &mut over);
            }
            self.last_over_hover = maybe_id.clone();
        }

        // captured renderable routing (renderer.ts:3765-3792)
        if let Some(captured) = self.captured.clone() {
            if event.event_type != crate::parse_mouse::MouseEventType::Up {
                let mut evt = MouseEvent::new(Some(&captured), &event, None, false);
                self.send_mouse_event(&captured, &mut evt);
                return true;
            }
            // drag-end + up + drop (renderer.ts:3771-3792)
            let mut drag_end = MouseEvent::new(Some(&captured), &event, None, false)
                .with_type(crate::parse_mouse::MouseEventType::DragEnd);
            self.send_mouse_event(&captured, &mut drag_end);
            let mut up = MouseEvent::new(Some(&captured), &event, None, false);
            self.send_mouse_event(&captured, &mut up);
            if let Some(id) = &maybe_id {
                let mut drop = MouseEvent::new(Some(id), &event, Some(&captured), false)
                    .with_type(crate::parse_mouse::MouseEventType::Drop);
                self.send_mouse_event(id, &mut drop);
            }
            self.last_over = Some(captured.clone());
            self.last_over_hover = Some(captured);
            self.captured = None;
            return true;
        }

        if let Some(id) = maybe_id {
            // drag with the left button captures the target
            // (renderer.ts:3795-3800)
            if event.event_type == crate::parse_mouse::MouseEventType::Drag
                && event.button == MouseButton::Left as i32
            {
                self.captured = Some(id.clone());
            }
            self.dispatch_mouse_event(&id, &event);
            // the renderer's selection block (renderer.ts:3680-3710):
            // left-down over a selectable embedded terminal anchors a
            // cell selection; drags update it (upstream
            // startSelection/updateSelection)
            if event.event_type == crate::parse_mouse::MouseEventType::Down
                && event.button == MouseButton::Left as i32
            {
                self.start_mouse_selection(&id, event.x, event.y);
            } else if matches!(
                event.event_type,
                crate::parse_mouse::MouseEventType::Drag | crate::parse_mouse::MouseEventType::Up
            ) {
                self.update_mouse_selection(event.x, event.y);
            }
        }
        true
    }

    /// `startSelection` (renderer.ts + EmbeddedTerminal.ts:176-181) —
    /// `shouldStartSelection` gates the anchor.
    fn start_mouse_selection(&mut self, id: &str, x: i32, y: i32) {
        let Some(node) = self.find_node(&self.root.inner, id) else {
            return;
        };
        let Content::EmbeddedTerminal(state) = &node.content else {
            return;
        };
        if !state.should_start_selection(
            x,
            y,
            node.screen_x,
            node.screen_y,
            node.w.max(0.0) as u32,
            node.h.max(0.0) as u32,
        ) {
            return;
        }
        self.mouse_selection = Some(MouseSelection {
            target: id.to_owned(),
            anchor: (x, y),
            focus: (x, y),
        });
    }

    /// `updateSelection` — recompute the terminal's local selection
    /// (`onSelectionChanged`, EmbeddedTerminal.ts:183-216).
    fn update_mouse_selection(&mut self, x: i32, y: i32) {
        let Some(sel) = self.mouse_selection.as_mut() else {
            return;
        };
        sel.focus = (x, y);
        let (id, anchor, focus) = (sel.target.clone(), sel.anchor, sel.focus);
        let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
            return;
        };
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return;
        };
        state.on_selection_changed(Some(
            crate::renderables::embedded_terminal::SelectionUpdate {
                behavior_cell: true,
                anchor: (anchor.0 - node.screen_x, anchor.1 - node.screen_y),
                focus: (focus.0 - node.screen_x, focus.1 - node.screen_y),
                width: node.w.max(0.0) as u32,
                height: node.h.max(0.0) as u32,
            },
        ));
    }

    // -----------------------------------------------------------------
    // Focus — renderer.ts:1407-1439
    // -----------------------------------------------------------------

    /// `focusRenderable(renderable)` (renderer.ts:1407-1424).
    pub fn focus_renderable(&mut self, id: &str) {
        if self.focused.as_deref() == Some(id) {
            return;
        }
        let previous = self.focused.clone();
        let previous_editor = self.current_focused_editor();
        self.focused = Some(id.to_owned());

        // blur the previous renderable's state (Renderable.blur,
        // Renderable.ts:445-460)
        if let Some(prev_id) = &previous {
            self.apply_focus_state(prev_id, false);
        }
        // focus the new one (Renderable.focus, Renderable.ts:399-430)
        self.apply_focus_state(id, true);

        let current_editor = self.current_focused_editor();
        if previous_editor != current_editor {
            self.emit_event(CliRenderEvent::FocusedEditor {
                current: current_editor,
                previous: previous_editor,
            });
        }
        self.emit_event(CliRenderEvent::FocusedRenderable {
            current: Some(id.to_owned()),
            previous,
        });
    }

    /// `blurRenderable(renderable)` (renderer.ts:1426-1439).
    pub fn blur_renderable(&mut self, id: &str) {
        if self.focused.as_deref() != Some(id) {
            return;
        }
        let previous_editor = self.current_focused_editor();
        self.focused = None;
        self.apply_focus_state(id, false);
        if previous_editor.is_some() {
            self.emit_event(CliRenderEvent::FocusedEditor {
                current: None,
                previous: previous_editor,
            });
        }
        self.emit_event(CliRenderEvent::FocusedRenderable {
            current: None,
            previous: Some(id.to_owned()),
        });
    }

    fn apply_focus_state(&mut self, id: &str, focused: bool) {
        let mut park_cursor = false;
        if let Some(node) = Self::find_node_mut(&mut self.root.inner, id) {
            node.focused = focused;
            match &mut node.content {
                Content::Input(state) => {
                    if focused {
                        state.focus();
                    } else {
                        state.blur();
                    }
                }
                Content::Textarea(state) => {
                    // `Textarea.focus`/`blur` (Textarea.ts:310-320) — the
                    // effective-colors flip
                    if focused {
                        state.focus();
                    } else {
                        state.blur();
                    }
                }
                // `EmbeddedTerminal.focus`/`blur` (EmbeddedTerminal.ts:227-253):
                // the mode-1004 focus report; blur parks the terminal
                // cursor at (0,0) hidden (:251)
                Content::EmbeddedTerminal(state) => {
                    if focused {
                        state.focus();
                    } else {
                        state.blur();
                        park_cursor = true;
                    }
                }
                _ => {}
            }
        }
        if park_cursor {
            self.cursor_state.set_position(0, 0, false);
        }
    }

    /// `currentFocusedRenderable` (renderer.ts:1381-1382).
    #[must_use]
    pub fn current_focused_renderable(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Observe an input renderable's value (`input.value`).
    #[must_use]
    pub fn input_value(&self, id: &str) -> Option<String> {
        let node = self.find_node(&self.root.inner, id)?;
        match &node.content {
            Content::Input(state) => Some(state.value().to_owned()),
            _ => None,
        }
    }

    /// Run an edit action on an input renderable — the host-side
    /// application of keymap edit commands (`withFocusedEditor`,
    /// edit-buffer-bindings.ts:433-440).
    pub fn run_input_action(&mut self, id: &str, action: crate::input::InputAction) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        match &mut node.content {
            Content::Input(state) => state.run_action(action),
            _ => false,
        }
    }

    /// Toggle the textarea mapping suspension on an input — the
    /// renderer side of `registerTextareaMappingSuspension`
    /// (edit-buffer-bindings.ts:374-431): with the keymap owning the
    /// keys, the input's own binding map stands down.
    pub fn set_input_mapping_suspension(&mut self, id: &str, suspended: bool) {
        if let Some(node) = Self::find_node_mut(&mut self.root.inner, id) {
            if let Content::Input(state) = &mut node.content {
                state.suspend_mappings = suspended;
            }
        }
    }

    /// `currentFocusedEditor` (renderer.ts:1384-1388) — the focused
    /// renderable when it is an EditBuffer family member (Input or
    /// Textarea; `isEditBufferRenderable`, EditBufferRenderable.ts:51).
    #[must_use]
    pub fn current_focused_editor(&self) -> Option<String> {
        let id = self.focused.as_ref()?;
        let node = self.find_node(&self.root.inner, id)?;
        match node.content {
            // `isEditBufferRenderable` (EditBufferRenderable.ts:51-53):
            // both the Input and the Textarea are EditBuffer family
            Content::Input(_) | Content::Textarea(_) => Some(id.clone()),
            _ => None,
        }
    }

    /// Focus tree snapshot — the queries `@opentui/keymap`'s host
    /// needs (`getParentTarget`, `getFocusedTarget`,
    /// keymap opentui.ts:60-71): node → parent links plus the focused
    /// id. Feeds [`opentui_keymap`]'s `OpenTuiHost::add`/`set_focused`.
    #[must_use]
    pub fn focus_tree(&self) -> (HashMap<String, Option<String>>, Option<String>) {
        let mut parents = HashMap::new();
        fn walk(
            node: &Renderable,
            parent: Option<&str>,
            parents: &mut HashMap<String, Option<String>>,
        ) {
            parents.insert(node.id.0.clone(), parent.map(str::to_owned));
            for child in &node.children {
                walk(child, Some(node.id.0.as_str()), parents);
            }
        }
        walk(&self.root.inner, None, &mut parents);
        (parents, self.focused.clone())
    }

    // -----------------------------------------------------------------
    // Renderer events — CliRenderEvents (renderer.ts:750-767)
    // -----------------------------------------------------------------

    /// `renderer.on(CliRenderEvents.X, listener)`.
    pub fn on_event<F>(&mut self, listener: F)
    where
        F: FnMut(&CliRenderEvent) + Send + 'static,
    {
        self.event_listeners.push(Box::new(listener));
    }

    pub(crate) fn emit_event(&mut self, event: CliRenderEvent) {
        // THEME_MODE also feeds the renderer's theme mirror + resolves
        // the waitForThemeMode waiters (applyThemeMode,
        // renderer-theme-mode.ts:223-235; emission renderer.ts:1214).
        if let CliRenderEvent::ThemeMode { mode } = event {
            self.theme_mode = Some(mode);
            self.resolve_theme_waiters();
        }
        for listener in &mut self.event_listeners {
            listener(&event);
        }
    }

    /// The terminal-backend event seam — the pub counterpart of the
    /// private `emit_event`, for the `terminal` module's pumps. The
    /// TS facade emits the same events itself on the native
    /// callback boundary (RESIZE renderer.ts:3198, CAPABILITIES
    /// renderer.ts:3415); the Rust backend owns the boundary, so it
    /// needs the same pipe. Zero behavior change for every other
    /// caller.
    pub fn emit_terminal_event(&mut self, event: CliRenderEvent) {
        self.emit_event(event);
    }

    // -----------------------------------------------------------------
    // Terminal cursor — setCursorPosition (renderer.ts:4149-4155)
    // -----------------------------------------------------------------

    /// The Input-family cursor, folded into the one
    /// [`CursorOutputState`] the frame emits (the textarea cursors
    /// arrive via `prepare_paint` earlier in `render`; one coherent
    /// renderCursor pipeline — EditBufferRenderable.ts:1010-1022).
    pub(crate) fn update_terminal_cursor(&mut self) {
        // only the focused editor shows the cursor; +1 for 1-based
        // terminal coordinates (line 1017-1018)
        if let Some(id) = self.focused.clone() {
            if let Some(node) = self.find_node(&self.root.inner, &id) {
                if let Content::Input(state) = &node.content {
                    let x = node.screen_x + state.visual_cursor_col() as i32;
                    let y = node.screen_y;
                    self.cursor_state.set_position(
                        (x + 1).max(1) as u32,
                        (y + 1).max(1) as u32,
                        true,
                    );
                }
            }
        }
    }

    /// The terminal cursor position (1-based, upstream coordinates)
    /// and visibility — `renderCursor` + `setCursorPosition`. Sourced
    /// from the same [`CursorOutputState`] the frame emission writes
    /// (the oracle greps the emitted CUP + show sequence).
    #[must_use]
    pub fn terminal_cursor(&self) -> (u32, u32, bool) {
        if self.cursor_state.visible {
            (self.cursor_state.x, self.cursor_state.y, true)
        } else {
            (0, 0, false)
        }
    }

    // -----------------------------------------------------------------
    // Input feed — the StdinParser slice (stdin-parser.ts) +
    // handleStdinEvent (renderer.ts:3498-3545)
    // -----------------------------------------------------------------

    /// Feed raw stdin bytes. The events lane's parser handles the unit
    /// classes the input family produces: mouse sequences (SGR + X10,
    /// runs included), bracketed paste with partial-chunk reassembly,
    /// terminal focus sequences, and complete key units. Incomplete
    /// units (a lone ESC awaiting timeout disambiguation, split UTF-8
    /// codepoints, lone high bytes) stay pending across feeds —
    /// matching the oracle's immediate-event behavior. The full
    /// protocol-context state machine (CPR/DA/pixel-resolution
    /// deferral) is the tier-2 `StdinParser` follow-up port.
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            // Preserve the existing empty-chunk → empty-keypress
            // behavior (stdin-parser.ts:735-740)
            let parsed = parse_keypress(
                "",
                ParseKeypressOptions {
                    use_kitty_keyboard: self.use_kitty_keyboard(),
                },
            );
            if let Some(key) = parsed {
                self.dispatch_key(key);
            }
            return;
        }
        // Active paste: everything flows through the collector
        // (stdin-parser.ts:744-747)
        if let Some(mut collector) = self.paste_collector.take() {
            match collector.consume(bytes) {
                Some((paste, remainder)) => {
                    self.dispatch_paste(paste);
                    if !remainder.is_empty() {
                        self.feed_bytes(&remainder);
                    }
                }
                None => {
                    self.paste_collector = Some(collector);
                }
            }
            return;
        }
        self.feed_pending.extend_from_slice(bytes);
        self.drain_feed_units();
    }

    /// `renderer.useKittyKeyboard` (renderer.ts:1975-1983): the parser
    /// flag — true when the protocol is configured at all.
    #[must_use]
    pub fn use_kitty_keyboard(&self) -> bool {
        self.config.kitty_keyboard.is_some()
    }

    /// The configured kitty flags (`buildKittyKeyboardFlags`,
    /// renderer.ts:1131-1133).
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> u32 {
        build_kitty_keyboard_flags(self.config.kitty_keyboard.as_ref())
    }

    /// Process complete units off the pending buffer (the
    /// `scanPending` loop's unit classes, stdin-parser.ts:905+).
    fn drain_feed_units(&mut self) {
        loop {
            // Paste start at a unit boundary (stdin-parser.ts:753-756)
            if self
                .feed_pending
                .starts_with(crate::paste::BRACKETED_PASTE_START)
            {
                let start_len = crate::paste::BRACKETED_PASTE_START.len();
                self.feed_pending.drain(..start_len);
                self.paste_collector = Some(BracketedPasteCollector::new());
                let rest = std::mem::take(&mut self.feed_pending);
                if !rest.is_empty() {
                    self.feed_bytes(&rest);
                }
                return;
            }

            // Mouse sequences route before keys (handleStdinEvent,
            // renderer.ts:3521-3527)
            if let Some(consumed) = self
                .mouse_parser
                .parse_mouse_sequence_len(&self.feed_pending, 0)
            {
                let events = self
                    .mouse_parser
                    .parse_all_mouse_events(&self.feed_pending[..consumed]);
                if !events.is_empty() {
                    if self.config.use_mouse {
                        for event in events {
                            self.process_mouse_event(event);
                        }
                    }
                    self.feed_pending.drain(..consumed);
                    continue;
                }
            }

            let buf = self.feed_pending.clone();
            if buf.is_empty() {
                return;
            }

            // Terminal focus sequences (renderer.ts:3446-3471)
            if buf == b"\x1b[I" || buf == b"\x1b[O" {
                let focus = buf == b"\x1b[I";
                self.emit_event(if focus {
                    CliRenderEvent::TerminalFocus
                } else {
                    CliRenderEvent::TerminalBlur
                });
                self.feed_pending.drain(..buf.len());
                continue;
            }

            let Some(unit_len) = complete_unit_len(&buf) else {
                return; // incomplete unit stays pending
            };
            if unit_len == 0 {
                return;
            }
            let unit: Vec<u8> = self.feed_pending.drain(..unit_len).collect();
            // Valid UTF-8 decodes as such (the parser's KEY_DECODER,
            // stdin-parser.ts:548-550); non-UTF-8 bytes take the
            // latin1 path (decodeLatin1, stdin-parser.ts:544-546)
            let raw = match String::from_utf8(unit.clone()) {
                Ok(text) => text,
                Err(_) => unit
                    .iter()
                    .map(|b| char::from_u32(u32::from(*b)).unwrap_or('\u{fffd}'))
                    .collect(),
            };
            let parsed = parse_keypress(
                &raw,
                ParseKeypressOptions {
                    use_kitty_keyboard: self.use_kitty_keyboard(),
                },
            );
            if let Some(key) = parsed {
                self.dispatch_key(key);
            }
            // response/unknown sequences produce no key event (the
            // sequence handler chain is a follow-up port)
        }
    }

    /// `handleStdinEvent` key branch (renderer.ts:3514-3520): global
    /// key listeners first (KeyHandler), then the focused
    /// renderable's `handleKeyPress` (the internal-handler
    /// registration of Renderable.focus, Renderable.ts:406-427).
    fn dispatch_key(&mut self, key: crate::parse_keypress::ParsedKey) {
        // exitOnCtrlC (renderer.ts:1252-1257): Ctrl+C destroys the
        // renderer. Upstream defers through `process.nextTick` and the
        // key still propagates to the other listeners; the sync port
        // destroys immediately and likewise continues dispatch —
        // bounded divergence (no microtask queue in the sync port).
        if self.config.exit_on_ctrl_c {
            let event_like =
                crate::keybinding::KeyBindingLookup {
                    binding: crate::keybinding::KeyBindingLike::from(&key),
                    base_code: None,
                };
            let ctrl_c = crate::keybinding::KeyBindingLike::with_mods("c", true, false, false, false);
            if crate::keybinding::matches_key_binding(&event_like, &ctrl_c) {
                self.destroy();
            }
        }
        let mut event = KeyEvent::new(key);
        // globals (KeyHandler global listeners)
        self.key_input.process_parsed_key(event.key.clone());
        // the focused renderable only when the globals did not
        // prevent or stop the event (KeyHandler.ts:179-181)
        if event.default_prevented() || event.propagation_stopped() {
            return;
        }
        self.deliver_key_to_focused(&mut event);
    }

    fn deliver_key_to_focused(&mut self, event: &mut KeyEvent) {
        let Some(id) = self.focused.clone() else {
            return;
        };
        let is_textarea = self
            .find_node(&self.root.inner, &id)
            .is_some_and(|n| matches!(n.content, Content::Textarea(_)));
        if is_textarea {
            // the focused textarea's handleKeyPress (Renderable.focus
            // internal-handler registration, Renderable.ts:406-427)
            self.textarea_key_press(&id, event);
            return;
        }
        // the focused embedded terminal's handleKeyPress
        // (EmbeddedTerminal.ts:255-259 + the keyrelease internal
        // registration, :227-240 — release events ride the same path)
        {
            let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
                return;
            };
            if let Content::EmbeddedTerminal(state) = &mut node.content {
                state.handle_key_press(event);
                return;
            }
        }
        let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
            return;
        };
        if let Content::Input(state) = &mut node.content {
            state.handle_key(event);
        }
    }

    /// The paste branch (renderer.ts:3528-3530).
    fn dispatch_paste(&mut self, bytes: Vec<u8>) {
        // global paste listeners
        self.key_input.process_paste(bytes.clone(), None);
        // focused renderable
        let Some(id) = self.focused.clone() else {
            return;
        };
        let is_textarea = self
            .find_node(&self.root.inner, &id)
            .is_some_and(|n| matches!(n.content, Content::Textarea(_)));
        if is_textarea {
            // `handlePaste` (Textarea.ts:259-261)
            self.with_textarea_mut(&id, |state, _pos, _selection| {
                state.handle_paste(&bytes);
                true
            });
            return;
        }
        let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
            return;
        };
        if let Content::EmbeddedTerminal(state) = &mut node.content {
            // `handlePaste` (EmbeddedTerminal.ts:261-263)
            let event = PasteEvent::new(bytes, None);
            state.handle_paste(&event);
            return;
        }
        if let Content::Input(state) = &mut node.content {
            let mut event = PasteEvent::new(bytes, None);
            state.handle_paste(&mut event);
        }
    }
}
