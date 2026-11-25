//! The access screen's endpoints: which teams reach the organization's records.
//!
//! | Route | Asks for | Effect |
//! |---|---|---|
//! | `GET /organizations/{organization_id}/resource-grants` | member | The kinds the application shares, the teams, and each team's grants |
//! | `PUT /organizations/{organization_id}/teams/{team_id}/resource-grants/{kind}` | `Tenancy::ResourceGrant.update`, and every permission on the kind | Replace what one team reaches of one kind |
//! | `GET /organizations/{organization_id}/resource-grants/{kind}/options` | the same | The organization's records of a kind, for a picker |
//!
//! **Sharing is held to what the sharer holds.** A team granted a kind acts on
//! its records with its own roles, and its administrator's platform tokens
//! with the compiled `admin` grants, so a grant hands the team, at most, every
//! permission on the kind a tenant may hold. Choosing which teams reach a kind
//! therefore takes, beside `Tenancy::ResourceGrant.update`, organization
//! standing that reaches every record of the kind and holds every one of
//! those permissions; an administrator is exempt from holding, as everywhere.
//! A team's administrator cannot share with their own team what they do not
//! reach, because a team standing counts for nothing here.
//!
//! A change runs under the organization's lock, the caller's standing read
//! again there, so two changes to one organization's grants are ordered and
//! the second is judged against what the first left behind. A request states
//! the end state for one team and one kind, so two managers cannot interleave
//! into a set neither asked for, and a change that leaves the set as it was
//! records nothing.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::audit::{self, Changes};
use crate::guard::OrganizationMember;
use crate::http::{ApiError, FieldOption, FieldOptions};
use crate::roles::{Action, RESOURCE_GRANT, RoleSet};
use crate::schema::{resource_grants, teams};
use crate::tenancy::access::{Access, admitted_teams};
use crate::tenancy::hierarchy;
use crate::tenancy::management::lock_organization;
use crate::tenancy::model::Team;
use crate::tenancy::resource_access::OrganizationReach;
use crate::tenancy::routes::{TenancyState, log_internal};

/// Most specific records one request may grant a team.
///
/// Every id is looked up and locked in the request's transaction, so the
/// bound is on that work. A team that should reach more than this many
/// records of a kind is better granted the whole kind.
const MAX_RESOURCE_IDS: usize = 1_000;

/// Most options a picker is offered at once; typing narrows them.
const MAX_OPTIONS: i64 = 50;

/// Longest search a picker sends, which is far past any name a person types.
const MAX_SEARCH_CHARS: usize = 100;

/// Returns the resource grant routes, merged into the tenancy router.
pub(super) fn routes() -> Router<TenancyState> {
    Router::new()
        .route(
            "/organizations/{organization_id}/resource-grants",
            get(list_grants),
        )
        .route(
            "/organizations/{organization_id}/resource-grants/{kind}/options",
            get(list_options),
        )
        .route(
            "/organizations/{organization_id}/teams/{team_id}/resource-grants/{kind}",
            put(replace_grants),
        )
}

/// One kind the application shares, as the viewer stands toward it.
#[derive(Serialize)]
struct KindView {
    kind: String,
    /// Whether the viewer's organization standing reaches every record of it,
    /// which is what lets them see who else does.
    reaches: bool,
    /// Whether the viewer may change which teams reach it.
    may_manage: bool,
}

#[derive(Serialize)]
struct TeamView {
    id: Uuid,
    name: String,
}

/// What one team reaches of one kind.
#[derive(Serialize)]
struct GrantView {
    team_id: Uuid,
    kind: String,
    /// Every record of the kind, now and later.
    every: bool,
    /// The specific records, by name, when `every` is false. A record that
    /// was deleted is left out: its grant is inert.
    resources: Vec<FieldOption>,
}

#[derive(Serialize)]
struct ListBody {
    kinds: Vec<KindView>,
    /// The teams the viewer may read grants against: every team of the
    /// organization for an administrator or a sharer, who may make any of
    /// them a target; the teams that admit the viewer for anybody else; none
    /// for a viewer who reaches no kind.
    teams: Vec<TeamView>,
    /// Those teams' grants, of the kinds the viewer reaches.
    grants: Vec<GrantView>,
}

#[derive(Serialize)]
struct GrantBody {
    grant: GrantView,
}

/// Replaces what one team reaches of one kind, so it is strict: a body that
/// does not say both, or names a field this server does not take, is refused
/// rather than read as "nothing", which would revoke everything the team had.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantsBody {
    /// Every record of the kind the organization owns, now and later.
    every: bool,
    /// Specific records, when `every` is false. Empty with `every` false
    /// leaves the team reaching nothing of the kind.
    resource_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
struct OptionsQuery {
    search: Option<String>,
}

