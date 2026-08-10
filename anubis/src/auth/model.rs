//! The framework-owned user account model.

use std::fmt::{self, Formatter};

use chrono::{DateTime, Utc};
use diesel::prelude::{Insertable, Queryable, Selectable};
use serde::Serialize;
use uuid::Uuid;

use crate::schema::users;

/// A registered user account.
///
/// The password hash stays crate-private and is redacted from `Debug`; expose
/// users over HTTP via [`UserResponse`].
#[derive(Clone, Queryable, Selectable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    /// Primary key.
    pub id: Uuid,
    /// Normalized (trimmed, lowercased) email address.
    pub email: String,
    /// The argon2id PHC hash of the user's password.
    pub(crate) password_hash: String,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the account was last updated.
    pub updated_at: DateTime<Utc>,
    /// When the user confirmed control of their email address, if ever.
    pub email_verified_at: Option<DateTime<Utc>>,
    /// Given name, when the user has provided one.
    pub first_name: Option<String>,
    /// Family name, when the user has provided one.
    pub last_name: Option<String>,
    /// IANA time zone name; defaults to UTC.
    pub time_zone: String,
    /// BCP 47 locale tag; defaults to en-US.
    pub locale: String,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("password_hash", &"...")
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("email_verified_at", &self.email_verified_at)
            .field("first_name", &self.first_name)
            .field("last_name", &self.last_name)
            .field("time_zone", &self.time_zone)
            .field("locale", &self.locale)
            .finish()
    }
}

/// Insertable row for creating a user account.
#[derive(Insertable)]
#[diesel(table_name = users)]
pub(crate) struct NewUser<'a> {
    pub email: &'a str,
    pub password_hash: &'a str,
}

/// The user shape serialized in HTTP responses.
#[derive(Debug, Clone, Serialize)]
pub struct UserResponse {
    /// Primary key.
    pub id: Uuid,
    /// Normalized email address.
    pub email: String,
    /// Whether the user has confirmed control of their email address.
    pub email_verified: bool,
    /// Given name, when provided.
    pub first_name: Option<String>,
    /// Family name, when provided.
    pub last_name: Option<String>,
    /// IANA time zone name.
    pub time_zone: String,
    /// BCP 47 locale tag.
    pub locale: String,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
}

impl From<&User> for UserResponse {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            email_verified: user.email_verified_at.is_some(),
            first_name: user.first_name.clone(),
            last_name: user.last_name.clone(),
            time_zone: user.time_zone.clone(),
            locale: user.locale.clone(),
            created_at: user.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{User, UserResponse};

    fn sample_user() -> User {
        User {
            id: Uuid::new_v4(),
            email: "sample@example.com".to_owned(),
            password_hash: "$argon2id$super-secret-hash".to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            email_verified_at: None,
            first_name: Some("Sample".to_owned()),
            last_name: None,
            time_zone: "UTC".to_owned(),
            locale: "en-US".to_owned(),
        }
    }

    #[test]
    fn debug_output_never_leaks_the_password_hash() {
        let user = sample_user();
        let rendered = format!("{user:?}");

        assert!(rendered.contains("sample@example.com"), "got: {rendered}");
        assert!(!rendered.contains("super-secret-hash"), "got: {rendered}");
    }

    #[test]
    fn responses_never_carry_the_password_hash() {
        let user = sample_user();
        let response = UserResponse::from(&user);

        let rendered = serde_json::to_string(&response).expect("serialization must succeed");
        assert!(rendered.contains("sample@example.com"), "got: {rendered}");
        assert!(!rendered.contains("super-secret-hash"), "got: {rendered}");
        assert!(!rendered.contains("password"), "got: {rendered}");
    }
}
