//! HTTP building blocks shared by framework and application handlers.
//!
//! [`ApiError`] is the one error shape handlers return: a status code plus a
//! user-safe JSON body of `{"message": "..."}`. Internal detail never reaches
//! the response body; log it with `tracing` at the point of failure and return
//! [`ApiError::internal`]. The body shape holds for every status, including
//! the `429` that [`ApiError::too_many_requests`] adds a `Retry-After` header
//! to.
//!
//! A refusal a client has to *act* on rather than merely show carries a
//! `code` beside the message ([`ApiError::with_code`]), because branching on
//! prose is branching on a string that translation is free to change.
//!
//! [`ListParams`] and [`Pagination`] carry the locked list-endpoint
//! conventions from the repository's `docs/api.md`, so every scaffolded list
//! endpoint pages, sorts, and answers in exactly one shape.

use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A user-safe HTTP error: a status code and a JSON `message` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    /// A stable identifier for the refusal, rendered as `code` in the body.
    code: Option<&'static str>,
    /// How long the caller should wait, rendered as a `Retry-After` header.
    retry_after: Option<Duration>,
}

impl ApiError {
    /// A `400 Bad Request` for input that fails validation.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// A `401 Unauthorized` for missing or bad credentials.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    /// A `403 Forbidden` for authenticated users lacking permission.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// A `404 Not Found` that does not reveal whether the resource exists.
    ///
    /// Tenancy guards answer non-members with this, so probing ids leaks
    /// nothing: absent and forbidden look identical.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not found.")
    }

    /// A `409 Conflict` for requests that collide with existing state.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// A `429 Too Many Requests` telling the caller how long to wait.
    ///
    /// The response carries a `Retry-After` header alongside the usual JSON
    /// body. The message names the wait and nothing else: which budget a
    /// caller exhausted, and whether the account or address involved exists,
    /// stay invisible, so a limit can never become a probe.
    ///
    /// See [`crate::rate_limit`] for the budgets that produce this.
    #[must_use]
    pub fn too_many_requests(retry_after: Duration) -> Self {
        let seconds = retry_after_seconds(retry_after);
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("Too many requests. Try again in {seconds} seconds."),
            code: None,
            retry_after: Some(retry_after),
        }
    }

    /// A `500 Internal Server Error` with a deliberately generic body.
    ///
    /// Log the actual failure with `tracing` before returning this; the
    /// response body never carries internal detail. The response's
    /// `x-request-id` header is what ties a user's report to that log line.
    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong on our side.",
        )
    }

    /// A `503 Service Unavailable` for work this server cannot do right now.
    ///
    /// The honest answer when the server itself ran out of time or a
    /// dependency is unreachable, both of which a caller may retry. Keep the
    /// message free of which dependency failed, for the same reason
    /// [`ApiError::internal`]'s body is generic.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// Attaches a stable identifier for the refusal to the error.
    ///
    /// Two errors of the same status can demand different things of a client:
    /// a disabled account is a dead end, while an account owing a password
    /// change has one screen to go to. The code is what a client branches on,
    /// so it never has to match on a message that translation may rewrite.
    /// Codes are `snake_case` and permanent; see `docs/api.md`.
    #[must_use]
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    /// Returns the HTTP status code.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the refusal's stable identifier, when it carries one.
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        self.code
    }

    /// Returns the user-safe message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns how long the caller should wait, when the error says so.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
            retry_after: None,
        }
    }
}

/// Renders a wait as the whole seconds a `Retry-After` header carries.
///
/// Rounded up, and never below one: a header of `0` invites an immediate
/// retry that the limiter would reject again.
fn retry_after_seconds(retry_after: Duration) -> u64 {
    let rounded_up = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
    rounded_up.max(1)
}

impl From<diesel::result::Error> for ApiError {
    /// Renders a failed query as a `500`, logging the cause on the way.
    ///
    /// This exists so a handler can run its writes inside a Diesel
    /// transaction, whose error type must be `From<diesel::result::Error>`,
    /// and still answer with the framework's one error shape. Generated
    /// handlers do exactly that: the record, its associations, and the
    /// outgoing webhook it emits all commit together or not at all.
    ///
    /// It logs rather than dropping the cause, because
    /// [`ApiError::internal`]'s body is deliberately generic and the
    /// response's `x-request-id` is the only thread back to this line.
    fn from(source: diesel::result::Error) -> Self {
        tracing::error!(
            error.message = %source,
            "a database query failed: {{error.message}}",
        );
        Self::internal()
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
    /// Absent from the body entirely when the refusal names no code, so the
    /// shape every other error answers with stays exactly `{"message": ...}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            message: &self.message,
            code: self.code,
        });

        match self.retry_after {
            None => (self.status, body).into_response(),
            Some(retry_after) => {
                let seconds = retry_after_seconds(retry_after);
                (self.status, [(RETRY_AFTER, seconds.to_string())], body).into_response()
            }
        }
    }
}

