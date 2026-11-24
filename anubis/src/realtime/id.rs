//! Event ids: where an event sits in the deployment's one realtime sequence.

use std::fmt::{self, Display, Formatter};

/// Where one published event sits in the deployment's realtime sequence.
///
/// Every publish takes the next number of one sequence shared by every
/// channel, so ids order events across channels as well as within one. That
/// is what lets a stream carrying several channels resume from a single id:
/// a reconnecting client sends the last id it received as `Last-Event-ID`,
/// and receives every event after it on the channels it listens to.
///
/// The epoch names one run of the sequence. In process it is minted when the
/// [`super::Channels`] service starts. Under Redis it is stored beside the
/// sequence and the `run_id` of the Redis server holding them, and changes
/// whenever that server is a different run: a restart, whether it comes back
/// empty or from any snapshot or append-only file, or a replica promoted in
/// its place. So a planned Redis restart resets every client that resumes
/// across it, even one that kept every byte. An id from another epoch cannot
/// be resumed from, so it is answered with a reset rather than with a guess.
///
/// Ids render as `{epoch}-{sequence}`, the epoch in sixteen hex digits, such
/// as `3f2b9c0d1e4a5b6c-42`. Clients treat the text as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId {
    epoch: u64,
    sequence: u64,
}

impl EventId {
    /// Builds an id from its two halves.
    pub(crate) fn new(epoch: u64, sequence: u64) -> Self {
        Self { epoch, sequence }
    }

    /// The run of the sequence this id belongs to.
    pub(crate) fn epoch(self) -> u64 {
        self.epoch
    }

    /// The position in that run, counting from one.
    pub(crate) fn sequence(self) -> u64 {
        self.sequence
    }

    /// Reads an id a client sent back, or `None` for anything this server did
    /// not render.
    ///
    /// The text arrives in a request header, so it is parsed strictly: exactly
    /// sixteen lowercase hex digits, a hyphen, and a decimal sequence.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (epoch, sequence) = text.split_once('-')?;
        let lowercase_hex = epoch.len() == EPOCH_DIGITS
            && epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !lowercase_hex
            || sequence.is_empty()
            || !sequence.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }

        Some(Self {
            epoch: u64::from_str_radix(epoch, 16).ok()?,
            sequence: sequence.parse().ok()?,
        })
    }

    /// Renders an epoch the way ids carry it, which is also how Redis stores it.
    pub(crate) fn render_epoch(epoch: u64) -> String {
        format!("{epoch:016x}")
    }

    /// Reads an epoch back from that rendering.
    pub(crate) fn parse_epoch(text: &str) -> Option<u64> {
        if text.len() != EPOCH_DIGITS {
            return None;
        }
        u64::from_str_radix(text, 16).ok()
    }
}

/// How many hex digits an epoch renders as: all sixty-four bits, zero padded,
/// so every id of one epoch shares its prefix character for character.
const EPOCH_DIGITS: usize = 16;

impl Display for EventId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}-{}", self.epoch, self.sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::EventId;

    #[test]
    fn an_id_renders_and_parses_back_unchanged() {
        for id in [
            EventId::new(0, 1),
            EventId::new(0x3f2b_9c0d_1e4a_5b6c, 42),
            EventId::new(u64::MAX, u64::MAX),
        ] {
            let rendered = id.to_string();
            assert_eq!(EventId::parse(&rendered), Some(id), "for {rendered}");
        }
        assert_eq!(EventId::new(0xab, 7).to_string(), "00000000000000ab-7");
    }

    #[test]
    fn text_this_server_never_rendered_is_not_an_id() {
        for text in [
            "",
            "-",
            "42",
            "00000000000000ab",
            "00000000000000ab-",
            "00000000000000AB-7",
            "ab-7",
            "000000000000000ab-7",
            "00000000000000ab--7",
            "00000000000000ab-+7",
            "00000000000000ab-7 ",
            "00000000000000ab-99999999999999999999999",
            "zzzzzzzzzzzzzzzz-7",
        ] {
            assert_eq!(EventId::parse(text), None, "{text:?} must be refused");
        }
    }

    #[test]
    fn an_epoch_round_trips_through_its_stored_form() {
        let epoch = 0x0123_4567_89ab_cdef;
        let stored = EventId::render_epoch(epoch);
        assert_eq!(stored, "0123456789abcdef");
        assert_eq!(EventId::parse_epoch(&stored), Some(epoch));
        assert_eq!(EventId::parse_epoch("123"), None);
    }
}
