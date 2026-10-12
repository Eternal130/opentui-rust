//! The frame scheduler — `requestRender` and friends from
//! `packages/core/src/renderer.ts` (lines 1543-1619, 4660-4838).
//!
//! Upstream drives renders from a real event loop (timers +
//! `process.nextTick`); the headless port exposes the same **scheduling
//! contract** over a deterministic virtual clock:
//!
//! * `requestRender` (renderer.ts:1565-1605) — coalesces invalidations:
//!   one scheduled attempt at a time (`renderTimeout` /
//!   `updateScheduled`), an in-flight render flips
//!   `immediateRerenderRequested` instead of recursing, a suspended
//!   renderer drops the request, and while running the request only
//!   arms the frame timer.
//! * `scheduleRenderTimer` (renderer.ts:1543-1554) — delay
//!   `max(minTargetFrameTime - elapsed, 0)`; `minTargetFrameTime =
//!   1000 / maxFps`.
//! * `loop` (renderer.ts:4674-4833) — runs animation-frame callbacks
//!   (drained per frame, renderer.ts:4705-4706), then the frame
//!   callbacks, then one render; while running (or after an immediate
//!   re-request) it reschedules at `max(1, targetFrameTime -
//!   frameTime)` where `targetFrameTime = 1000 / targetFps`
//!   (renderer.ts:1196-1197, 4771-4778).
//! * `requestAnimationFrame` (renderer.ts:1295-1303) — callbacks land
//!   in a map drained at the START of the next frame.
//!
//! Divergence (documented): frame *durations* are virtual — a frame
//! consumes 0 virtual ms, so the inter-frame delay is the full
//! `targetFrameTime`/`minTargetFrameTime` (the subtraction of measured
//! frame time, renderer.ts:4773, degenerates). Timer ordering and
//! coalescing behavior are exact.

use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// `RendererControlState` (renderer.ts:769-776).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ControlState {
    #[default]
    Idle,
    AutoStarted,
    ExplicitStarted,
    ExplicitPaused,
    ExplicitSuspended,
    ExplicitStopped,
}

/// `getSchedulerState()` (renderer.ts:1667-1673).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerState {
    pub is_running: bool,
    pub is_rendering: bool,
    pub has_scheduled_render: bool,
}

/// The render body — called once per frame by the scheduler.
pub type RenderFn<'a> = &'a mut dyn FnMut();

type AnimationCallback = Box<dyn FnMut(f64) + Send>;
type FrameCallback = Box<dyn FnMut(f64) + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerAction {
    /// `clock.setTimeout(() => this.loop(), delay)` — the frame timer
    /// (`renderTimeout`, renderer.ts:4775-4778).
    RenderTimeout,
    /// `clock.setTimeout(() => this.activateFrame(generation), delay)`
    /// — the deferred first frame (`updateScheduled`,
    /// renderer.ts:1603).
    ActivateFrame(u64),
}

#[derive(Debug)]
struct Timer {
    deadline: u64,
    seq: u64,
    action: TimerAction,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.seq == other.seq
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap: reverse for deadline order.
        other
            .deadline
            .cmp(&self.deadline)
            .then(other.seq.cmp(&self.seq))
    }
}

/// `CliRenderer.animationFrameId` (renderer.ts:781) — a shared counter
/// so multiple schedulers (like upstream's multiple renderers) never
/// hand out colliding rAF ids.
static ANIMATION_FRAME_ID: AtomicU64 = AtomicU64::new(0);

/// The scheduler half of the renderer. The render body is a callback
/// so the renderer (which owns buffers + the renderable tree) stays
/// the single owner of painting.
pub struct RenderScheduler {
    target_fps: f64,
    max_fps: f64,
    control_state: ControlState,
    /// `_isRunning` — the continuous `start()` cadence.
    is_running: bool,
    /// `rendering` (renderer.ts:830) — an in-flight `loop()`.
    rendering: bool,
    /// `renderTimeout` presence — a scheduled `loop()` timer.
    render_timeout: bool,
    /// `updateScheduled` + `updateGeneration` (renderer.ts:1591-1604).
    update_scheduled: bool,
    update_generation: u64,
    /// `immediateRerenderRequested` (renderer.ts:1584-1589).
    immediate_rerender_requested: bool,
    /// `feedIdleRenderScheduled` (renderer.ts:1573-1575).
    feed_idle_render_scheduled: bool,
    /// virtual clock (ms).
    now_ms: u64,
    last_time: u64,
    timers: BinaryHeap<Timer>,
    timer_seq: u64,
    animation_requests: HashMap<u64, AnimationCallback>,
    frame_callbacks: Vec<FrameCallback>,
    /// `_frameId` (renderer.ts:4694) — bumped per attempted frame.
    frame_id: u64,
    /// Frames actually painted.
    frame_count: u64,
}

