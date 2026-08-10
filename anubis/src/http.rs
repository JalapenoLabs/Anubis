//! HTTP building blocks shared by framework and application handlers.
//!
//! [`ApiError`] is the one error shape handlers return: a status code plus a
//! user-safe JSON body of `{"message": "..."}`. Internal detail never reaches
//! the response body; log it with `tracing` at the point of failure and return
//! [`ApiError::internal`].

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::ApiError;

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
