//! The error shape OAuth clients read: `{"error": ..., "error_description": ...}`.
//!
//! The token, revocation, and registration endpoints answer machines that
//! branch on the `error` code RFC 6749 section 5.2 and RFC 7591 section 3.2.2
//! define, not on the framework's `{"message": ...}` shape, so they speak this
//! one. Every response carries `Cache-Control: no-store`, which RFC 6749
//! requires of anything a token might pass through.

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A refusal in RFC 6749's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OauthError {
    status: StatusCode,
    code: &'static str,
    description: String,
}

impl OauthError {
    /// `invalid_request`: something required is missing or malformed.
    pub(crate) fn invalid_request(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    }

    /// `invalid_grant`: the code or refresh token is unknown, spent, expired,
    /// or belongs to somebody else. One code for all of them, so a probe learns
    /// nothing about which.
    pub(crate) fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_grant", description)
    }

    /// `invalid_scope`: a refresh asked for more than the grant holds.
    pub(crate) fn invalid_scope(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_scope", description)
    }

    /// `invalid_target`: the RFC 8707 resource is not this server's.
    pub(crate) fn invalid_target(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_target", description)
    }

    /// `unsupported_grant_type`.
    pub(crate) fn unsupported_grant_type() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Only authorization_code and refresh_token are supported.",
        )
    }

    /// `invalid_client_metadata`, RFC 7591's refusal of a registration.
    pub(crate) fn invalid_client_metadata(description: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            description,
        )
    }

    /// `invalid_redirect_uri`, RFC 7591's refusal of a redirect URI.
    pub(crate) fn invalid_redirect_uri(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_redirect_uri", description)
    }

    /// `temporarily_unavailable`: the server will not take this right now.
    pub(crate) fn temporarily_unavailable(description: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            description,
        )
    }

    /// `server_error`, with a deliberately generic description.
    ///
    /// Log the cause before returning this, exactly as with
    /// [`crate::http::ApiError::internal`].
    pub(crate) fn server_error() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Something went wrong on our side.",
        )
    }

    fn new(status: StatusCode, code: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            code,
            description: description.into(),
        }
    }
}

impl From<diesel::result::Error> for OauthError {
    /// Renders a failed query as `server_error`, logging the cause.
    fn from(source: diesel::result::Error) -> Self {
        tracing::error!(
            error.message = %source,
            "an authorization server query failed: {{error.message}}",
        );
        Self::server_error()
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    error_description: &'a str,
}

impl IntoResponse for OauthError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(CACHE_CONTROL, "no-store")],
            Json(ErrorBody {
                error: self.code,
                error_description: &self.description,
            }),
        )
            .into_response()
    }
}
