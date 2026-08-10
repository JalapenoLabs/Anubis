//! The `PeripheralNotion` model: the far side of the join template.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Serialize;
use uuid::Uuid;

use crate::schema::peripheral_notions;

/// The key this model is granted permissions under in `config/roles.yml`.
pub const MODEL: &str = "PeripheralNotion";

/// A peripheral notion, owned by a team.
///
/// Deliberately plain: it exists so the join template has a second team-owned
/// model to link, and it carries only the columns an association needs, an id
/// to link by and a name to label the option with.
#[derive(Debug, Clone, Serialize, Queryable, Selectable, Identifiable)]
#[diesel(table_name = peripheral_notions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PeripheralNotion {
    /// Primary key.
    pub id: Uuid,
    /// The owning team; the whole ownership chain hangs off this column.
    pub team_id: Uuid,
    /// Display name.
    pub name: String,
    /// When the peripheral notion was created.
    pub created_at: DateTime<Utc>,
    /// When the peripheral notion was last updated, kept by the database trigger.
    pub updated_at: DateTime<Utc>,
}

/// The insertable shape; the database fills the id and both timestamps.
#[derive(Debug, Insertable)]
#[diesel(table_name = peripheral_notions)]
pub struct NewPeripheralNotion<'a> {
    /// The owning team, taken from the route, never from the request body.
    pub team_id: Uuid,
    /// Display name.
    pub name: &'a str,
}
