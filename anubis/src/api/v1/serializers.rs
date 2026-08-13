//! The frozen response shapes of API v1.
//!
//! These structs are the public contract: once v1 has users, fields are only
//! added (never renamed or removed), and breaking changes mint a v2 module
//! with its own serializers.

use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::tenancy::Team;

/// A team, as serialized by API v1.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TeamV1 {
    /// Primary key.
    pub id: Uuid,
    /// The organization the team belongs to.
    pub organization_id: Uuid,
    /// Display name.
    pub name: String,
    /// When the team was created.
    pub created_at: DateTime<Utc>,
}

impl From<&Team> for TeamV1 {
    fn from(team: &Team) -> Self {
        Self {
            id: team.id,
            organization_id: team.organization_id,
            name: team.name.clone(),
            created_at: team.created_at,
        }
    }
}

/// Envelope for a single team.
#[derive(Serialize, ToSchema)]
pub(crate) struct TeamEnvelopeV1 {
    pub team: TeamV1,
}

/// The error shape every v1 endpoint answers with.
///
/// Application handlers name it in their own `#[utoipa::path]` responses, so
/// every endpoint in the merged document documents failure the same way.
// The doc comment above is for whoever reads this code; the description below
// is what an API consumer reads in the published document.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ErrorV1, description = "The error shape every v1 endpoint answers with.")]
pub struct ErrorV1 {
    /// A user-safe description of what went wrong.
    pub message: String,
}
