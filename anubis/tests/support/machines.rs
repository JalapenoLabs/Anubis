//! A kind of record the organization owns directly, owned by these suites.
//!
//! The framework cannot see an application's tables, so the suites that prove
//! resource grants carry a small application of their own: a `machines` table,
//! the [`ResourceKind`] describing it, and the routes an application writes
//! over it on every surface it serves (session routes, the `/api/v1` token
//! surface, an MCP tool, and a realtime stream). Every one of them asks the
//! framework's resolver and decides nothing itself, which is what the suites
//! hold it to.

use std::sync::Arc;

use anubis::api::v1::ApiCaller;
use anubis::auth::CurrentUser;
use anubis::db::DbPool;
use anubis::http::{ApiError, FieldOption};
use anubis::mcp::{Tool, ToolCall, ToolError};
use anubis::realtime::{ChannelName, Channels, EventStream, Subscriber};
use anubis::roles::{Action, RoleSet};
use anubis::tenancy::{
    OrganizationResource, ResourceKind, ResourceKindFuture, ResourceScope, forget_resource,
    resolve_resource_access, resolve_resource_scope, team_reaches_resource, team_resource_scope,
};
use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Harness, TestDatabase, register, send};

/// The model a machine is, as the suites' `roles.yml` names it.
pub const KIND: &str = "Machine";

diesel::table! {
    machines (id) {
        id -> Uuid,
        organization_id -> Uuid,
        name -> Text,
    }
}

/// A machine, as the routes answer it.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = machines)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Machine {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
}

impl Machine {
    fn resource(&self) -> OrganizationResource<'static> {
        OrganizationResource {
            kind: KIND,
            organization_id: self.organization_id,
            id: self.id,
        }
    }
}

/// Creates the table, owned by the organization the way an application's is.
///
/// # Panics
/// Panics when the database refuses the statement.
pub async fn create_table(pool: &DbPool) {
    let mut connection = pool.get().await.expect("a connection");
    connection
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS machines (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                organization_id UUID NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("the machines table must be created");
}

/// Adds a machine to an organization, the way an application's create would.
///
/// # Panics
/// Panics when the insert fails.
pub async fn insert(pool: &DbPool, organization_id: Uuid, name: &str) -> Uuid {
    let mut connection = pool.get().await.expect("a connection");
    diesel::insert_into(machines::table)
        .values((
            machines::organization_id.eq(organization_id),
            machines::name.eq(name),
        ))
        .returning(machines::id)
        .get_result(&mut connection)
        .await
        .expect("the machine must be inserted")
}

/// Deletes a machine and forgets its grants, in one transaction, in that order.
///
/// # Errors
/// The delete's own error.
pub async fn delete(connection: &mut AsyncPgConnection, machine_id: Uuid) -> QueryResult<usize> {
    connection
        .transaction(async |transaction| {
            let deleted = diesel::delete(machines::table.find(machine_id))
                .execute(transaction)
                .await?;
            forget_resource(transaction, KIND, machine_id).await?;
            Ok(deleted)
        })
        .await
}

/// The framework's view of the machines table.
#[derive(Debug)]
pub struct Machines;

impl ResourceKind for Machines {
    fn name(&self) -> &str {
        KIND
    }

    fn find<'a>(
        &'a self,
        connection: &'a mut AsyncPgConnection,
        organization_id: Uuid,
        resource_ids: &'a [Uuid],
    ) -> ResourceKindFuture<'a, Vec<FieldOption>> {
        Box::pin(async move {
            let rows: Vec<(Uuid, String)> = machines::table
                .filter(machines::organization_id.eq(organization_id))
                .filter(machines::id.eq_any(resource_ids))
                .select((machines::id, machines::name))
                .for_key_share()
                .load(connection)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(value, label)| FieldOption { value, label })
                .collect())
        })
    }

    fn labels<'a>(
        &'a self,
        connection: &'a mut AsyncPgConnection,
        organization_id: Uuid,
        resource_ids: &'a [Uuid],
    ) -> ResourceKindFuture<'a, Vec<FieldOption>> {
        Box::pin(async move {
            let rows: Vec<(Uuid, String)> = machines::table
                .filter(machines::organization_id.eq(organization_id))
                .filter(machines::id.eq_any(resource_ids))
                .select((machines::id, machines::name))
                .load(connection)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(value, label)| FieldOption { value, label })
                .collect())
        })
    }

    fn options<'a>(
        &'a self,
        connection: &'a mut AsyncPgConnection,
        organization_id: Uuid,
        search: Option<&'a str>,
        limit: i64,
    ) -> ResourceKindFuture<'a, Vec<FieldOption>> {
        Box::pin(async move {
            let mut query = machines::table
                .filter(machines::organization_id.eq(organization_id))
                .select((machines::id, machines::name))
                .order(machines::name.asc())
                .limit(limit)
                .into_boxed();
            if let Some(search) = search {
                // What a person typed is matched literally, so the characters
                // `LIKE` reads as patterns are escaped first.
                let literal = search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                query = query.filter(machines::name.ilike(format!("%{literal}%")).escape('\\'));
            }
            let rows: Vec<(Uuid, String)> = query.load(connection).await?;
            Ok(rows
                .into_iter()
                .map(|(value, label)| FieldOption { value, label })
                .collect())
        })
    }
}

