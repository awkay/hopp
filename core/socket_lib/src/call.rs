//! Call-id rules shared by core and Tauri.
//!
//! Every call gets an id when the frontend starts it. Messages about a call carry that id,
//! and each side keeps the id of the call it considers current. A message about any other
//! id is about a call that is already over and must not touch the current one.

use crate::CallId;

/// What to do with a `CallEnd(requested)` given the current call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndAction {
    /// The request names a call other than the current one: leave the current call alone.
    Stale,
    /// Tear down (idempotent when no call is active) and report `ended` in `CallEnded`.
    TearDown { ended: Option<CallId> },
}

pub fn resolve_call_end(requested: Option<CallId>, current: Option<CallId>) -> CallEndAction {
    match (requested, current) {
        (Some(requested), Some(current)) if requested != current => CallEndAction::Stale,
        (_, Some(current)) => CallEndAction::TearDown {
            ended: Some(current),
        },
        (requested, None) => CallEndAction::TearDown { ended: requested },
    }
}

/// Whether a message tagged `message_call` (from `CallEnded`) concerns the `current` call.
/// `None` on the message means "whatever call core had" and matches any current call.
pub fn concerns_current_call(message_call: Option<CallId>, current: Option<CallId>) -> bool {
    match (message_call, current) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(message_call), Some(current)) => message_call == current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_end_for_the_current_call_tears_it_down() {
        assert_eq!(
            resolve_call_end(Some(2), Some(2)),
            CallEndAction::TearDown { ended: Some(2) }
        );
    }

    #[test]
    fn call_end_for_an_older_call_is_stale() {
        assert_eq!(resolve_call_end(Some(1), Some(2)), CallEndAction::Stale);
    }

    #[test]
    fn call_end_without_id_ends_whatever_is_active() {
        assert_eq!(
            resolve_call_end(None, Some(3)),
            CallEndAction::TearDown { ended: Some(3) }
        );
    }

    #[test]
    fn call_end_with_no_active_call_is_an_idempotent_teardown_tagged_with_the_request() {
        // e.g. the second CallEnd after a hang-up in a core window, or a CallEnd after a
        // failed CallStart: cleanup is harmless and the CallEnded echo carries the id.
        assert_eq!(
            resolve_call_end(Some(4), None),
            CallEndAction::TearDown { ended: Some(4) }
        );
        assert_eq!(
            resolve_call_end(None, None),
            CallEndAction::TearDown { ended: None }
        );
    }

    #[test]
    fn call_ended_only_concerns_the_matching_call() {
        assert!(concerns_current_call(Some(5), Some(5)));
        assert!(
            !concerns_current_call(Some(4), Some(5)),
            "late CallEnded of an older call"
        );
        assert!(!concerns_current_call(Some(5), None));
        assert!(concerns_current_call(None, Some(5)));
        assert!(!concerns_current_call(None, None));
    }
}
