use super::{nip17::IMAGE_PLACEHOLDER, service::message_id};
use crate::{
    protocol::{Id, Message, REPLY_EXCERPT_CHARS, ReplyRef},
    state::AppState,
};

/// The quote a DM reply carries. Until its original is held the quote has no
/// author, `features::chat` draws none, and `resolve_waiting` fills it in.
pub fn quote(
    state: &AppState,
    channel: Id,
    author: &str,
    at: i64,
    target: &str,
) -> Option<ReplyRef> {
    let target = message_id(target);
    match state
        .messages
        .get(&channel)
        .and_then(|held| held.iter().find(|m| m.id == target))
    {
        Some(original) if is_caption(original, author, at) => None,
        Some(original) => Some(resolved(original)),
        None => Some(ReplyRef {
            message_id: target,
            author_pubkey: String::new(),
            author_username: String::new(),
            excerpt: String::new(),
        }),
    }
}

pub fn resolve_waiting(state: &mut AppState, channel: Id, original: Id) {
    let Some(held) = state.messages.get_mut(&channel) else {
        return;
    };
    let Some(found) = held.iter().find(|m| m.id == original).cloned() else {
        return;
    };
    for reply in held.iter_mut() {
        if reply
            .reply_to
            .as_ref()
            .is_some_and(|q| q.message_id == original && waiting(q))
        {
            reply.reply_to =
                (!is_caption(&found, &reply.author.pubkey, reply.created_at.timestamp()))
                    .then(|| resolved(&found));
        }
    }
}

pub fn waiting(quote: &ReplyRef) -> bool {
    quote.author_pubkey.is_empty()
}

// A caption is sent as a reply to its own picture, stamped the same second; filing
// may move either up to the clock deadband, so a quick self-reply reads as one too.
fn is_caption(original: &Message, author: &str, at: i64) -> bool {
    original.content == IMAGE_PLACEHOLDER
        && original.author.pubkey == author
        && (original.created_at.timestamp() - at).abs() <= super::calls::CLOCK_DEADBAND
}

fn resolved(original: &Message) -> ReplyRef {
    let flat = original
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let excerpt = if flat.chars().count() > REPLY_EXCERPT_CHARS {
        format!(
            "{}…",
            flat.chars().take(REPLY_EXCERPT_CHARS).collect::<String>()
        )
    } else {
        flat
    };
    ReplyRef {
        message_id: original.id,
        author_pubkey: original.author.pubkey.clone(),
        author_username: original.author.username.clone(),
        excerpt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::User;

    const ME: &str = "ab";
    const PEER: &str = "cd";

    fn message(
        rumor: &str,
        author: &str,
        content: &str,
        at: i64,
        reply_to: Option<ReplyRef>,
    ) -> Message {
        Message {
            id: message_id(rumor),
            channel_id: Id::nil(),
            author: User {
                pubkey: author.into(),
                username: author.into(),
            },
            author_bot: false,
            content: content.into(),
            image: None,
            image_metadata: None,
            attachment: None,
            reactions: Vec::new(),
            reply_to,
            edited_at: None,
            pinned: false,
            created_at: chrono::DateTime::from_timestamp(at, 0).unwrap(),
        }
    }

    fn held(messages: Vec<Message>) -> AppState {
        let mut state = AppState::empty();
        state.messages.insert(Id::nil(), messages);
        state
    }

    #[test]
    fn a_reply_quotes_its_original() {
        let long = "word ".repeat(40);
        let state = held(vec![message("q", PEER, &long, 100, None)]);
        let quote = quote(&state, Id::nil(), ME, 500, "q").unwrap();
        assert_eq!(quote.message_id, message_id("q"));
        assert_eq!(quote.author_pubkey, PEER);
        assert_eq!(quote.excerpt.chars().count(), REPLY_EXCERPT_CHARS + 1);
        assert!(!waiting(&quote));
    }

    #[test]
    fn a_caption_does_not_quote_its_picture_in_either_order() {
        let picture = || message("pic", ME, IMAGE_PLACEHOLDER, 100, None);
        let state = held(vec![picture()]);
        assert_eq!(quote(&state, Id::nil(), ME, 100, "pic"), None);
        assert!(
            quote(&state, Id::nil(), PEER, 100, "pic").is_some(),
            "the peer's reply is a reply"
        );

        let early = quote(&AppState::empty(), Id::nil(), ME, 100, "pic").unwrap();
        assert!(waiting(&early));
        let mut state = held(vec![
            message("cap", ME, "look", 100, Some(early)),
            picture(),
        ]);
        resolve_waiting(&mut state, Id::nil(), message_id("pic"));
        assert_eq!(state.messages[&Id::nil()][0].reply_to, None);
    }

    #[test]
    fn a_reply_held_before_its_original_is_resolved_when_it_arrives() {
        let early = quote(&AppState::empty(), Id::nil(), ME, 500, "q").unwrap();
        let mut state = held(vec![
            message("q", PEER, "question", 100, None),
            message("a", ME, "answer", 500, Some(early)),
        ]);
        resolve_waiting(&mut state, Id::nil(), message_id("q"));
        let quote = state.messages[&Id::nil()][1].reply_to.clone().unwrap();
        assert_eq!(
            (quote.author_pubkey.as_str(), quote.excerpt.as_str()),
            (PEER, "question")
        );
    }
}