/// The routes an application writes over its machines.
pub fn router(channels: Channels) -> Router {
    Router::new()
        .route(
            "/account/machines/{machine_id}",
            get(show).patch(rename).delete(destroy),
        )
        .route(
            "/account/organizations/{organization_id}/machines",
            get(list),
        )
        .route("/account/machines/{machine_id}/events", get(events))
        .route("/api/v1/machines/{machine_id}", get(show_for_token))
        .route("/api/v1/machines", get(list_for_token))
        .layer(Extension(channels))
}

/// The MCP tool a connected client reads a machine through.
pub fn tool(roles: RoleSet) -> Tool {
    let roles = Arc::new(roles);
    Tool::new(
        "read_machine",
        "Read one machine the person can reach.",
        move |call: ToolCall| {
            let roles = Arc::clone(&roles);
            async move {
                let machine_id: Uuid = call.arguments["machine_id"]
                    .as_str()
                    .and_then(|raw| raw.parse().ok())
                    .ok_or_else(|| ToolError::new("Name a machine by its id."))?;
                let mut connection = call
                    .pool
                    .get()
                    .await
                    .map_err(|_error| ToolError::internal())?;
                let found = load(&mut connection, &roles, call.caller.user.id, machine_id)
                    .await
                    .map_err(|_error| ToolError::internal())?;
                let (machine, access) = found.ok_or_else(|| ToolError::new("No such machine."))?;
                if !access.can(Action::Read, KIND) {
                    return Err(ToolError::new("No such machine."));
                }
                Ok(json!({ "name": machine.name }))
            }
        },
    )
    .input_schema(json!({
        "type": "object",
        "properties": { "machine_id": { "type": "string" } },
        "required": ["machine_id"],
        "additionalProperties": false,
    }))
}

/// Loads one machine with the caller's access to it, as a guard would.
async fn load(
    connection: &mut AsyncPgConnection,
    roles: &RoleSet,
    user_id: Uuid,
    machine_id: Uuid,
) -> QueryResult<Option<(Machine, anubis::tenancy::Access)>> {
    let machine: Option<Machine> = machines::table
        .find(machine_id)
        .select(Machine::as_select())
        .first(connection)
        .await
        .optional()?;
    let Some(machine) = machine else {
        return Ok(None);
    };
    let access = resolve_resource_access(connection, roles, user_id, machine.resource()).await?;
    Ok(access.map(|access| (machine, access)))
}

/// A machine the caller reaches, and what they may do to it, or a `404`.
async fn guarded(
    pool: &DbPool,
    roles: &RoleSet,
    user_id: Uuid,
    machine_id: Uuid,
) -> Result<(Machine, anubis::tenancy::Access), ApiError> {
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    load(&mut connection, roles, user_id, machine_id)
        .await?
        .ok_or_else(ApiError::not_found)
}

