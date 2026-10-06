//! What a filed issue says: its title, its body, and a recurrence comment.
//!
//! **Nothing that reaches an issue is trusted as markdown.** Every value here is
//! an error's own text, a browser's user agent, or a message a failing page
//! produced, and some of it was posted by a stranger. A heading, an image, or an
//! `@team` mention in one of them would render live in an issue nobody wrote,
//! and a mention notifies real people. So every value is rendered literally: a
//! summary and a fact as a code span that cannot be escaped, and a detail as a
//! fenced block that cannot be closed early.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};

use super::{Sample, Source};

/// Longest summary that reaches an issue title.
///
/// GitHub accepts 256 characters. A title is read in a list, so this is about
/// what fits there rather than what the API allows.
const TITLE_LIMIT: usize = 160;

/// Longest detail block that reaches an issue body.
///
/// A stack is the useful part of most reports and the tail of a deep one is
/// almost never read. Enough for any stack worth having.
const DETAIL_LIMIT: usize = 8_000;

/// Longest detail block a recurrence comment quotes.
///
/// Shorter than the body's: the body already quotes the first occurrence in
/// full, and the comment is there to say how often, not to repeat it.
const COMMENT_DETAIL_LIMIT: usize = 2_000;

/// The issue title: which half of the application, then what happened.
#[must_use]
pub fn title(source: Source, summary: &str) -> String {
    format!(
        "[{}] {}",
        source.as_str(),
        // Flattened for the same reason the body's values are, minus the
        // markdown: a title is plain text to the tracker, and a newline in one
        // is left to its mercy rather than decided here.
        clip(&one_line(summary), TITLE_LIMIT)
    )
}

/// What a new issue is opened with.
pub(super) struct Opening<'a> {
    pub source: Source,
    pub kind: &'a str,
    pub fingerprint: &'a str,
    pub occurrences: i64,
    pub sample: &'a Sample,
    /// The product that filed it, named in the footer.
    pub product: &'a str,
    /// The closed issue this one replaces, when the fault came back.
    pub recurrence_of: Option<i64>,
}

/// The issue body, as markdown.
///
/// The footer is load bearing rather than decorative: whoever reads the issue
/// has to know that recurrences are counted here rather than filed again, and
/// that closing it is what lets the next occurrence open a fresh one. **The
/// fingerprint appears in the body and nowhere else**, because a retry that
/// may already have filed searches bodies for it; see
/// [`super::Tracker::find_open`].
#[must_use]
pub(super) fn body(opening: &Opening<'_>) -> String {
    let sample = opening.sample;
    let mut body = String::with_capacity(sample.detail.len() + 1_024);

    // Writing into a String cannot fail, so every result below is discarded
    // rather than propagated through a function that has nothing to report.
    let _ = writeln!(body, "{}\n", inline_code(&sample.summary));

    if let Some(previous) = opening.recurrence_of {
        let _ = writeln!(
            body,
            "This came back after #{previous} was closed, so it is filed again rather than \
             counted against the closed issue.\n"
        );
    }

    let _ = writeln!(body, "| | |\n|---|---|");
    let _ = writeln!(body, "| Source | {} |", table_cell(opening.source.as_str()));
    let _ = writeln!(body, "| Kind | {} |", table_cell(opening.kind));
    let _ = writeln!(body, "| Occurrences | {} |", opening.occurrences);
    write_facts(&mut body, sample);
    write_detail(&mut body, &sample.detail, DETAIL_LIMIT);

    let _ = write!(
        body,
        "\n---\n\nFiled automatically by {}. Fingerprint `{}`.\n\n\
         Further occurrences are counted against this issue rather than filed again, and \
         an hourly check comments with the count while it stays open. Closing it lets the \
         next occurrence open a fresh one.",
        one_line(opening.product),
        opening.fingerprint,
    );

    body
}

