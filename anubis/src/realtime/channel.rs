//! Channel names: the addresses realtime events are published to.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use uuid::Uuid;

/// The longest a topic may be.
///
/// Names arrive from browsers, so the length is bounded where it is parsed.
/// Sixty-four characters is far past any name a person would write and keeps
/// a whole channel name inside a single cache line's worth of text.
const MAX_TOPIC: usize = 64;

/// What separates the parts of a channel name, and so may not appear in one.
const SEPARATOR: char = ':';

/// The audience a channel name addresses.
///
/// The audience is also the authorization rule: a team channel admits the
/// team's members, and a user channel admits that one user. Adding a variant
/// means adding the rule that goes with it, which is why the set is small and
/// deliberate. See `docs/realtime.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    /// Everyone who belongs to the team.
    Team(Uuid),
    /// One user, on every device they are signed in on.
    User(Uuid),
}

impl Audience {
    /// The prefix this audience renders as.
    fn prefix(self) -> &'static str {
        match self {
            Self::Team(_) => "team",
            Self::User(_) => "user",
        }
    }

    /// The id this audience names.
    fn id(self) -> Uuid {
        match self {
            Self::Team(id) | Self::User(id) => id,
        }
    }
}

/// A channel name: an audience and a topic, such as `team:{uuid}:projects`.
///
/// Names are structured rather than free-form so that the server can decide
/// who may listen without consulting a registry of hand-written rules. Build
/// one with [`ChannelName::team`] or [`ChannelName::user`], parse one from a
/// browser with `str::parse`, and render one with [`Display`].
///
/// # Examples
/// ```
/// use anubis::realtime::{Audience, ChannelName};
/// use uuid::Uuid;
///
/// let team_id = Uuid::nil();
/// let channel = ChannelName::team(team_id, "projects");
///
/// assert_eq!(channel.to_string(), format!("team:{team_id}:projects"));
/// assert_eq!(channel.audience(), Audience::Team(team_id));
/// assert_eq!(channel.topic(), "projects");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelName {
    audience: Audience,
    topic: String,
}

impl ChannelName {
    /// Names a topic every member of `team_id` may listen to.
    ///
    /// # Panics
    /// Panics when `topic` is not a valid topic: one to sixty-four characters
    /// of lowercase ASCII letters, digits, `_`, `-`, or `.`. Topics are
    /// literals at the call site, so a bad one is a programming error rather
    /// than a runtime condition; parse untrusted names with `str::parse`
    /// instead.
    #[must_use]
    pub fn team(team_id: Uuid, topic: impl AsRef<str>) -> Self {
        Self::new(Audience::Team(team_id), topic.as_ref())
    }

    /// Names a topic only `user_id` may listen to.
    ///
    /// # Panics
    /// Panics on an invalid topic; see [`ChannelName::team`].
    #[must_use]
    pub fn user(user_id: Uuid, topic: impl AsRef<str>) -> Self {
        Self::new(Audience::User(user_id), topic.as_ref())
    }

    /// Returns who may listen to this channel.
    #[must_use]
    pub fn audience(&self) -> Audience {
        self.audience
    }

    /// Returns the topic, the part of the name the application chose.
    #[must_use]
    pub fn topic(&self) -> &str {
        &self.topic
    }

    fn new(audience: Audience, topic: &str) -> Self {
        assert!(
            is_valid_topic(topic),
            "{topic:?} is not a valid channel topic: expected 1 to {MAX_TOPIC} characters of \
             lowercase ASCII letters, digits, `_`, `-`, or `.`",
        );
        Self {
            audience,
            topic: topic.to_owned(),
        }
    }
}

impl Display for ChannelName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{SEPARATOR}{}{SEPARATOR}{}",
            self.audience.prefix(),
            self.audience.id(),
            self.topic,
        )
    }
}

impl FromStr for ChannelName {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.splitn(3, SEPARATOR);
        let (Some(prefix), Some(id), Some(topic)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(ParseError::new(
                "a channel name is audience:id:topic, such as team:{uuid}:projects",
            ));
        };

        let id = id
            .parse::<Uuid>()
            .map_err(|_error| ParseError::new("the second part of a channel name is a UUID"))?;

        let audience = match prefix {
            "team" => Audience::Team(id),
            "user" => Audience::User(id),
            _ => return Err(ParseError::new("a channel name begins with team or user")),
        };

        if !is_valid_topic(topic) {
            return Err(ParseError::new(
                "a topic is 1 to 64 characters of lowercase ASCII letters, digits, _, -, or .",
            ));
        }

        Ok(Self {
            audience,
            topic: topic.to_owned(),
        })
    }
}

/// Returns `true` for a topic that renders and parses back unchanged.
///
/// The alphabet is deliberately narrow. Uppercase is excluded because two
/// names differing only in case would look identical to a person and route
/// differently, and the separator is excluded because it would split the name
/// somewhere else on the way back in.
fn is_valid_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic.len() <= MAX_TOPIC
        && topic
            .chars()
            .all(|character| matches!(character, 'a'..='z' | '0'..='9' | '_' | '-' | '.'))
}

/// A string is not a channel name.
#[derive(Debug)]
pub struct ParseError {
    expected: &'static str,
    backtrace: Backtrace,
}

impl ParseError {
    fn new(expected: &'static str) -> Self {
        Self {
            expected,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "invalid channel name: expected {}", self.expected)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{Audience, ChannelName};

    #[test]
    fn names_render_and_parse_back_unchanged() {
        let team_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        for channel in [
            ChannelName::team(team_id, "projects"),
            ChannelName::user(user_id, "notifications"),
            ChannelName::team(team_id, "projects.comments-2"),
        ] {
            let rendered = channel.to_string();
            let parsed: ChannelName = rendered.parse().expect("a rendered name must parse");
            assert_eq!(parsed, channel, "for {rendered}");
        }
    }

    #[test]
    fn the_audience_carries_the_id_the_name_names() {
        let team_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        assert_eq!(
            ChannelName::team(team_id, "projects").audience(),
            Audience::Team(team_id),
        );
        assert_eq!(
            ChannelName::user(user_id, "inbox").audience(),
            Audience::User(user_id),
        );
    }

    #[test]
    fn names_a_browser_could_send_but_never_route_are_rejected() {
        let id = Uuid::new_v4();

        for value in [
            "",
            "team",
            &format!("team:{id}"),
            &format!("organization:{id}:audits"),
            "team:not-a-uuid:projects",
            &format!("team:{id}:"),
            &format!("team:{id}:Projects"),
            &format!("team:{id}:with space"),
            &format!("team:{id}:{}", "x".repeat(65)),
        ] {
            assert!(
                value.parse::<ChannelName>().is_err(),
                "{value:?} must be rejected",
            );
        }
    }

    #[test]
    fn a_topic_may_not_smuggle_a_separator() {
        // `splitn(3, ..)` leaves the rest of the string as the topic, so the
        // topic alphabet is what stops `team:{id}:a:b` from parsing.
        let id = Uuid::new_v4();
        let _refused = format!("team:{id}:a:b").parse::<ChannelName>().unwrap_err();
    }

    #[test]
    #[should_panic(expected = "not a valid channel topic")]
    fn building_a_name_from_an_invalid_topic_is_a_programming_error() {
        let _channel = ChannelName::team(Uuid::new_v4(), "Not A Topic");
    }
}