/// Lists the kinds, the teams, and the grants of every kind the viewer reaches.
///
/// Readable by any member, because the screen says who may manage what; the
/// grants of a kind are listed only to somebody whose organization standing
/// reaches every record of it, so a record's name never reaches a member who
/// could not open the record itself. Only an administrator or a sharer is
/// shown every team: anybody else reads the teams that admit them, which is
/// what the membership overview already shows them, so no team a suspension
/// cut them out of is named here.
async fn list_grants(
    State(state): State<TenancyState>,
    member: OrganizationMember,
) -> Result<impl IntoResponse, ApiError> {
    let organization_id = member.organization.id;
    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let reach = OrganizationReach::read(&mut connection, member.user.id, organization_id)
        .await?
        .ok_or_else(ApiError::not_found)?;

    let mut kinds = Vec::new();
    let mut reached = Vec::new();
    for kind in state.kinds.iter() {
        let over = reach.on_every(kind.name(), &state.roles);
        let may_manage = over
            .as_ref()
            .is_some_and(|access| may_share(access, kind.name(), &state.roles).is_ok());
        if over.is_some() {
            reached.push(kind);
        }
        kinds.push(KindView {
            kind: kind.name().to_owned(),
            reaches: over.is_some(),
            may_manage,
        });
    }

    if reached.is_empty() {
        return Ok(Json(ListBody {
            kinds,
            teams: Vec::new(),
            grants: Vec::new(),
        }));
    }

    let sees_every_team =
        member.access.is_administrator() || kinds.iter().any(|kind| kind.may_manage);
    let listed: Vec<(Uuid, String)> = if sees_every_team {
        teams::table
            .filter(teams::organization_id.eq(organization_id))
            .select((teams::id, teams::name))
            .order((teams::name.asc(), teams::id.asc()))
            .load(&mut connection)
            .await?
    } else {
        admitted_teams(member.user.id)
            .filter(teams::organization_id.eq(organization_id))
            .select((teams::id, teams::name))
            .order((teams::name.asc(), teams::id.asc()))
            .load(&mut connection)
            .await?
    };
    let team_ids: Vec<Uuid> = listed.iter().map(|(id, _name)| *id).collect();
    let teams: Vec<TeamView> = listed
        .into_iter()
        .map(|(id, name)| TeamView { id, name })
        .collect();

    let mut grants = Vec::new();
    for kind in reached {
        let rows: Vec<(Uuid, Option<Uuid>)> = resource_grants::table
            .filter(resource_grants::organization_id.eq(organization_id))
            .filter(resource_grants::kind.eq(kind.name()))
            .filter(resource_grants::team_id.eq_any(&team_ids))
            .select((resource_grants::team_id, resource_grants::resource_id))
            .load(&mut connection)
            .await?;

        let mut by_team: BTreeMap<Uuid, Vec<Option<Uuid>>> = BTreeMap::new();
        let mut named = BTreeSet::new();
        for (team_id, resource_id) in rows {
            if let Some(resource_id) = resource_id {
                named.insert(resource_id);
            }
            by_team.entry(team_id).or_default().push(resource_id);
        }

        // One lookup labels every record any team was granted, and leaves out
        // the ones that were deleted since.
        let named: Vec<Uuid> = named.into_iter().collect();
        let found = kind
            .labels(&mut connection, organization_id, &named)
            .await?;
        for (team_id, granted) in by_team {
            grants.push(grant_view(team_id, kind.name(), &granted, &found));
        }
    }

    Ok(Json(ListBody {
        kinds,
        teams,
        grants,
    }))
}

