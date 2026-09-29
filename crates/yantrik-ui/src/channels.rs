//! What the desktop knows about each channel a person can reach it from
//! (design/channels-2026-09-29.md).

/// Who besides the person can read a channel: `e2e` when it is end-to-end to this box, else
/// `provider-readable` — the operator can read it, as Telegram can a bot's chats. An unknown
/// channel is the latter: saying a channel is private when it is not is the mistake that matters.
pub fn trust_of(provider: &str) -> &'static str {
    match provider {
        "signal" | "native" => "e2e",
        _ => "provider-readable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_end_to_end_channels_say_so() {
        assert_eq!(trust_of("signal"), "e2e");
        assert_eq!(trust_of("telegram"), "provider-readable");
        assert_eq!(trust_of("whatsapp"), "provider-readable", "the Cloud API is Meta-readable");
        assert_eq!(trust_of("carrier-pigeon"), "provider-readable");
    }
}