/// What a still-open issue is told about the occurrences since it last heard.
#[must_use]
pub(super) fn recurrence_comment(
    since_reported: i64,
    occurrences: i64,
    last_seen_at: DateTime<Utc>,
    sample: &Sample,
) -> String {
    let mut comment = String::with_capacity(sample.detail.len().min(COMMENT_DETAIL_LIMIT) + 512);

    let times = if since_reported == 1 { "time" } else { "times" };
    let _ = writeln!(
        comment,
        "Seen {since_reported} more {times} since this issue was last updated, {occurrences} \
         in all, most recently at {}.\n",
        last_seen_at.format("%Y-%m-%d %H:%M UTC"),
    );
    let _ = writeln!(comment, "The latest occurrence:\n");
    let _ = writeln!(comment, "{}\n", inline_code(&sample.summary));

    if !sample.facts.is_empty() {
        let _ = writeln!(comment, "| | |\n|---|---|");
        write_facts(&mut comment, sample);
    }
    write_detail(&mut comment, &sample.detail, COMMENT_DETAIL_LIMIT);

    comment
}

/// One table row per fact, in the order they were given.
fn write_facts(output: &mut String, sample: &Sample) {
    for fact in &sample.facts {
        let _ = writeln!(
            output,
            "| {} | {} |",
            // A label is ours rather than a stranger's, but it is still a
            // cell, and a pipe in it would add a column.
            one_line(&fact.label).replace('|', "\\|"),
            table_cell(&fact.value),
        );
    }
}

/// The detail, fenced, when there is any.
fn write_detail(output: &mut String, detail: &str, limit: usize) {
    if detail.is_empty() {
        return;
    }

    let _ = writeln!(
        output,
        "\n```text\n{}\n```",
        // A fence inside the block would end it early and let the rest of the
        // detail render as markdown.
        clip(detail, limit).replace("```", "'''")
    );
}