/// Offers the organization's records of a kind to somebody sharing them.
async fn list_options(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    Path((_organization_id, kind)): Path<(Uuid, String)>,
    Query(query): Query<OptionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = state.kinds.get(&kind).ok_or_else(ApiError::not_found)?;
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let reach = OrganizationReach::read(&mut connection, member.user.id, organization_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    require_may_share(&reach, kind.name(), &state.roles)?;

    let search: Option<String> = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(|search| search.chars().take(MAX_SEARCH_CHARS).collect());
    let options = kind
        .options(
            &mut connection,
            organization_id,
            search.as_deref(),
            MAX_OPTIONS,
        )
        .await?;

    Ok(Json(FieldOptions { options }))
}

/// Replaces what one team reaches of one kind with the requested end state.
async fn replace_grants(
    State(state): State<TenancyState>,
    member: OrganizationMember,
    context: audit::Context,
    Path((_organization_id, team_id, kind)): Path<(Uuid, Uuid, String)>,
    Json(body): Json<GrantsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = state.kinds.get(&kind).ok_or_else(ApiError::not_found)?;
    if body.every && !body.resource_ids.is_empty() {
        return Err(ApiError::validation(
            "Grant a team every record of a kind or specific ones, not both.",
        ));
    }
    let requested: BTreeSet<Uuid> = body.resource_ids.iter().copied().collect();
    if requested.len() > MAX_RESOURCE_IDS {
        return Err(ApiError::validation(format!(
            "Grant a team at most {MAX_RESOURCE_IDS} records of a kind at once, \
             or every record of it."
        )));
    }

    // Refused before the organization's row is locked, so a member who could
    // never make this change queues nobody else's behind it. The standing is
    // read again under the lock before anything is written.
    member.access.require(Action::Update, RESOURCE_GRANT)?;
    let organization_id = member.organization.id;

    let mut connection = state.pool.get().await.map_err(log_internal)?;
    let grant = connection
        .transaction::<GrantView, ApiError, _>(async |transaction| {
            lock_organization(transaction, organization_id).await?;
            let reach = OrganizationReach::read(transaction, member.user.id, organization_id)
                .await?
                .ok_or_else(ApiError::not_found)?;
            require_may_share(&reach, kind.name(), &state.roles)?;

            let team = find_team(transaction, organization_id, team_id).await?;

            let named: Vec<Uuid> = requested.iter().copied().collect();
            let found = kind.find(transaction, organization_id, &named).await?;
            // Another organization's record and one that never existed are the
            // same answer, so a probe learns nothing about either.
            let owned: BTreeSet<Uuid> = found.iter().map(|option| option.value).collect();
            if owned != requested {
                return Err(ApiError::not_found());
            }

            let held: Vec<Option<Uuid>> = resource_grants::table
                .filter(resource_grants::team_id.eq(team.id))
                .filter(resource_grants::kind.eq(kind.name()))
                .select(resource_grants::resource_id)
                .load(transaction)
                .await?;
            // Named before anything is written, and without a lock: a record
            // being deleted is waiting to forget the grants this change is
            // about to remove, so locking it now would wait on that delete
            // while it waits on these rows.
            let was_named: Vec<Uuid> = held.iter().flatten().copied().collect();
            let was = kind
                .labels(transaction, organization_id, &was_named)
                .await?;
            let wanted: BTreeSet<Option<Uuid>> = if body.every {
                BTreeSet::from([None])
            } else {
                requested.iter().copied().map(Some).collect()
            };
            let held: BTreeSet<Option<Uuid>> = held.into_iter().collect();

            let revoked = revoke(transaction, &team, kind.name(), &held, &wanted).await?;
            let granted = grant(
                transaction,
                &team,
                kind.name(),
                &held,
                &wanted,
                member.user.id,
            )
            .await?;

            // Recorded only when what the team reaches moved: two identical
            // requests in a row, or racing, record one event between them, and
            // clearing a grant whose record was deleted since, which was inert,
            // records none. Compared by id, since two records may share a name.
            let reached: BTreeSet<Option<Uuid>> = held
                .iter()
                .filter(|granted| {
                    granted.is_none_or(|id| was.iter().any(|option| option.value == id))
                })
                .copied()
                .collect();
            let moved = revoked + granted > 0 && reached != wanted;

            let held: Vec<Option<Uuid>> = held.into_iter().collect();
            let wanted: Vec<Option<Uuid>> = wanted.into_iter().collect();
            let after = grant_view(team.id, kind.name(), &wanted, &found);
            if !moved {
                return Ok(after);
            }

            // Named as the records read now; one deleted since has no name left.
            let changes = Changes::new().field(
                kind.name(),
                describe(&held, &was),
                describe(&wanted, &found),
            );
            audit::record(
                transaction,
                &context.by(&member.user),
                &audit::Event::new(audit::TEAM_ACCESS_CHANGED, "Team")
                    .organization(organization_id)
                    .subject(team.id)
                    .label(&team.name)
                    .changes(changes),
            )
            .await?;
            Ok(after)
        })
        .await?;

    Ok(Json(GrantBody { grant }))
}

/// Refuses unless the caller's organization standing may share `kind`.
///
/// # Errors
/// Returns `403` when the standing lacks `Tenancy::ResourceGrant.update`, does
/// not reach every record of the kind, or lacks a permission on it.
fn require_may_share(
    reach: &OrganizationReach,
    kind: &str,
    roles: &RoleSet,
) -> Result<(), ApiError> {
    let Some(access) = reach.on_every(kind, roles) else {
        return Err(ApiError::forbidden(format!(
            "You can only choose which teams reach {kind} records when your organization-wide \
             roles reach all of them."
        )));
    };
    may_share(&access, kind, roles)
}

/// The rule behind [`require_may_share`], over a standing that reaches the kind.
///
/// Sharing hands a team every permission on the kind a tenant may hold, at
/// most, so the sharer must hold every one of them where the records are:
/// organization-wide. That is hierarchy rule 2, "only what you hold can be
/// given", asked of the whole kind at once.
fn may_share(access: &Access, kind: &str, roles: &RoleSet) -> Result<(), ApiError> {
    access.require(Action::Update, RESOURCE_GRANT)?;
    let on_kind = roles
        .grantable()
        .iter()
        .filter(|permission| permission.subject() == kind);
    hierarchy::require_holds(access, on_kind)
}

/// The organization's team `team_id`, locked against deletion until the change commits.
///
/// Deleting the team then waits for the grants to land and takes them with
/// it, instead of the grants' insert failing on a team that just went.
async fn find_team(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    team_id: Uuid,
) -> Result<Team, ApiError> {
    teams::table
        .filter(teams::id.eq(team_id))
        .filter(teams::organization_id.eq(organization_id))
        .select(Team::as_select())
        .for_key_share()
        .first(connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)
}

/// Deletes the team's grants of `kind` the end state no longer holds.
///
/// At most two statements, whatever the size of the change, because the
/// organization's lock is held across them: the named records go in one, and
/// the grant of every record in the other.
async fn revoke(
    connection: &mut AsyncPgConnection,
    team: &Team,
    kind: &str,
    held: &BTreeSet<Option<Uuid>>,
    wanted: &BTreeSet<Option<Uuid>>,
) -> QueryResult<usize> {
    let mut revoked = 0;

    let named: Vec<Uuid> = held.difference(wanted).flatten().copied().collect();
    if !named.is_empty() {
        revoked += diesel::delete(
            resource_grants::table
                .filter(resource_grants::team_id.eq(team.id))
                .filter(resource_grants::kind.eq(kind))
                .filter(resource_grants::resource_id.eq_any(&named)),
        )
        .execute(connection)
        .await?;
    }

    if held.contains(&None) && !wanted.contains(&None) {
        revoked += diesel::delete(
            resource_grants::table
                .filter(resource_grants::team_id.eq(team.id))
                .filter(resource_grants::kind.eq(kind))
                .filter(resource_grants::resource_id.is_null()),
        )
        .execute(connection)
        .await?;
    }

    Ok(revoked)
}

#[derive(Insertable)]
#[diesel(table_name = resource_grants)]
struct NewResourceGrant<'a> {
    organization_id: Uuid,
    team_id: Uuid,
    kind: &'a str,
    resource_id: Option<Uuid>,
    granted_by: Option<Uuid>,
}