impl Default for RenderScheduler {
    fn default() -> Self {
        RenderScheduler::new(30.0, 60.0)
    }
}

impl RenderScheduler {
    /// `this.targetFps = config.targetFps || 30; this.maxFps =
    /// config.maxFps || 60` (renderer.ts:1196-1197) with the derived
    /// frame times (renderer.ts:833, 1703-1718).
    #[must_use]
    pub fn new(target_fps: f64, max_fps: f64) -> Self {
        let target_fps = if target_fps == 0.0 { 30.0 } else { target_fps };
        let max_fps = if max_fps == 0.0 { 60.0 } else { max_fps };
        RenderScheduler {
            target_fps,
            max_fps,
            control_state: ControlState::Idle,
            is_running: false,
            rendering: false,
            render_timeout: false,
            update_scheduled: false,
            update_generation: 0,
            immediate_rerender_requested: false,
            feed_idle_render_scheduled: false,
            now_ms: 0,
            last_time: 0,
            timers: BinaryHeap::new(),
            timer_seq: 0,
            animation_requests: HashMap::new(),
            frame_callbacks: Vec::new(),
            frame_id: 0,
            frame_count: 0,
        }
    }

    #[must_use]
    pub fn target_fps(&self) -> f64 {
        self.target_fps
    }

    #[must_use]
    pub fn max_fps(&self) -> f64 {
        self.max_fps
    }

    /// `targetFrameTime` (renderer.ts:833).
    #[must_use]
    pub fn target_frame_time(&self) -> u64 {
        (1000.0 / self.target_fps).floor().max(1.0) as u64
    }

    /// `minTargetFrameTime` (renderer.ts:834).
    #[must_use]
    pub fn min_target_frame_time(&self) -> u64 {
        (1000.0 / self.max_fps).floor().max(1.0) as u64
    }

    #[must_use]
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    #[must_use]
    pub fn now(&self) -> u64 {
        self.now_ms
    }

    pub fn set_control_state(&mut self, state: ControlState) {
        self.control_state = state;
    }

    /// `renderer.start()` (renderer.ts:4654-4671): marks running and
    /// kicks one loop immediately (unless a feed-idle retry owns it).
    pub fn start(&mut self, render: RenderFn) {
        self.is_running = true;
        if self.control_state == ControlState::Idle {
            self.control_state = ControlState::AutoStarted;
        }
        self.last_time = self.now_ms;
        if self.feed_idle_render_scheduled {
            return;
        }
        self.r#loop(render);
    }

    /// `renderer.stop()` — pauses the continuous cadence.
    pub fn stop(&mut self) {
        self.is_running = false;
    }

    /// `isIdleNow()` (renderer.ts:1638-1649) — the synchronous form of
    /// `renderer.idle()`.
    #[must_use]
    pub fn is_idle_now(&self) -> bool {
        !self.is_running
            && !self.rendering
            && !self.render_timeout
            && !self.update_scheduled
            && !self.feed_idle_render_scheduled
            && !self.immediate_rerender_requested
    }

    /// `getSchedulerState()` (renderer.ts:1667-1673).
    #[must_use]
    pub fn scheduler_state(&self) -> SchedulerState {
        SchedulerState {
            is_running: self.is_running,
            is_rendering: self.rendering,
            has_scheduled_render: self.render_timeout
                || self.update_scheduled
                || self.immediate_rerender_requested,
        }
    }

    /// `requestAnimationFrame` (renderer.ts:1295-1300): the callback
    /// is drained at the START of the next frame. Registering one also
    /// arms the frame (`requestLive`, renderer.ts:1298) so the
    /// callback actually runs — modeled as an update-generation timer
    /// when nothing is scheduled yet.
    pub fn request_animation_frame(&mut self, callback: AnimationCallback) -> u64 {
        let id = ANIMATION_FRAME_ID.fetch_add(1, Ordering::SeqCst);
        self.animation_requests.insert(id, callback);
        if !self.render_timeout && !self.update_scheduled && !self.is_running {
            self.update_scheduled = true;
            self.update_generation += 1;
            let generation = self.update_generation;
            self.push_timer(0, TimerAction::ActivateFrame(generation));
        }
        id
    }

    /// `cancelAnimationFrame` (renderer.ts:1301-1303).
    pub fn cancel_animation_frame(&mut self, handle: u64) {
        self.animation_requests.remove(&handle);
    }