/// The list-endpoint query conventions: `?page=`, `?limit=`, `?sort=`.
///
/// `page` is 1-based and defaults to 1; `limit` defaults to
/// [`ListParams::DEFAULT_LIMIT`] and caps at [`ListParams::MAX_LIMIT`]. Values
/// that are out of range, or that do not parse at all, fall back to the
/// convention rather than failing the request: a paging bug in a client
/// degrades into a valid page instead of a 400.
///
/// Handlers take it as a query extractor, alongside their own filter struct:
///
/// ```ignore
/// async fn list(
///     Query(params): Query<ListParams>,
///     Query(filters): Query<ProjectFilters>,
/// ) -> Result<impl IntoResponse, ApiError> {
///     let (field, descending) = params.sort(&["name", "created_at"], "created_at");
///     ...
/// }
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ListParams {
    page: Option<String>,
    limit: Option<String>,
    sort: Option<String>,
}

impl ListParams {
    /// Page size when `?limit=` is absent.
    pub const DEFAULT_LIMIT: i64 = 25;
    /// Largest page size a caller may ask for.
    pub const MAX_LIMIT: i64 = 100;

    /// The requested page, 1-based and at least 1.
    #[must_use]
    pub fn page(&self) -> i64 {
        self.page
            .as_deref()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(1)
            .max(1)
    }

    /// The requested page size, clamped to `1..=MAX_LIMIT`.
    #[must_use]
    pub fn limit(&self) -> i64 {
        self.limit
            .as_deref()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT)
    }

    /// The number of rows to skip for the requested page.
    #[must_use]
    pub fn offset(&self) -> i64 {
        (self.page() - 1) * self.limit()
    }

    /// Resolves `?sort=` against the model's whitelist.
    ///
    /// `?sort=name` sorts ascending, `?sort=-name` descending. An absent or
    /// unwhitelisted field falls back to `default`, descending, which keeps
    /// newest-first the default everywhere.
    #[must_use]
    pub fn sort<'field>(
        &self,
        sortable: &[&'field str],
        default: &'field str,
    ) -> (&'field str, bool) {
        let requested = self
            .sort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(requested) = requested else {
            return (default, true);
        };

        let (field, descending) = requested
            .strip_prefix('-')
            .map_or((requested, false), |rest| (rest, true));

        sortable
            .iter()
            .copied()
            .find(|candidate| *candidate == field)
            .map_or((default, true), |matched| (matched, descending))
    }
}

/// The pagination object every list response carries alongside its records.
///
/// It is part of the public API contract as well as the account one: a
/// scaffolded model's `/api/v1` list envelope carries it, so it registers with
/// utoipa and is documented once for every endpoint that pages.
// The doc comment above is for whoever reads this code; the description below
// is what an API consumer reads in the published document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[schema(description = "The page a list response describes, beside its records.")]
pub struct Pagination {
    /// The page these records came from, 1-based.
    pub page: i64,
    /// The page size in effect.
    pub limit: i64,
    /// How many records match the query in total.
    pub total_items: i64,
    /// How many pages the total spans; zero when nothing matches.
    pub total_pages: i64,
}