async fn show(
    CurrentUser(user): CurrentUser,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Path(machine_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (machine, access) = guarded(&pool, &roles, user.id, machine_id).await?;
    access.require(Action::Read, KIND)?;
    Ok(Json(json!({
        "machine": machine,
        "access": access.view(),
    })))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename(
    CurrentUser(user): CurrentUser,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Path(machine_id): Path<Uuid>,
    Json(body): Json<RenameBody>,
) -> Result<Json<Value>, ApiError> {
    let (machine, access) = guarded(&pool, &roles, user.id, machine_id).await?;
    access.require(Action::Update, KIND)?;

    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    diesel::update(machines::table.find(machine.id))
        .set(machines::name.eq(&body.name))
        .execute(&mut connection)
        .await?;
    Ok(Json(json!({ "name": body.name })))
}

async fn destroy(
    CurrentUser(user): CurrentUser,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Path(machine_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (machine, access) = guarded(&pool, &roles, user.id, machine_id).await?;
    access.require(Action::Destroy, KIND)?;

    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    delete(&mut connection, machine.id).await?;
    Ok(Json(json!({ "deleted": machine.id })))
}

async fn list(
    CurrentUser(user): CurrentUser,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Path(organization_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let scope = resolve_resource_scope(
        &mut connection,
        &roles,
        user.id,
        organization_id,
        KIND,
        Action::Read,
    )
    .await?
    .ok_or_else(ApiError::not_found)?;

    Ok(Json(
        json!({ "machines": scoped(&mut connection, organization_id, &scope).await? }),
    ))
}

/// The organization's machines a scope takes in, by name.
async fn scoped(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    scope: &ResourceScope,
) -> QueryResult<Vec<String>> {
    // The organization filter applies whatever the scope says: `Every` is
    // every machine of this organization, never of any other.
    let mut query = machines::table
        .filter(machines::organization_id.eq(organization_id))
        .select(machines::name)
        .order(machines::name.asc())
        .into_boxed();
    if let ResourceScope::Only(ids) = scope {
        let ids: Vec<Uuid> = ids.iter().copied().collect();
        query = query.filter(machines::id.eq_any(ids));
    }
    query.load(connection).await
}

async fn events(
    subscriber: Subscriber,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Extension(channels): Extension<Channels>,
    Path(machine_id): Path<Uuid>,
) -> Result<EventStream, ApiError> {
    let (machine, access) = guarded(&pool, &roles, subscriber.user().id, machine_id).await?;
    access.require(Action::Read, KIND)?;
    let stream = channels
        .stream(subscriber, [ChannelName::resource(machine.id, "logs")])
        .await?;
    Ok(stream)
}

async fn show_for_token(
    caller: ApiCaller,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
    Path(machine_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require(&roles, Action::Read, KIND)?;

    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let machine: Machine = machines::table
        .find(machine_id)
        .select(Machine::as_select())
        .first(&mut connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    if !team_reaches_resource(&mut connection, &caller.team, machine.resource()).await? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "machine": machine })))
}

async fn list_for_token(
    caller: ApiCaller,
    Extension(pool): Extension<DbPool>,
    Extension(roles): Extension<Arc<RoleSet>>,
) -> Result<Json<Value>, ApiError> {
    caller.require(&roles, Action::Read, KIND)?;

    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let scope = team_resource_scope(&mut connection, &caller.team, KIND).await?;
    let organization_id = caller.team.organization_id;
    Ok(Json(
        json!({ "machines": scoped(&mut connection, organization_id, &scope).await? }),
    ))
}

/// One organization with machines to share, set up the way the suites need.
///
/// The owner administers the organization and its bootstrapped team, made two
/// more teams, A and C, and an Operator role that reads and updates machines
/// (and nothing else). Four machines belong to the organization. Nothing is
/// shared yet: every grant is a test's own.
pub struct Fleet {
    pub harness: Harness,
    pub owner: String,
    pub organization: Uuid,
    pub team_a: Uuid,
    pub team_c: Uuid,
    pub operator: Uuid,
    pub machines: [Uuid; 4],
}

impl Fleet {
    /// Opens a fleet over `database`.
    ///
    /// # Panics
    /// Panics when any step of the setup is refused.
    pub async fn open(database: &TestDatabase) -> Self {
        let harness = Harness::boot(database).await;
        let owner = register(
            &harness.router,
            &format!("owner-{}@example.com", Uuid::new_v4()),
        )
        .await;
        let (status, _headers, body) = send(
            &harness.router,
            "GET",
            "/tenancy/memberships",
            None,
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let organization = id_of(&body["organizations"][0]["id"]);

        let mut machines = [Uuid::nil(); 4];
        for (index, machine) in machines.iter_mut().enumerate() {
            *machine = insert(&harness.pool, organization, &format!("Machine {index}")).await;
        }

        let mut fleet = Self {
            harness,
            owner,
            organization,
            team_a: Uuid::nil(),
            team_c: Uuid::nil(),
            operator: Uuid::nil(),
            machines,
        };
        fleet.team_a = fleet.create_team("Team A").await;
        fleet.team_c = fleet.create_team("Team C").await;
        fleet.operator = fleet
            .create_role("Operator", &["Machine.read", "Machine.update"])
            .await;
        fleet
    }

    /// Sends one request as `cookie`, answering the status and body.
    pub async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        cookie: &str,
    ) -> (StatusCode, Value) {
        let (status, _headers, body) =
            send(&self.harness.router, method, path, body, Some(cookie)).await;
        (status, body)
    }

    /// Makes a team in the organization, as the owner.
    ///
    /// # Panics
    /// Panics when the team is refused.
    pub async fn create_team(&self, name: &str) -> Uuid {
        let path = format!("/tenancy/organizations/{}/teams", self.organization);
        let (status, body) = self
            .send("POST", &path, Some(&json!({ "name": name })), &self.owner)
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        id_of(&body["team"]["id"])
    }

    /// Makes a role carrying `permissions`, as the owner.
    ///
    /// # Panics
    /// Panics when the role is refused.
    pub async fn create_role(&self, name: &str, permissions: &[&str]) -> Uuid {
        let path = format!("/tenancy/organizations/{}/roles", self.organization);
        let role = json!({ "name": name, "color": "#2563eb", "permissions": permissions });
        let (status, body) = self.send("POST", &path, Some(&role), &self.owner).await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        id_of(&body["role"]["id"])
    }

    /// A fresh account, joined to `target` (`team_id` or `organization_id`)
    /// holding `roles` there; answers its email and session.
    ///
    /// # Panics
    /// Panics when the invitation or the claim is refused.
    pub async fn join(&self, target: (&str, Uuid), roles: &[Uuid]) -> (String, String) {
        let email = format!("joiner-{}@example.com", Uuid::new_v4());
        let cookie = register(&self.harness.router, &email).await;
        self.add(&email, &cookie, target, roles).await;
        (email, cookie)
    }

    /// Joins an existing account to `target` holding `roles` there.
    ///
    /// # Panics
    /// Panics when the invitation or the claim is refused.
    pub async fn add(&self, email: &str, cookie: &str, target: (&str, Uuid), roles: &[Uuid]) {
        let invitation = json!({ "email": email, target.0: target.1, "role_ids": roles });
        let (status, body) = self
            .send(
                "POST",
                "/tenancy/invitations",
                Some(&invitation),
                &self.owner,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");

        let token = latest_invitation_token(&self.harness, email);
        let (status, body) = self
            .send(
                "POST",
                "/tenancy/invitations/claim",
                Some(&json!({ "token": token })),
                cookie,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    /// Replaces what `team` reaches of the machines, as `cookie`.
    pub async fn share(&self, cookie: &str, team: Uuid, every: bool, ids: &[Uuid]) -> StatusCode {
        let path = format!(
            "/tenancy/organizations/{}/teams/{team}/resource-grants/{KIND}",
            self.organization
        );
        let body = json!({ "every": every, "resource_ids": ids });
        self.send("PUT", &path, Some(&body), cookie).await.0
    }

    /// Opens one machine's page as `cookie`, answering the status and body.
    pub async fn open_machine(&self, cookie: &str, machine: Uuid) -> (StatusCode, Value) {
        self.send("GET", &format!("/account/machines/{machine}"), None, cookie)
            .await
    }

    /// The machine names `cookie` sees listed in the organization.
    pub async fn listed(&self, cookie: &str) -> (StatusCode, Vec<String>) {
        let path = format!("/account/organizations/{}/machines", self.organization);
        let (status, body) = self.send("GET", &path, None, cookie).await;
        let names = body["machines"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        (status, names)
    }

    /// The access screen's listing, as `cookie`.
    pub async fn grants(&self, cookie: &str) -> (StatusCode, Value) {
        let path = format!(
            "/tenancy/organizations/{}/resource-grants",
            self.organization
        );
        self.send("GET", &path, None, cookie).await
    }

    /// The organization membership behind an email, read off the roster.
    ///
    /// # Panics
    /// Panics when nobody with that email is on the roster.
    pub async fn organization_membership(&self, email: &str) -> Uuid {
        let path = format!("/tenancy/organizations/{}/members", self.organization);
        let (status, body) = self.send("GET", &path, None, &self.owner).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        roster_entry(&body, email)
    }

    /// The team membership behind an email, read off the team's roster.
    ///
    /// # Panics
    /// Panics when nobody with that email is on the roster.
    pub async fn team_membership(&self, team: Uuid, email: &str) -> Uuid {
        let (status, body) = self
            .send(
                "GET",
                &format!("/tenancy/teams/{team}/members"),
                None,
                &self.owner,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        roster_entry(&body, email)
    }

    /// How many grants of machines the database holds for `team`.
    ///
    /// # Panics
    /// Panics when the count cannot be read.
    pub async fn grant_rows(&self, team: Uuid) -> i64 {
        use anubis::schema::resource_grants;

        let mut connection = self.harness.pool.get().await.expect("a connection");
        resource_grants::table
            .filter(resource_grants::team_id.eq(team))
            .count()
            .get_result(&mut connection)
            .await
            .expect("the grants must count")
    }
}

fn id_of(value: &Value) -> Uuid {
    value
        .as_str()
        .expect("an id is a string")
        .parse()
        .expect("an id is a UUID")
}

fn roster_entry(roster: &Value, email: &str) -> Uuid {
    let entry = roster["members"]
        .as_array()
        .expect("a roster lists members")
        .iter()
        .find(|member| member["email"] == email)
        .unwrap_or_else(|| panic!("nobody named {email} in {roster}"));
    id_of(&entry["membership_id"])
}

/// The newest invitation token mailed to an address.
fn latest_invitation_token(harness: &Harness, email: &str) -> String {
    let invitation = harness
        .outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation
        .text_body
        .split_once("token=")
        .expect("the invitation email must carry a link");
    rest.split_whitespace()
        .next()
        .expect("the token ends at whitespace")
        .to_owned()
}
