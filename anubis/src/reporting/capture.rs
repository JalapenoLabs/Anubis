//! Noticing failures without every call site having to report them.
//!
//! Three nets, each catching what the others cannot:
//!
//! | Net | Catches |
//! |---|---|
//! | [`ErrorLayer`] | Every `ERROR` event. The framework's convention is to log the cause where a failure happens and answer a generic `500`, so this is where most causes are. |
//! | [`install_panic_hook`] | Every panic, in a handler, a job, or a spawned task, with its location and a backtrace. |
//! | [`instrument`] | A `500` that logged no cause, and a handler that panicked, which it answers with a `500` rather than a dropped connection. |
//!
//! A `500` whose cause was logged is reported once, by the log line, and not
//! again by [`instrument`]: the layer remembers which request ids logged an
//! error, and the middleware asks before reporting.

use std::any::Any;
use std::backtrace::Backtrace;
use std::fmt::{Debug, Write as _};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::catch_panic::CatchPanicLayer;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::{FileErrorReport, Report, Reporter};
use crate::http::ApiError;
use crate::jobs::Job;
use crate::server::RequestId;

/// The target every line this module and its siblings log under.
///
/// Ignored by [`ErrorLayer`], because a line about reporting that became a
/// report would be the loop the module docs describe.
const OWN_TARGET: &str = "anubis::reporting";

/// The field the job worker names a job's kind in.
const JOB_KIND_FIELD: &str = "job.kind";

/// The span field the request span carries its id in.
const REQUEST_ID_FIELD: &str = "request.id";

/// Span fields worth a row in an issue, and the label each gets.
///
/// The request span's, which say which request failed. Anything else a span
/// carries is the application's business and stays in the logs.
const SPAN_FACTS: [(&str, &str); 3] = [
    ("http.request.method", "Method"),
    ("url.path", "Path"),
    (REQUEST_ID_FIELD, "Request id"),
];

/// Reports every `ERROR` event, with the request it happened in.
///
/// Add it to the subscriber beside the formatter; see
/// [`crate::telemetry::init_with_reporting`]. An event's **message template**
/// is the stable part of it, and the framework writes templates with escaped
/// placeholders (`"failed: {{error.message}}"`), so the template, plus the
/// event's `error.message` field normalized, is the signature: one call site
/// failing for one reason is one fault, however many rows it was about.
#[derive(Debug, Clone)]
pub struct ErrorLayer {
    reporter: Reporter,
}

impl ErrorLayer {
    /// A layer reporting through `reporter`.
    #[must_use]
    pub fn new(reporter: Reporter) -> Self {
        Self { reporter }
    }
}

/// An event's or a span's fields, as text.
#[derive(Debug, Default)]
struct Fields(Vec<(&'static str, String)>);

impl Fields {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(field, _value)| *field == name)
            .map(|(_field, value)| value.as_str())
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name(), value.to_owned()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.push((field.name(), format!("{value:?}")));
    }
}

#[expect(
    clippy::renamed_function_params,
    reason = "the trait abbreviates its parameter names, and this codebase spells names out"
)]
impl<S> tracing_subscriber::Layer<S> for ErrorLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(fields) = span.extensions_mut().get_mut::<Fields>()
        {
            values.record(fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let metadata = event.metadata();
        if *metadata.level() != Level::ERROR || metadata.target().starts_with(OWN_TARGET) {
            return;
        }

        let mut fields = Fields::default();
        event.record(&mut fields);
        // The job worker's own lines about a filing job that failed. Reported,
        // they would file issues about failing to file issues.
        if fields.get(JOB_KIND_FIELD) == Some(FileErrorReport::KIND) {
            return;
        }

        let template = fields.get("message").unwrap_or_default().to_owned();
        // `error.message` is the convention; a bare `%error` field is the
        // shorthand some call sites use for the same thing.
        let cause = fields
            .get("error.message")
            .or_else(|| fields.get("error"))
            .unwrap_or_default()
            .to_owned();

        let mut report = Report::new(
            self.reporter.source(),
            metadata.target(),
            render(&template, &fields),
        )
        .signature(format!("{template}\n{cause}"))
        .fact("Logged by", metadata.target());
        if let (Some(file), Some(line)) = (metadata.file(), metadata.line()) {
            report = report.fact("Location", format!("{file}:{line}"));
        }
        for (name, value) in &fields.0 {
            if *name != "message" {
                report = report.fact(*name, value);
            }
        }

        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                let Some(span_fields) = extensions.get::<Fields>() else {
                    continue;
                };
                for (name, label) in SPAN_FACTS {
                    if let Some(value) = span_fields.get(name) {
                        report = report.fact(label, value);
                    }
                }
                if let Some(request_id) = span_fields.get(REQUEST_ID_FIELD) {
                    self.reporter.flag_request(request_id);
                }
            }
        }

        self.reporter.report(report);
    }
}