/// Flattens a value onto one line.
///
/// A newline is an escape from both containers used here, and neither is
/// obvious. **A code span cannot cross a blank line**, so a message carrying
/// `\n\n` leaves the backticks unmatched and renders everything after it as
/// live markdown. **A table row is one source line**, so a single `\n` in a cell
/// ends the row. Both are reachable from a browser, whose JSON strings hold
/// newlines. Runs of them collapse, so a stack pasted into a message still
/// reads as a sentence.
fn one_line(value: &str) -> String {
    value
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wraps a value so the tracker renders it literally.
///
/// A code span stops headings, images and mentions alike, once the value
/// cannot break out of it: a backtick would end it and a blank line would
/// abandon it, so both go first.
fn inline_code(value: &str) -> String {
    format!("`{}`", one_line(value).replace('`', "'"))
}

/// One table cell: literal, and unable to add a column.
///
/// A pipe splits a cell **even inside a code span**, so escaping it is what
/// keeps the table intact; the span alone only handles the markdown.
fn table_cell(value: &str) -> String {
    inline_code(value).replace('|', "\\|")
}

/// Shortens `value` to `limit` characters, marking that it was shortened.
///
/// Counted in characters rather than bytes: the inputs come from browsers and
/// from errors, and slicing bytes would panic in the middle of one.
#[must_use]
pub fn clip(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }

    let mut clipped: String = value.chars().take(limit).collect();
    clipped.push_str("...");
    clipped
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{Opening, TITLE_LIMIT, body, clip, recurrence_comment, title};
    use crate::reporting::{Fact, Sample, Source};

    fn sample(summary: &str) -> Sample {
        Sample {
            summary: summary.to_owned(),
            facts: vec![Fact {
                label: "Path".to_owned(),
                value: "/operator/entries".to_owned(),
            }],
            detail: String::new(),
        }
    }

    fn render(sample: &Sample) -> String {
        body(&Opening {
            source: Source::Server,
            kind: "http.request.failed",
            fingerprint: "abc123",
            occurrences: 4,
            sample,
            product: "the benchmark",
            recurrence_of: None,
        })
    }

    #[test]
    fn a_title_names_the_half_of_the_application_it_came_from() {
        assert_eq!(
            title(Source::Worker, "a pass fell over"),
            "[worker] a pass fell over"
        );
    }

    #[test]
    fn a_title_is_one_line_and_clipped() {
        assert_eq!(
            title(Source::Server, "boom\nand again\r\nand again"),
            "[server] boom and again and again"
        );

        let long = title(Source::Server, &"x".repeat(TITLE_LIMIT * 2));
        assert!(long.ends_with("..."));
        assert!(long.chars().count() < TITLE_LIMIT + 16);
    }

    #[test]
    fn the_body_says_how_recurrences_are_handled() {
        let body = render(&sample("boom"));

        assert!(body.contains("Fingerprint `abc123`"));
        assert!(body.contains("| Occurrences | 4 |"));
        assert!(body.contains("Filed automatically by the benchmark."));
        assert!(body.contains("Closing it lets the next occurrence open a fresh one."));
    }

    #[test]
    fn a_recurrence_names_the_issue_it_replaces() {
        let sample = sample("boom");
        let body = body(&Opening {
            source: Source::Server,
            kind: "panic",
            fingerprint: "abc123",
            occurrences: 9,
            sample: &sample,
            product: "the benchmark",
            recurrence_of: Some(41),
        });

        assert!(body.contains("came back after #41 was closed"));
    }

    #[test]
    fn a_fact_cannot_break_out_of_the_table_or_notify_somebody() {
        let mut sample = sample("boom");
        sample.facts = vec![Fact {
            label: "Path".to_owned(),
            value: "/x | @everyone `whoami`".to_owned(),
        }];

        let body = render(&sample);

        // Escaped, not merely wrapped: a code span does not stop a pipe
        // splitting a cell.
        assert!(body.contains(r"| Path | `/x \| @everyone 'whoami'` |"));
        assert!(!body.contains("`whoami`"), "a backtick escaped the cell");
    }

    #[test]
    fn a_blank_line_cannot_abandon_the_code_span_around_a_summary() {
        let body = render(&sample("boom\n\n### @mooreslabaiv1/everyone"));
        let first_line = body.lines().next().expect("the body has a first line");

        assert_eq!(first_line, "`boom ### @mooreslabaiv1/everyone`");
        assert!(
            !body.contains("\n### "),
            "a heading reached the body: {body}"
        );
    }

    #[test]
    fn a_newline_in_a_fact_cannot_end_the_table_row() {
        let mut sample = sample("boom");
        sample.facts = vec![Fact {
            label: "Page".to_owned(),
            value: "/samples\n\n# owned".to_owned(),
        }];

        let body = render(&sample);

        assert!(body.contains("| Page | `/samples # owned` |"));
    }

    #[test]
    fn a_fence_inside_the_detail_cannot_end_the_block_early() {
        let mut sample = sample("boom");
        sample.detail = "at foo\n```\n# heading".to_owned();

        let body = render(&sample);

        assert_eq!(
            body.matches("```").count(),
            2,
            "the detail block must open and close exactly once"
        );
    }

    #[test]
    fn a_recurrence_comment_counts_and_quotes_the_latest() {
        let mut latest = sample("boom again");
        latest.detail = "x".repeat(10_000);
        let seen = Utc
            .with_ymd_and_hms(2026, 10, 6, 9, 30, 0)
            .single()
            .expect("a valid instant");

        let comment = recurrence_comment(3, 12, seen, &latest);

        assert!(
            comment.starts_with("Seen 3 more times since this issue was last updated, 12 in all")
        );
        assert!(comment.contains("2026-10-06 09:30 UTC"));
        assert!(comment.contains("`boom again`"));
        assert!(
            comment.len() < 3_000,
            "a comment quotes a clipped detail, not the whole of it"
        );
    }

    #[test]
    fn one_occurrence_reads_as_one_time() {
        let comment = recurrence_comment(1, 2, Utc::now(), &sample("boom"));

        assert!(comment.starts_with("Seen 1 more time since"));
    }

    #[test]
    fn clip_counts_characters_rather_than_bytes() {
        assert_eq!(clip("ab", 5), "ab");
        assert_eq!(clip("😀😀😀", 2), "😀😀...");
    }
}
