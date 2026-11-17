//! Action feedback is separate from the persistent canvas activity summary.
use crate::terminal_status::State;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Notice,
    Progress,
    Error,
}

#[derive(Default)]
pub(super) struct StatusMessage {
    current: Option<(&'static str, Kind, String)>,
    deadline: Option<Instant>,
    phase: Option<State>,
}

impl StatusMessage {
    fn set(&mut self, action: &'static str, kind: Kind, status: &mut String, text: String) {
        // Background reloads and unrelated successes must not erase an error.
        if kind != Kind::Error
            && self
                .current
                .as_ref()
                .is_some_and(|(owner, current, previous)| {
                    *current == Kind::Error && *owner != action && previous == status
                })
        {
            return;
        }
        *status = text.clone();
        self.current = Some((action, kind, text));
        self.phase = None;
        self.deadline = (kind == Kind::Notice).then(|| Instant::now() + Duration::from_secs(5));
    }

    pub fn notice(&mut self, action: &'static str, status: &mut String, text: String) {
        self.set(action, Kind::Notice, status, text);
    }

    pub fn progress(&mut self, action: &'static str, status: &mut String, text: String) {
        self.set(action, Kind::Progress, status, text);
    }

    pub fn error(&mut self, action: &'static str, status: &mut String, text: String) {
        self.set(action, Kind::Error, status, text);
    }

    pub fn result(
        &mut self,
        action: &'static str,
        status: &mut String,
        result: Result<String, String>,
    ) {
        match result {
            Ok(text) => self.notice(action, status, text),
            Err(text) => self.error(action, status, text),
        }
    }

    pub fn is_error(&self, status: &str) -> bool {
        self.current
            .as_ref()
            .is_some_and(|(_, kind, text)| *kind == Kind::Error && text == status)
    }

    fn clear(&mut self, status: &mut String) {
        if self
            .current
            .as_ref()
            .is_some_and(|(_, _, text)| text == status)
        {
            status.clear();
        }
        self.current = None;
        self.deadline = None;
        self.phase = None;
    }

    pub fn retry(&mut self, action: &str, status: &mut String) {
        if self
            .current
            .as_ref()
            .is_some_and(|(owner, _, _)| *owner == action)
        {
            self.clear(status);
        }
    }

    pub fn observe_state(&mut self, status: &mut String, state: &State) {
        if self
            .current
            .as_ref()
            .is_some_and(|(_, kind, _)| *kind == Kind::Progress)
        {
            if self
                .phase
                .as_ref()
                .is_some_and(|previous| previous != state)
            {
                self.clear(status);
            } else {
                self.phase = Some(state.clone());
            }
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn expire(&mut self, status: &mut String) {
        self.clear(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_lifetime_follows_its_kind_and_action() {
        let mut message = StatusMessage::default();
        let mut text = String::new();
        message.notice("form-submit", &mut text, "Form submitted".into());
        assert!(message.deadline().is_some());
        message.expire(&mut text);
        assert!(text.is_empty());

        message.progress("form-wait", &mut text, "Waiting for form".into());
        assert!(message.deadline().is_none());
        message.observe_state(&mut text, &State::Waiting("form".into()));
        assert_eq!(text, "Waiting for form");
        message.observe_state(&mut text, &State::Running("card".into()));
        assert!(text.is_empty());

        message.error("form-submit", &mut text, "Invalid value".into());
        assert!(message.is_error(&text));
        assert!(message.deadline().is_none());
        message.observe_state(&mut text, &State::Completed("card".into()));
        message.notice("reload", &mut text, "Reloaded".into());
        message.retry("open", &mut text);
        assert_eq!(text, "Invalid value");
        message.retry("form-submit", &mut text);
        assert!(text.is_empty());
    }
}
