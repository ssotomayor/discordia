//! What a session sent lately, so a rate limit can name what tripped it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The server counts over this long (`RATE_WINDOW` in its gateway).
const WINDOW: Duration = Duration::from_secs(10);
/// Past the server's flood limit nothing more is learned.
const KEPT: usize = 512;
const NAMED: usize = 3;

#[derive(Default)]
pub(super) struct SentLately(VecDeque<(Instant, String)>);

impl SentLately {
    pub(super) fn note(&mut self, op: &str, now: Instant) {
        self.forget_before(now);
        if self.0.len() == KEPT {
            self.0.pop_front();
        }
        self.0.push_back((now, op.to_owned()));
    }

    fn forget_before(&mut self, now: Instant) {
        while self
            .0
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.0.pop_front();
        }
    }

    /// The busiest operations of the window, most frequent first.
    pub(super) fn summary(&mut self, now: Instant) -> String {
        self.forget_before(now);
        let mut counts = Vec::<(&str, usize)>::new();
        for (_, op) in &self.0 {
            match counts.iter_mut().find(|(name, _)| name == op) {
                Some((_, count)) => *count += 1,
                None => counts.push((op, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        counts
            .iter()
            .take(NAMED)
            .map(|(op, count)| format!("{op} ×{count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The refusal with what was being sent when it came. Only the server's generic
/// refusal: the others say what they were about, and one is translated by its exact text.
pub(super) fn explain(message: String, sent: &str) -> String {
    if message == discordia_server::gateway::RATE_LIMITED && !sent.is_empty() {
        format!("{message} ({sent})")
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busiest_operations_of_the_last_window_are_named_in_order() {
        let start = Instant::now();
        let mut sent = SentLately::default();
        sent.note("SendMessage", start);
        for i in 0..40 {
            sent.note(
                "FetchEmoji",
                start + Duration::from_secs(5) + Duration::from_millis(i),
            );
        }
        for _ in 0..3 {
            sent.note("FetchMessages", start + Duration::from_secs(6));
        }
        sent.note("Typing", start + Duration::from_secs(6));
        sent.note("SetAttention", start + Duration::from_secs(6));
        let now = start + Duration::from_secs(12);
        assert_eq!(
            sent.summary(now),
            "FetchEmoji ×40, FetchMessages ×3, SetAttention ×1",
            "what left the window is not counted, and a tie goes by name"
        );
        assert_eq!(sent.summary(now + WINDOW + Duration::from_secs(7)), "");
    }

    #[test]
    fn a_flood_does_not_grow_the_record() {
        let now = Instant::now();
        let mut sent = SentLately::default();
        for _ in 0..KEPT * 3 {
            sent.note("SetSpeaking", now);
        }
        assert_eq!(sent.0.len(), KEPT);
        assert_eq!(sent.summary(now), format!("SetSpeaking ×{KEPT}"));
    }

    #[test]
    fn only_the_generic_refusal_is_explained() {
        let generic = discordia_server::gateway::RATE_LIMITED.to_string();
        assert_eq!(
            explain(generic.clone(), "FetchEmoji ×61"),
            format!("{generic} (FetchEmoji ×61)")
        );
        assert_eq!(explain(generic.clone(), ""), generic);
        let translated = "rate limited: too many messages, slow down".to_string();
        assert_eq!(explain(translated.clone(), "SendMessage ×31"), translated);
    }
}