/// Inserts the grants of `kind` the end state holds and the team does not.
async fn grant(
    connection: &mut AsyncPgConnection,
    team: &Team,
    kind: &str,
    held: &BTreeSet<Option<Uuid>>,
    wanted: &BTreeSet<Option<Uuid>>,
    granted_by: Uuid,
) -> QueryResult<usize> {
    let rows: Vec<NewResourceGrant<'_>> = wanted
        .difference(held)
        .map(|resource_id| NewResourceGrant {
            organization_id: team.organization_id,
            team_id: team.id,
            kind,
            resource_id: *resource_id,
            granted_by: Some(granted_by),
        })
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }

    diesel::insert_into(resource_grants::table)
        .values(&rows)
        .on_conflict_do_nothing()
        .execute(connection)
        .await
}

/// What one team's grants of one kind read as, from its rows.
///
/// `found` is what the kind answered for the records named, which may hold
/// more than this team's and leaves out the ones deleted since.
fn grant_view(
    team_id: Uuid,
    kind: &str,
    granted: &[Option<Uuid>],
    found: &[FieldOption],
) -> GrantView {
    GrantView {
        team_id,
        kind: kind.to_owned(),
        every: granted.iter().any(Option::is_none),
        resources: named_resources(granted, found),
    }
}

/// The specific records among `granted` that `found` names, by name.
///
/// None when the grants take in every record, because then naming a few of
/// them would misread as only those.
fn named_resources(granted: &[Option<Uuid>], found: &[FieldOption]) -> Vec<FieldOption> {
    if granted.iter().any(Option::is_none) {
        return Vec::new();
    }

    let ids: BTreeSet<Uuid> = granted.iter().flatten().copied().collect();
    let mut resources: Vec<FieldOption> = found
        .iter()
        .filter(|option| ids.contains(&option.value))
        .cloned()
        .collect();
    resources.sort_by(|left, right| left.label.cmp(&right.label));
    resources
}

/// How a team's grants of one kind read in the audit log.
///
/// Names are copied, as everywhere in the log, so a record renamed or deleted
/// later still reads as it did when the grant changed.
fn describe(granted: &[Option<Uuid>], found: &[FieldOption]) -> Value {
    let names: Vec<String> = named_resources(granted, found)
        .into_iter()
        .map(|resource| resource.label)
        .collect();
    json!({
        "every": granted.iter().any(Option::is_none),
        "resources": names,
    })
}
