//! Call-id rules shared by core and Tauri.
//!
//! Every call gets an id when the frontend starts it. Messages about a call carry that id,
//! and each side keeps the id of the call it considers current. A message about any other
//! id is about a call that is already over and must not touch the current one.

use crate::CallId;

/// What to do with a `CallEnd(requested)` given the current call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndAction {
    /// Tear down the current call and report it in `CallEnded(ended)`.
    TearDown { ended: CallId },
    /// Nothing to tear down: `requested` is an older call, one that already ended, or there
    /// is no call. Only acknowledge a named call with `CallEnded(id)`.
    Acknowledge { requested: Option<CallId> },
}

pub fn resolve_call_end(requested: Option<CallId>, current: Option<CallId>) -> CallEndAction {
    match (requested, current) {
        (Some(requested), Some(current)) if requested != current => CallEndAction::Acknowledge {
            requested: Some(requested),
        },
        (_, Some(current)) => CallEndAction::TearDown { ended: current },
        (requested, None) => CallEndAction::Acknowledge { requested },
    }
}

/// Whether `CallEnded(message_call)` concerns the `current` call.
pub fn concerns_current_call(message_call: CallId, current: Option<CallId>) -> bool {
    current == Some(message_call)
}

/// Tauri-side view of the call lifecycle. Every method is a pure state transition; the
/// caller applies the side effects (shortcuts, dock icon, sleep prevention) when a method
/// says so, while still holding the lock that guards the tracker, so effects are queued in
/// the same order as the transitions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CallTracker {
    current: Option<CallId>,
    active: bool,
}

impl CallTracker {
    pub fn current(&self) -> Option<CallId> {
        self.current
    }

    /// True once core confirmed the current call started.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The UI is starting call `call_id` (before CallStart is sent).
    pub fn begin(&mut self, call_id: CallId) {
        self.current = Some(call_id);
        self.active = false;
    }

    /// Core answered CallStart for `call_id`. Returns true when the call just became active
    /// (apply call-started effects); false for a stale or failed start.
    pub fn on_start_result(&mut self, call_id: CallId, ok: bool) -> bool {
        if self.current != Some(call_id) || !ok || self.active {
            return false;
        }
        self.active = true;
        true
    }

    /// The UI ended call `call_id` (`None`: whatever is current). Returns true when the
    /// current call was cleared (apply call-ended effects); false when `call_id` names an
    /// older call, which must not touch the current one.
    pub fn end(&mut self, call_id: Option<CallId>) -> bool {
        if let (Some(requested), Some(current)) = (call_id, self.current) {
            if requested != current {
                return false;
            }
        }
        self.clear()
    }

    /// Core reported `CallEnded(call_id)`. Returns true when that was the current call.
    pub fn on_call_ended(&mut self, call_id: CallId) -> bool {
        if !concerns_current_call(call_id, self.current) {
            return false;
        }
        self.clear()
    }

    /// Core went away (restart). Returns the call that was current, if any.
    pub fn reset(&mut self) -> Option<CallId> {
        let current = self.current;
        self.clear();
        current
    }

    fn clear(&mut self) -> bool {
        let had_call = self.current.is_some() || self.active;
        self.current = None;
        self.active = false;
        // Effects are idempotent; report whether there was anything to undo.
        had_call
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_end_for_the_current_call_tears_it_down() {
        assert_eq!(
            resolve_call_end(Some(2), Some(2)),
            CallEndAction::TearDown { ended: 2 }
        );
    }

    #[test]
    fn call_end_for_an_older_call_only_acknowledges() {
        assert_eq!(
            resolve_call_end(Some(1), Some(2)),
            CallEndAction::Acknowledge { requested: Some(1) }
        );
    }

    #[test]
    fn call_end_without_id_ends_whatever_is_active() {
        assert_eq!(
            resolve_call_end(None, Some(3)),
            CallEndAction::TearDown { ended: 3 }
        );
    }

    #[test]
    fn call_end_with_no_active_call_does_no_teardown() {
        // e.g. the UI's CallEnd after a hang-up in a core window already ended the call,
        // or a CallEnd after a failed CallStart.
        assert_eq!(
            resolve_call_end(Some(4), None),
            CallEndAction::Acknowledge { requested: Some(4) }
        );
        assert_eq!(
            resolve_call_end(None, None),
            CallEndAction::Acknowledge { requested: None }
        );
    }

    #[test]
    fn call_ended_only_concerns_the_matching_call() {
        assert!(concerns_current_call(5, Some(5)));
        assert!(
            !concerns_current_call(4, Some(5)),
            "late CallEnded of an older call"
        );
        assert!(!concerns_current_call(5, None));
    }

    #[test]
    fn tracker_activates_only_on_a_successful_result_for_the_current_call() {
        let mut tracker = CallTracker::default();
        tracker.begin(1);
        assert!(!tracker.on_start_result(1, false), "failed start");
        assert!(!tracker.is_active());
        assert!(!tracker.on_start_result(7, true), "result for another call");
        assert!(tracker.on_start_result(1, true));
        assert!(tracker.is_active());
        assert!(!tracker.on_start_result(1, true), "effects applied once");
    }

    #[test]
    fn late_call_ended_of_the_previous_call_does_not_end_the_new_one() {
        // Wire order on a quick end-then-start: CallEnded(1), ..., CallStartResult(2).
        let mut tracker = CallTracker::default();
        tracker.begin(1);
        tracker.on_start_result(1, true);
        assert!(tracker.end(Some(1)), "UI hangs up call 1");
        tracker.begin(2);
        assert!(!tracker.on_call_ended(1));
        assert!(tracker.on_start_result(2, true));
        assert!(!tracker.on_call_ended(1));
        assert_eq!(tracker.current(), Some(2));
        assert!(tracker.is_active());
    }

    #[test]
    fn ui_end_for_an_older_call_is_ignored() {
        let mut tracker = CallTracker::default();
        tracker.begin(2);
        assert!(!tracker.end(Some(1)));
        assert_eq!(tracker.current(), Some(2));
    }

    #[test]
    fn start_result_after_the_ui_already_ended_the_call_is_ignored() {
        let mut tracker = CallTracker::default();
        tracker.begin(3);
        assert!(tracker.end(Some(3)));
        assert!(!tracker.on_start_result(3, true));
        assert!(!tracker.is_active());
    }

    #[test]
    fn hang_up_in_a_core_window_then_ui_cleanup_is_idempotent() {
        let mut tracker = CallTracker::default();
        tracker.begin(4);
        tracker.on_start_result(4, true);
        assert!(tracker.on_call_ended(4));
        assert!(!tracker.end(Some(4)));
        assert!(!tracker.on_call_ended(4), "second CallEnded echo");
    }

    #[test]
    fn reset_returns_the_call_that_was_current() {
        let mut tracker = CallTracker::default();
        tracker.begin(5);
        assert_eq!(tracker.reset(), Some(5));
        assert_eq!(tracker.reset(), None);
    }
}
