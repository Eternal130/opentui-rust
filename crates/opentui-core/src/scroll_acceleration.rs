//! Scroll acceleration — port of `packages/core/src/lib/scroll-acceleration.ts`
//! (upstream lines cited per item).
//!
//! `ScrollAcceleration` (scroll-acceleration.ts:1-4): a tick/reset curve
//! object. `LinearScrollAccel` (6-12) is the identity. `MacOSScrollAccel`
//! (34-98) keeps a short moving window of inter-tick intervals and maps the
//! average to a multiplier through an exponential curve.

/// `ScrollAcceleration` — scroll-acceleration.ts:1-4.
pub trait ScrollAcceleration: Send {
    /// Returns the scroll multiplier for this tick. `now` is a monotonic
    /// millisecond timestamp (upstream defaults to `Date.now()`).
    fn tick(&mut self, now: Option<f64>) -> f64;
    fn reset(&mut self);
}

/// `LinearScrollAccel` — scroll-acceleration.ts:6-12: always 1.
#[derive(Debug, Clone, Default)]
pub struct LinearScrollAccel;

impl ScrollAcceleration for LinearScrollAccel {
    fn tick(&mut self, _now: Option<f64>) -> f64 {
        1.0
    }
    fn reset(&mut self) {}
}

/// `MacOSScrollAccel` — scroll-acceleration.ts:34-98 (exact).
///
/// Measures the time between consecutive scroll events and keeps a short
/// moving window (`historySize = 3`) of the latest intervals. The average
/// interval determines the multiplier: quick bursts accelerate, slow
/// gestures stay precise. Ticks closer than `minTickInterval` (6ms — some
/// terminals like Ghostty split one wheel tick into 2+ events ~4ms apart)
/// are part of the same logical tick and return the base multiplier without
/// touching the window.
#[derive(Debug, Clone)]
pub struct MacOSScrollAccel {
    /// `opts.A` (default 0.8) — curve amplitude.
    pub a: f64,
    /// `opts.tau` (default 3) — curve time constant.
    pub tau: f64,
    /// `opts.maxMultiplier` (default 6).
    pub max_multiplier: f64,
    /// scroll-acceleration.ts:35
    last_tick_time: f64,
    /// scroll-acceleration.ts:36
    velocity_history: Vec<f64>,
}

/// scroll-acceleration.ts:37 — `historySize = 3`
const HISTORY_SIZE: usize = 3;
/// scroll-acceleration.ts:38 — `streakTimeout = 150`
const STREAK_TIMEOUT: f64 = 150.0;
/// scroll-acceleration.ts:42 — `minTickInterval = 6` (Ghostty splits one
/// wheel tick into 2+ events ~4ms apart)
const MIN_TICK_INTERVAL: f64 = 6.0;

impl Default for MacOSScrollAccel {
    fn default() -> Self {
        MacOSScrollAccel {
            a: 0.8,
            tau: 3.0,
            max_multiplier: 6.0,
            last_tick_time: 0.0,
            velocity_history: Vec::new(),
        }
    }
}

impl MacOSScrollAccel {
    #[must_use]
    pub fn new(a: f64, tau: f64, max_multiplier: f64) -> Self {
        MacOSScrollAccel {
            a,
            tau,
            max_multiplier,
            ..Default::default()
        }
    }
}

impl ScrollAcceleration for MacOSScrollAccel {
    /// `tick` — scroll-acceleration.ts:52-92 (exact math).
    fn tick(&mut self, now: Option<f64>) -> f64 {
        let now = now.unwrap_or_else(now_ms);
        let dt = if self.last_tick_time != 0.0 {
            now - self.last_tick_time
        } else {
            f64::INFINITY // scroll-acceleration.ts:57 — `lastTickTime ? … : Infinity`
        };

        // Reset streak if too much time has passed or first tick (60-64)
        if dt == f64::INFINITY || dt > STREAK_TIMEOUT {
            self.last_tick_time = now;
            self.velocity_history.clear();
            return 1.0;
        }

        // Ignore ticks closer than minTickInterval (67-69): they're part of
        // the same logical tick
        if dt < MIN_TICK_INTERVAL {
            return 1.0;
        }

        self.last_tick_time = now;

        // 73-76: push + trim to historySize
        self.velocity_history.push(dt);
        if self.velocity_history.len() > HISTORY_SIZE {
            self.velocity_history.remove(0);
        }

        // 79: average interval (lower = faster scrolling)
        let avg_interval = self.velocity_history.iter().sum::<f64>()
            / f64::from(self.velocity_history.len() as u32);

        // 83-84: interval → velocity, normalized to a 100ms reference
        let reference_interval = 100.0;
        let velocity = reference_interval / avg_interval;

        // 88-89: exponential curve — `1 + A * (exp(x) - 1)`
        let x = velocity / self.tau;
        let multiplier = 1.0 + self.a * (x.exp() - 1.0);

        // 91
        multiplier.min(self.max_multiplier)
    }