/// Fills a message template's `{field}` placeholders from the event's fields.
///
/// A placeholder naming no field is left as written, which is how a literal
/// brace in a message survives.
fn render(template: &str, fields: &Fields) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        rendered.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            rendered.push_str(&rest[open..]);
            return rendered;
        };

        let name = &after[..close];
        match fields.get(name) {
            Some(value) => rendered.push_str(value),
            None => {
                let _ = write!(rendered, "{{{name}}}");
            }
        }
        rest = &after[close + 1..];
    }

    rendered.push_str(rest);
    rendered
}

/// Reports every panic in this process, then lets the previous hook run.
///
/// Process-wide, and meant to be installed once at boot. The report carries
/// where it panicked, what it said, and a backtrace captured whatever
/// `RUST_BACKTRACE` says, because a panic is rare enough to afford one and
/// useless without one. The previous hook still runs, so the panic still
/// reaches stderr the way it always did.
pub fn install_panic_hook(reporter: Reporter) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info.payload());
        let location = info.location().map_or_else(
            || "an unknown location".to_owned(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );
        let file = info.location().map_or("", |location| location.file());
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();

        reporter.report(
            Report::new(
                reporter.source(),
                "panic",
                format!("panicked at {location}: {message}"),
            )
            // The file and the message, not the line: a line moves with every
            // edit above it, and the digits would be masked anyway.
            .signature(format!("{file}\n{message}"))
            .fact("Location", &location)
            .fact("Thread", thread)
            .detail(Backtrace::force_capture().to_string()),
        );

        previous(info);
    }));
}

/// What a panic said, whatever it was given.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "a panic with no message".to_owned()
}

/// Marks a `500` that answered a panic, which the panic hook already reported.
#[derive(Debug, Clone, Copy)]
struct Panicked;

/// Answers a panicking handler with a `500`, and reports `500`s nobody logged.
///
/// Apply it to the application's router, inside the framework's request-id
/// layer, which is where [`crate::server::serve`] puts every router:
///
/// ```ignore
/// let app = anubis::reporting::instrument(app, reporter.clone());
/// anubis::server::serve(app, pool, &config).await?;
/// ```
///
/// Without it a panic in a handler drops the connection, and the caller sees a
/// network error rather than the framework's error shape. A `500` whose cause
/// was logged at `ERROR` was reported by that line; one that was not, such as
/// `ApiError::internal()` returned from an unexpected state, is reported here,
/// under the route that answered it rather than the path, so a route over many
/// ids is one fault.
pub fn instrument(router: Router, reporter: Reporter) -> Router {
    router
        .layer(CatchPanicLayer::custom(answer_panic))
        .layer(axum::middleware::from_fn_with_state(
            reporter,
            report_unexplained,
        ))
}

/// The `500` a panicking handler answers with, in the framework's error shape.
fn answer_panic(_payload: Box<dyn Any + Send + 'static>) -> Response {
    let mut response = ApiError::internal().into_response();
    response.extensions_mut().insert(Panicked);
    response
}

