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
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("password_hash", &"...")
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
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
    /// When the account was created.
    pub created_at: DateTime<Utc>,
}

impl From<&User> for UserResponse {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
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