    /// `reset` — scroll-acceleration.ts:94-97.
    fn reset(&mut self) {
        self.last_tick_time = 0.0;
        self.velocity_history.clear();
    }
}

/// `Date.now()` equivalent for the standalone API (renderer callers inject
/// their own clock; scroll-acceleration.ts:52).
#[must_use]
pub fn now_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity() {
        let mut a = LinearScrollAccel;
        assert_eq!(a.tick(Some(0.0)), 1.0);
        assert_eq!(a.tick(Some(1000.0)), 1.0);
        a.reset();
        assert_eq!(a.tick(None), 1.0);
    }

    #[test]
    fn macos_first_tick_returns_one() {
        // scroll-acceleration.ts:57,60-64 — dt Infinity → reset + 1
        let mut a = MacOSScrollAccel::default();
        assert_eq!(a.tick(Some(1000.0)), 1.0);
    }

    #[test]
    fn macos_submin_interval_ignored() {
        // 42: dt < 6ms is the same logical tick (Ghostty split ticks)
        let mut a = MacOSScrollAccel::default();
        a.tick(Some(1000.0));
        assert_eq!(a.tick(Some(1003.0)), 1.0); // dt=3 < 6 → 1
                                               // the ignored tick must not enter the history: next real tick dt
                                               // measures from 1000 (capped at maxMultiplier 6)
        assert_eq!(
            a.tick(Some(1010.0)),
            (1.0 + 0.8 * ((100.0f64 / 10.0 / 3.0).exp() - 1.0)).min(6.0)
        );
    }

    #[test]
    fn macos_streak_timeout_resets() {
        let mut a = MacOSScrollAccel::default();
        a.tick(Some(1000.0));
        a.tick(Some(1010.0));
        // dt = 200 > 150 streakTimeout → window cleared, base 1
        assert_eq!(a.tick(Some(1210.0)), 1.0);
    }

    #[test]
    fn macos_window_is_three_entries() {
        // 37: historySize 3 — a burst of fast ticks then one slow tick:
        // average of the last three intervals drives the multiplier
        let mut a = MacOSScrollAccel::default();
        a.tick(Some(1000.0)); // seed lastTickTime (first tick)
        a.tick(Some(1010.0)); // dt 10
        a.tick(Some(1020.0)); // dt 10
        a.tick(Some(1030.0)); // dt 10 (window full)
        let avg = (10.0 + 10.0 + 10.0) / 3.0;
        let expected = 1.0 + 0.8 * ((100.0f64 / avg / 3.0).exp() - 1.0);
        assert_eq!(a.tick(Some(1040.0)), expected.min(6.0)); // window (10,10,10)
                                                             // a slow tick enters the window: avg (10+10+100)/3
        a.tick(Some(1140.0)); // dt 100 → window (10,10,100)
        let avg2 = (10.0 + 10.0 + 100.0) / 3.0;
        let expected2 = 1.0 + 0.8 * ((100.0f64 / avg2 / 3.0).exp() - 1.0);
        assert_eq!(a.tick(Some(1150.0)), expected2.min(6.0));
    }

    #[test]
    fn macos_caps_at_max_multiplier() {
        let mut a = MacOSScrollAccel::new(0.8, 0.01, 2.0);
        a.tick(Some(1000.0));
        let m = a.tick(Some(1007.0)); // dt 7 ≥ 6: velocity 100/7 ~ 14.3, x ~ 1428 → huge
        assert_eq!(m, 2.0);
    }

    #[test]
    fn macos_reset_clears_streak() {
        let mut a = MacOSScrollAccel::default();
        a.tick(Some(1000.0));
        a.tick(Some(1010.0));
        a.reset();
        // after reset the next tick is a "first tick" again
        assert_eq!(a.tick(Some(1015.0)), 1.0);
    }
}