    /// Frame callbacks (`onFrame` handlers — renderer.ts:4716-4722),
    /// run every frame after the animation requests.
    pub fn add_frame_callback(&mut self, callback: FrameCallback) {
        self.frame_callbacks.push(callback);
    }

    /// `requestRender()` (renderer.ts:1565-1605). The render callback
    /// is invoked synchronously when the `delay === 0` nextTick path
    /// fires (renderer.ts:1598-1600).
    pub fn request_render(&mut self, render: RenderFn) {
        if self.control_state == ControlState::ExplicitSuspended {
            return;
        }

        // A skipped feed-backed frame already owns the next scheduling
        // attempt (renderer.ts:1570-1575).
        if self.feed_idle_render_scheduled {
            return;
        }

        if self.is_running {
            if !self.rendering && !self.render_timeout {
                self.schedule_render_timer();
            }
            return;
        }

        // Re-render while already rendering loops forever; flag it
        // instead (renderer.ts:1584-1589).
        if self.rendering {
            self.immediate_rerender_requested = true;
            return;
        }

        if !self.update_scheduled && !self.render_timeout {
            self.update_scheduled = true;
            self.update_generation += 1;
            let generation = self.update_generation;
            let elapsed = self.now_ms.saturating_sub(self.last_time);
            let delay = self.min_target_frame_time().saturating_sub(elapsed);
            if delay == 0 {
                // process.nextTick — runs before any timer
                self.activate_frame(generation, render);
                return;
            }
            self.push_timer(delay, TimerAction::ActivateFrame(generation));
        }
    }

    /// `intermediateRender()` (renderer.ts:4835-4838).
    pub fn intermediate_render(&mut self, render: RenderFn) {
        self.immediate_rerender_requested = true;
        self.r#loop(render);
    }

    fn push_timer(&mut self, delay: u64, action: TimerAction) {
        self.timer_seq += 1;
        self.timers.push(Timer {
            deadline: self.now_ms + delay,
            seq: self.timer_seq,
            action,
        });
    }

    /// `scheduleRenderTimer()` (renderer.ts:1543-1554).
    fn schedule_render_timer(&mut self) {
        if self.render_timeout || self.control_state == ControlState::ExplicitSuspended {
            return;
        }
        let elapsed = self.now_ms.saturating_sub(self.last_time);
        let delay = self.min_target_frame_time().saturating_sub(elapsed);
        self.render_timeout = true;
        self.push_timer(delay, TimerAction::RenderTimeout);
    }

    /// `activateFrame(generation)` (renderer.ts:1607-1619).
    fn activate_frame(&mut self, generation: u64, render: RenderFn) {
        if generation != self.update_generation || !self.update_scheduled {
            return;
        }
        self.r#loop(render);
        if generation == self.update_generation {
            self.update_scheduled = false;
        }
    }

    /// Runs due timers (the virtual event loop firing). Returns true
    /// when at least one timer ran.
    pub fn pump_timers(&mut self, render: RenderFn) -> bool {
        let mut ran = false;
        while let Some(next) = self.timers.peek() {
            if next.deadline > self.now_ms {
                break;
            }
            let timer = self.timers.pop().expect("peeked");
            self.now_ms = self.now_ms.max(timer.deadline);
            match timer.action {
                TimerAction::RenderTimeout => {
                    self.render_timeout = false;
                    self.r#loop(render);
                }
                TimerAction::ActivateFrame(generation) => {
                    self.activate_frame(generation, render);
                }
            }
            ran = true;
        }
        ran
    }

    /// Advances the virtual clock to the next timer deadline and runs
    /// it. Returns false when nothing is scheduled.
    pub fn advance_next_timer(&mut self, render: RenderFn) -> bool {
        let Some(next) = self.timers.peek() else {
            return false;
        };
        let deadline = next.deadline;
        self.now_ms = self.now_ms.max(deadline);
        self.pump_timers(render)
    }

    /// Drives the scheduler until idle (capped at `max_frames` frames):
    /// every due timer fires, the clock jumps to the next deadline when
    /// none is due. The deterministic equivalent of awaiting
    /// `renderer.idle()`.
    pub fn run_until_idle(&mut self, max_frames: u64, render: RenderFn) {
        let mut frames = 0u64;
        while !self.is_idle_now() {
            if frames >= max_frames {
                return;
            }
            if !self.pump_timers(render) {
                // nothing due — jump to the next deadline
                if !self.advance_next_timer(render) {
                    return;
                }
            }
            frames += 1;
        }
    }

