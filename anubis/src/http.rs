//! HTTP building blocks shared by framework and application handlers.
//!
//! [`ApiError`] is the one error shape handlers return: a status code plus a
//! user-safe JSON body of `{"message": "..."}`. Internal detail never reaches
//! the response body; log it with `tracing` at the point of failure and return
//! [`ApiError::internal`].
//!
//! [`ListParams`] and [`Pagination`] carry the locked list-endpoint
//! conventions from the repository's `docs/api.md`, so every scaffolded list
//! endpoint pages, sorts, and answers in exactly one shape.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// A user-safe HTTP error: a status code and a JSON `message` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// A `400 Bad Request` for input that fails validation.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// A `401 Unauthorized` for missing or bad credentials.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    /// A `403 Forbidden` for authenticated users lacking permission.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    /// A `404 Not Found` that does not reveal whether the resource exists.
    ///
    /// Tenancy guards answer non-members with this, so probing ids leaks
    /// nothing: absent and forbidden look identical.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "Not found.".to_owned(),
        }
    }

    /// A `409 Conflict` for requests that collide with existing state.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    /// A `500 Internal Server Error` with a deliberately generic body.
    ///
    /// Log the actual failure with `tracing` before returning this; the
    /// response body never carries internal detail.
    #[must_use]
    pub fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Something went wrong on our side.".to_owned(),
        }
    }

    /// Returns the HTTP status code.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the user-safe message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            message: &self.message,
        });
        (self.status, body).into_response()
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    use axum::extract::Query;
    use axum::http::{StatusCode, Uri};
    use axum::response::IntoResponse;

    use super::{ApiError, ListParams, Pagination};

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
    }

    #[test]
    fn internal_errors_carry_no_detail() {
        let error = ApiError::internal();
        assert!(!error.message().is_empty());
        assert!(!error.message().contains("error"), "keep the body generic");
    }

    #[test]
    fn responses_carry_the_status() {
        let response = ApiError::conflict("taken").into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