/// Reports a `500` that neither a logged error nor a panic explained.
async fn report_unexplained(
    State(reporter): State<Reporter>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let route = request.extensions().get::<MatchedPath>().map_or_else(
        || request.uri().path().to_owned(),
        |matched| matched.as_str().to_owned(),
    );
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.as_str().to_owned());

    let response = next.run(request).await;

    // Forgotten whatever the status, so the memory only ever holds requests
    // still in flight.
    let logged = request_id
        .as_deref()
        .is_some_and(|id| reporter.take_flag(id));
    let panicked = response.extensions().get::<Panicked>().is_some();

    if response.status() == StatusCode::INTERNAL_SERVER_ERROR && !logged && !panicked {
        reporter.report(
            Report::new(
                reporter.source(),
                "http.request.failed",
                format!("{method} {route} answered 500 without logging why"),
            )
            .signature(format!("{method} {route}"))
            .fact("Method", method.as_str())
            .fact("Route", &route)
            .fact("Request id", request_id.as_deref().unwrap_or("none")),
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use tracing::Level;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{ErrorLayer, Fields, panic_message, render};
    use crate::jobs::Job;
    use crate::reporting::{FileErrorReport, Reporter, Source};

    #[test]
    fn an_error_event_is_reported_with_the_request_it_happened_in() {
        let (reporter, mut inbox) = Reporter::new(Source::Server);
        let subscriber = tracing_subscriber::registry().with(ErrorLayer::new(reporter.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "http.request",
                http.request.method = "GET",
                url.path = "/operator/entries",
                request.id = "request-1",
            );
            let _entered = span.enter();

            // Not this module's own target, which the layer ignores.
            tracing::event!(
                target: "benchmark::grading::routes",
                Level::ERROR,
                error.message = "connection refused",
                "the entries could not be read: {{error.message}}",
            );
            tracing::error!(target: "anubis::reporting::filing", "a line about reporting");
            tracing::event!(
                target: "anubis::jobs::worker",
                Level::ERROR,
                job.kind = FileErrorReport::KIND,
                "a filing job died",
            );
            tracing::warn!(target: "benchmark::grading::routes", "only a warning");
        });

        let report = inbox
            .receiver
            .try_recv()
            .expect("the error must be reported");
        assert_eq!(report.kind(), "benchmark::grading::routes");
        assert_eq!(
            report.summary(),
            "the entries could not be read: connection refused"
        );
        let facts: Vec<(&str, &str)> = report
            .facts()
            .iter()
            .map(|fact| (fact.label.as_str(), fact.value.as_str()))
            .collect();
        assert!(facts.contains(&("Method", "GET")), "{facts:?}");
        assert!(facts.contains(&("Path", "/operator/entries")), "{facts:?}");
        assert!(facts.contains(&("Request id", "request-1")), "{facts:?}");

        assert!(
            inbox.receiver.try_recv().is_err(),
            "reporting's own lines, a filing job's death and a warning are not reports",
        );
        assert!(
            reporter.take_flag("request-1"),
            "the request is remembered as explained, so its 500 is not reported twice",
        );
    }

    fn fields(pairs: &[(&'static str, &str)]) -> Fields {
        Fields(
            pairs
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn a_template_is_filled_from_the_events_fields() {
        let fields = fields(&[
            ("error.message", "connection refused"),
            ("rating.job", "42"),
        ]);

        assert_eq!(
            render(
                "the rating for {rating.job} failed: {error.message}",
                &fields
            ),
            "the rating for 42 failed: connection refused"
        );
    }

    #[test]
    fn a_placeholder_naming_no_field_is_left_as_written() {
        let fields = fields(&[]);

        assert_eq!(
            render("reading {id} from {", &fields),
            "reading {id} from {"
        );
    }

    #[test]
    fn a_panic_says_what_it_was_given() {
        assert_eq!(panic_message(&"boom"), "boom");
        assert_eq!(panic_message(&String::from("bang")), "bang");
        assert_eq!(panic_message(&42_u8), "a panic with no message");
    }
}