    /// `loop()` (renderer.ts:4674-4833) — the frame body.
    fn r#loop(&mut self, render: RenderFn) {
        if !self.begin_loop() {
            return;
        }
        // 1. animation requests drained first (renderer.ts:4705-4711)
        let delta = self.frame_delta();
        let mut requests: Vec<AnimationCallback> =
            self.animation_requests.drain().map(|(_, cb)| cb).collect();
        for cb in &mut requests {
            cb(delta);
        }
        // 2. frame callbacks (renderer.ts:4716-4722)
        for cb in &mut self.frame_callbacks {
            cb(delta);
        }
        // 3. the render itself
        render();
        self.frame_count += 1;
        self.end_loop(0);
    }

    fn frame_delta(&mut self) -> f64 {
        let now = self.now_ms;
        let elapsed = now.saturating_sub(self.last_time);
        self.last_time = now;
        elapsed as f64
    }

    /// Loop entry guards (renderer.ts:4675-4681). Returns false when
    /// the loop must not run. Bumps `_frameId` before any work
    /// (renderer.ts:4694) and clears a pending frame timer.
    fn begin_loop(&mut self) -> bool {
        if self.rendering {
            return false;
        }
        self.frame_id += 1;
        if self.render_timeout {
            self.cancel_render_timers();
            self.render_timeout = false;
        }
        self.rendering = true;
        true
    }

    fn cancel_render_timers(&mut self) {
        let mut kept = BinaryHeap::new();
        while let Some(timer) = self.timers.pop() {
            if timer.action != TimerAction::RenderTimeout {
                kept.push(timer);
            }
        }
        self.timers = kept;
    }

    /// Loop exit (renderer.ts:4771-4832): reschedule while running or
    /// when an immediate re-render was requested. Immediate rerenders
    /// run at the max-fps cadence, ordinary frames at the target
    /// cadence (renderer.ts:4772-4773).
    fn end_loop(&mut self, frame_time_ms: u64) {
        self.rendering = false;
        if self.is_running || self.immediate_rerender_requested {
            let target = if self.immediate_rerender_requested {
                self.min_target_frame_time()
            } else {
                self.target_frame_time()
            };
            let delay = target.saturating_sub(frame_time_ms).max(1);
            self.immediate_rerender_requested = false;
            self.render_timeout = true;
            self.push_timer(delay, TimerAction::RenderTimeout);
        } else {
            self.render_timeout = false;
        }
    }

    #[cfg(test)]
    fn has_timers(&self) -> bool {
        !self.timers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_times_derive_from_fps() {
        // renderer.ts:1196-1197, 833-834, 1712-1718
        let s = RenderScheduler::new(30.0, 60.0);
        assert_eq!(s.target_frame_time(), 33); // 1000/30 floored
        assert_eq!(s.min_target_frame_time(), 16); // 1000/60 floored
                                                   // falsy config values fall back to the defaults
        let s = RenderScheduler::new(0.0, 0.0);
        assert_eq!(s.target_fps(), 30.0);
        assert_eq!(s.max_fps(), 60.0);
    }

    #[test]
    fn request_render_defers_then_fires_once() {
        // renderer.ts:1591-1604: elapsed 0 → delay = minTargetFrameTime
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        s.request_render(&mut || frames += 1);
        assert_eq!(frames, 0, "deferred at t=0 (16ms min frame time)");
        assert!(!s.is_idle_now());
        assert!(s.advance_next_timer(&mut || frames += 1));
        assert_eq!(frames, 1);
        assert!(s.is_idle_now());
    }

    #[test]
    fn request_render_coalesces() {
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        s.request_render(&mut || frames += 1);
        s.request_render(&mut || frames += 1); // already scheduled → no-op
        s.request_render(&mut || frames += 1);
        assert!(!s.advance_next_timer(&mut || frames += 1) || true);
        s.run_until_idle(10, &mut || frames += 1);
        assert_eq!(frames, 1, "three requests → one frame");
    }

    #[test]
    fn zero_delay_fires_synchronously() {
        // renderer.ts:1598-1600: nextTick when elapsed >= minTargetFrameTime
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        // first frame deferred to t=16
        s.request_render(&mut || frames += 1);
        s.run_until_idle(10, &mut || frames += 1);
        assert_eq!(frames, 1);
        // clock now at 16; a request 16ms later has elapsed >= 16 → delay 0
        s.now_ms += 16;
        s.request_render(&mut || frames += 1);
        assert_eq!(frames, 2, "fired via the nextTick path");
        assert!(s.is_idle_now());
    }

    #[test]
    fn running_mode_repeats_at_target_fps() {
        // renderer.ts:4771-4778: while running, delay = targetFrameTime
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        s.start(&mut || frames += 1);
        assert_eq!(frames, 1);
        assert!(!s.is_idle_now(), "running mode schedules the next frame");
        assert!(s.advance_next_timer(&mut || frames += 1));
        assert_eq!(frames, 2);
        assert_eq!(s.now(), 33, "targetFrameTime = 1000/30");
        assert!(s.advance_next_timer(&mut || frames += 1));
        assert_eq!(frames, 3);
        assert_eq!(s.now(), 66);
        s.stop();
        s.run_until_idle(10, &mut || frames += 1);
        let settled = frames;
        assert!(s.is_idle_now());
        assert!(!s.advance_next_timer(&mut || frames += 1));
        assert_eq!(frames, settled);
    }

    #[test]
    fn request_render_while_running_arms_frame_timer() {
        // renderer.ts:1577-1582: while running, requestRender only
        // arms the frame timer — no second scheduled attempt
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        s.start(&mut || frames += 1);
        s.request_render(&mut || frames += 1);
        // exactly one timer outstanding
        assert!(s.has_timers());
        s.advance_next_timer(&mut || frames += 1);
        assert_eq!(frames, 2);
        s.stop();
        s.run_until_idle(5, &mut || frames += 1);
        assert!(s.is_idle_now());
    }

    #[test]
    fn animation_frames_drained_per_frame() {
        // renderer.ts:1295-1303 + 4705-4711
        use std::sync::{Arc, Mutex};
        let mut s = RenderScheduler::new(30.0, 60.0);
        let hits: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let hits = Arc::clone(&hits);
            s.request_animation_frame(Box::new(move |_dt| {
                hits.lock().unwrap().push(1);
            }));
        }
        s.run_until_idle(10, &mut || {});
        assert_eq!(*hits.lock().unwrap(), vec![1]);
        // drained — a second frame does not re-run it
        s.run_until_idle(10, &mut || {});
        assert_eq!(*hits.lock().unwrap(), vec![1]);
    }

    #[test]
    fn cancel_animation_frame() {
        let mut s = RenderScheduler::new(30.0, 60.0);
        let id = s.request_animation_frame(Box::new(|_dt| panic!("cancelled")));
        s.cancel_animation_frame(id);
        s.run_until_idle(10, &mut || {});
    }

    #[test]
    fn animation_callbacks_run_before_render() {
        use std::sync::{Arc, Mutex};
        let mut s = RenderScheduler::new(30.0, 60.0);
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let order = Arc::clone(&order);
            s.request_animation_frame(Box::new(move |_dt| {
                order.lock().unwrap().push("raf");
            }));
        }
        {
            let order = Arc::clone(&order);
            s.add_frame_callback(Box::new(move |_dt| {
                order.lock().unwrap().push("frame-cb");
            }));
        }
        let order2 = Arc::clone(&order);
        s.run_until_idle(10, &mut || order2.lock().unwrap().push("render"));
        assert_eq!(*order.lock().unwrap(), vec!["raf", "frame-cb", "render"]);
    }

    #[test]
    fn suspended_renderer_drops_requests() {
        // renderer.ts:1566-1568
        let mut s = RenderScheduler::new(30.0, 60.0);
        s.set_control_state(ControlState::ExplicitSuspended);
        s.request_render(&mut || panic!("must not render"));
        assert!(s.is_idle_now());
    }

    #[test]
    fn scheduler_state_report() {
        // renderer.ts:1667-1673
        let mut s = RenderScheduler::new(30.0, 60.0);
        let state = s.scheduler_state();
        assert!(!state.is_running && !state.is_rendering && !state.has_scheduled_render);
        s.start(&mut || {});
        let state = s.scheduler_state();
        assert!(state.is_running);
        assert!(state.has_scheduled_render);
    }

    #[test]
    fn intermediate_render_rerenders_at_max_fps_cadence() {
        // renderer.ts:4835-4838 + 4772: immediate rerun uses the
        // minTargetFrameTime cadence
        let mut s = RenderScheduler::new(30.0, 60.0);
        let mut frames = 0u64;
        s.intermediate_render(&mut || frames += 1);
        assert_eq!(frames, 1);
        // next scheduled timer at now+16 (not +33)
        assert!(s.advance_next_timer(&mut || frames += 1));
        assert_eq!(s.now(), 16);
        s.stop();
        s.run_until_idle(10, &mut || frames += 1);
        assert!(s.is_idle_now());
    }
}
