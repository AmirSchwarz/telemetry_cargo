//! Deterministic app-level timing. Callers provide monotonic milliseconds.
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LifecycleTotals {
    pub running_ms: u64,
    pub foreground_visible_ms: u64,
    pub focused_ms: u64,
    pub interaction_active_ms: u64,
    pub idle_ms: u64,
}

/// Multiple windows contribute a union of elapsed time, never a sum.
#[derive(Debug)]
pub struct Lifecycle {
    windows: BTreeMap<String, (bool, bool)>,
    last_tick: u64,
    last_input: Option<u64>,
    idle_threshold_ms: u64,
    suspended: bool,
    totals: LifecycleTotals,
}

impl Lifecycle {
    pub fn new(now_ms: u64, idle_threshold_ms: u64) -> Self {
        Self {
            windows: BTreeMap::new(),
            last_tick: now_ms,
            last_input: None,
            idle_threshold_ms,
            suspended: false,
            totals: LifecycleTotals::default(),
        }
    }

    pub fn advance(&mut self, now_ms: u64) {
        let now_ms = now_ms.max(self.last_tick);
        let elapsed = now_ms - self.last_tick;
        if !self.suspended {
            let visible = self.windows.values().any(|(visible, _)| *visible);
            let focused = self
                .windows
                .values()
                .any(|(visible, focused)| *visible && *focused);
            let recent = self.last_input.map_or(0, |input| {
                now_ms
                    .min(input.saturating_add(self.idle_threshold_ms))
                    .saturating_sub(self.last_tick)
                    .min(elapsed)
            });
            self.totals.running_ms = self.totals.running_ms.saturating_add(elapsed);
            if visible {
                self.totals.foreground_visible_ms =
                    self.totals.foreground_visible_ms.saturating_add(elapsed);
            }
            if focused {
                self.totals.focused_ms = self.totals.focused_ms.saturating_add(elapsed);
                self.totals.interaction_active_ms =
                    self.totals.interaction_active_ms.saturating_add(recent);
            }
            self.totals.idle_ms = self
                .totals
                .idle_ms
                .saturating_add(elapsed.saturating_sub(recent));
        }
        self.last_tick = now_ms;
    }

    /// Returns false when the bounded window registry is full.
    pub fn window(&mut self, id: &str, visible: bool, focused: bool, now_ms: u64) -> bool {
        self.advance(now_ms);
        if !visible && !focused {
            self.windows.remove(id);
            return true;
        }
        if self.windows.len() >= 256 && !self.windows.contains_key(id) {
            return false;
        }
        self.windows.insert(id.to_owned(), (visible, focused));
        true
    }

    pub fn interaction(&mut self, now_ms: u64) {
        self.advance(now_ms);
        if !self.suspended
            && self
                .windows
                .values()
                .any(|(visible, focused)| *visible && *focused)
        {
            self.last_input = Some(self.last_tick);
        }
    }

    pub fn suspend(&mut self, suspended: bool, now_ms: u64) {
        self.advance(now_ms);
        if self.suspended != suspended {
            self.last_input = None;
        }
        self.suspended = suspended;
    }

    pub fn totals(&self) -> LifecycleTotals {
        self.totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_does_not_imply_interaction_and_idle_boundary_is_exact() {
        let mut lifecycle = Lifecycle::new(0, 90);
        lifecycle.window("main", true, true, 0);
        lifecycle.advance(100);
        assert_eq!(lifecycle.totals().interaction_active_ms, 0);
        lifecycle.interaction(100);
        lifecycle.advance(1_000);
        assert_eq!(lifecycle.totals().interaction_active_ms, 90);
        assert_eq!(lifecycle.totals().idle_ms, 910);
        assert_eq!(lifecycle.totals().focused_ms, 1_000);
    }

    #[test]
    fn overlapping_windows_count_once_and_suspension_excludes_gap() {
        let mut lifecycle = Lifecycle::new(0, 90);
        lifecycle.window("one", true, true, 0);
        lifecycle.window("two", true, true, 10);
        lifecycle.interaction(10);
        lifecycle.window("one", false, false, 20);
        lifecycle.suspend(true, 30);
        lifecycle.suspend(false, 1_000);
        lifecycle.advance(1_010);
        let totals = lifecycle.totals();
        assert_eq!(totals.running_ms, 40);
        assert_eq!(totals.foreground_visible_ms, 40);
        assert_eq!(totals.focused_ms, 40);
        assert_eq!(totals.interaction_active_ms, 20);
        assert_eq!(totals.idle_ms, 20);
    }

    #[test]
    fn invisible_windows_are_ineligible_and_backward_ticks_are_safe() {
        let mut lifecycle = Lifecycle::new(10, 90);
        lifecycle.window("main", false, true, 10);
        lifecycle.interaction(10);
        lifecycle.advance(100);
        lifecycle.advance(1);
        assert_eq!(lifecycle.totals().running_ms, 90);
        assert_eq!(lifecycle.totals().focused_ms, 0);
        assert_eq!(lifecycle.totals().interaction_active_ms, 0);
    }
}