impl Pagination {
    /// Describes the page `params` asked for, given the total match count.
    #[must_use]
    pub fn new(params: &ListParams, total_items: i64) -> Self {
        let limit = params.limit();
        Self {
            page: params.page(),
            limit,
            total_items,
            // Ceiling division; both operands are non-negative and limit >= 1.
            total_pages: (total_items + limit - 1) / limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::extract::Query;
    use axum::http::header::RETRY_AFTER;
    use axum::http::{StatusCode, Uri};
    use axum::response::IntoResponse;

    use super::{ApiError, ErrorBody, ListParams, Pagination};

    /// Parses through the real extractor, so the serde attributes are covered.
    fn params(query: &str) -> ListParams {
        let uri: Uri = format!("/list?{query}").parse().expect("uri must parse");
        let Query(params) = Query::try_from_uri(&uri).expect("query must deserialize");
        params
    }

    #[test]
    fn paging_defaults_to_the_first_page_of_twenty_five() {
        let defaults = params("");
        assert_eq!(defaults.page(), 1);
        assert_eq!(defaults.limit(), ListParams::DEFAULT_LIMIT);
        assert_eq!(defaults.offset(), 0);

        let second = params("page=2&limit=10");
        assert_eq!(second.page(), 2);
        assert_eq!(second.limit(), 10);
        assert_eq!(second.offset(), 10);
    }

    #[test]
    fn out_of_range_and_unparsable_values_clamp() {
        assert_eq!(params("page=0").page(), 1);
        assert_eq!(params("page=-4").page(), 1);
        assert_eq!(params("page=soon").page(), 1);
        assert_eq!(params("limit=1000").limit(), ListParams::MAX_LIMIT);
        assert_eq!(params("limit=0").limit(), 1);
        assert_eq!(params("limit=many").limit(), ListParams::DEFAULT_LIMIT);
    }

    #[test]
    fn sorting_honors_the_whitelist_and_falls_back_to_newest_first() {
        let sortable = ["name", "created_at"];

        assert_eq!(
            params("sort=name").sort(&sortable, "created_at"),
            ("name", false)
        );
        assert_eq!(
            params("sort=-name").sort(&sortable, "created_at"),
            ("name", true)
        );
        assert_eq!(
            params("sort=password_hash").sort(&sortable, "created_at"),
            ("created_at", true),
        );
        assert_eq!(
            params("").sort(&sortable, "created_at"),
            ("created_at", true)
        );
    }

    #[test]
    fn pagination_counts_pages_by_ceiling() {
        let page = Pagination::new(&params("page=2&limit=25"), 61);
        assert_eq!(page.total_pages, 3);
        assert_eq!(page.page, 2);

        assert_eq!(Pagination::new(&params(""), 0).total_pages, 0);
        assert_eq!(Pagination::new(&params("limit=25"), 25).total_pages, 1);
    }

    #[test]
    fn constructors_map_to_the_right_status_codes() {
        assert_eq!(
            ApiError::validation("bad").status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::unauthorized("no").status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiError::conflict("taken").status(), StatusCode::CONFLICT);
        assert_eq!(
            ApiError::internal().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::unavailable("not ready").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn a_failed_query_becomes_a_generic_internal_error() {
        let error = ApiError::from(diesel::result::Error::NotFound);

        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.message(), ApiError::internal().message());
        assert!(
            !error.message().contains("NotFound"),
            "the cause belongs in the log, not the body: {}",
            error.message(),
        );
    }

    #[test]
    fn internal_errors_carry_no_detail() {
        let error = ApiError::internal();
        assert!(!error.message().is_empty());
        assert!(!error.message().contains("error"), "keep the body generic");
    }

    #[test]
    fn only_errors_that_name_a_code_carry_one_in_the_body() {
        let plain = serde_json::to_string(&ErrorBody {
            message: ApiError::forbidden("no").message(),
            code: None,
        })
        .expect("serialization must succeed");
        assert_eq!(plain, r#"{"message":"no"}"#);

        let coded = ApiError::forbidden("no").with_code("account_disabled");
        assert_eq!(coded.code(), Some("account_disabled"));
        let rendered = serde_json::to_string(&ErrorBody {
            message: coded.message(),
            code: coded.code(),
        })
        .expect("serialization must succeed");
        assert_eq!(rendered, r#"{"message":"no","code":"account_disabled"}"#);
    }

    #[test]
    fn responses_carry_the_status() {
        let response = ApiError::conflict("taken").into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(response.headers().get(RETRY_AFTER).is_none());
    }

    #[test]
    fn rate_limited_responses_carry_a_retry_after_header_in_whole_seconds() {
        let error = ApiError::too_many_requests(Duration::from_millis(6_200));
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error.retry_after(), Some(Duration::from_millis(6_200)));
        assert!(error.message().contains('7'), "got: {}", error.message());

        let response = error.into_response();
        assert_eq!(
            response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("7"),
        );
    }

    #[test]
    fn a_sub_second_wait_still_asks_for_one_second() {
        let response = ApiError::too_many_requests(Duration::from_millis(1)).into_response();
        assert_eq!(
            response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1"),
        );
    }
}
