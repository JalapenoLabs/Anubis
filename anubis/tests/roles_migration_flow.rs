//! The runtime roles migration, run over an application's real rows.
//!
//! Before roles were data, a member's roles were a `TEXT[]` of `roles.yml`
//! keys on each membership. The migration turns every one of those keys into
//! a grant of the organization's role seeded from it. This suite builds the
//! rows an application stamped before it would hold (organization and team
//! administrators, a team administrator with no organization membership,
//! editors, a billing member, a guest with a sub-tenant grant, a suspended
//! member, members holding nothing, a second organization, and invitations
//! still in flight), runs the migration, and pins that every member can do
//! exactly what their keys let them do before: nothing lost, and nothing
//! gained but the default role every member now holds.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use std::collections::BTreeSet;

use anubis::roles::{Action, MEMBERSHIP, Permission, ROLE, RoleSet, SUBSCRIPTION};
use anubis::schema::{invitation_roles, organizations, role_grants, roles};
use anubis::tenancy::{
    Access, Reach, resolve_organization_access, resolve_sub_tenant_access, resolve_team_access,
};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use diesel::migration::{Migration, MigrationConnection, MigrationSource};
use diesel::pg::Pg;
use diesel::prelude::*;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use diesel_migrations::MigrationHarness;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use support::{Harness, TestDatabase, register, send};

/// The suffix of the migration under test, matched by name so that
/// renumbering it never breaks this suite.
const RUNTIME_ROLES: &str = "_runtime_roles";

/// A `roles.yml` shaped like the starter's, with one model to grant.
const ROLES_YML: &str = "
roles:
  default:
    models:
      Project: [read]
  editor:
    includes: [default]
    models:
      Project: [manage]
  billing:
    includes: [default]
    scopes: [organization]
    models: {}
  admin:
    includes: [editor, billing]
    models: {}
";

/// The cast and the tenants they stand in, by id.
struct Legacy {
    acme: Uuid,
    other: Uuid,
    main: Uuid,
    atlas: Uuid,
    general: Uuid,
    atlas_crew: Uuid,
    admin: Uuid,
    team_admin: Uuid,
    editor: Uuid,
    biller: Uuid,
    guest: Uuid,
    suspended: Uuid,
    plain: Uuid,
    other_admin: Uuid,
}

/// The emails and tokens of the two invitations left in flight.
const TEAM_INVITEE: &str = "legacy-team-invitee@example.com";
const TEAM_TOKEN: &str = "legacy-team-invitation-token";
const ORGANIZATION_INVITEE: &str = "legacy-organization-invitee@example.com";
const ORGANIZATION_TOKEN: &str = "legacy-organization-invitation-token";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one matrix, every row of which is a member somebody depends on"
)]
async fn every_member_keeps_exactly_what_their_keys_granted() {
    let Some(database) = TestDatabase::create_bare("roles_migration_flow").await else {
        return;
    };
    apply_before_runtime_roles(database.url()).await;
    let pool = database.pool().await;
    let mut connection = pool.get().await.expect("a connection");
    let legacy = seed_legacy_rows(&mut connection).await;

    anubis::db::run_pending_migrations(database.url())
        .await
        .expect("the runtime roles migration must apply over legacy rows");

    let set = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");

    // ---------------------------------------------------------------------
    // Every organization gets a role per key its people held, and the two
    // every organization has; none reaches into another organization.
    // ---------------------------------------------------------------------
    assert_eq!(
        ladder(&mut connection, legacy.acme).await,
        ["default", "billing", "editor", "ghost", "admin"],
        "the default role at the bottom, the administrator on top, the rest by key",
    );
    assert_eq!(
        ladder(&mut connection, legacy.other).await,
        ["default", "admin"]
    );
    let offered: Vec<String> = organizations::table
        .find(legacy.acme)
        .select(organizations::offered_role_keys)
        .first(&mut connection)
        .await
        .expect("the organization must exist");
    assert_eq!(offered, ["admin", "billing", "default", "editor", "ghost"]);
    let stray: i64 = role_grants::table
        .inner_join(roles::table)
        .filter(roles::organization_id.ne(role_grants::organization_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run");
    assert_eq!(stray, 0, "no grant names another organization's role");

    // ---------------------------------------------------------------------
    // The organization: what each member's organization keys allowed.
    // ---------------------------------------------------------------------
    for (user, keys) in [
        (legacy.admin, &["admin"][..]),
        (legacy.editor, &["default"][..]),
        (legacy.biller, &["billing", "default"][..]),
        (legacy.guest, &["editor"][..]),
        // Held nothing at all, and gains the default role and only that.
        (legacy.plain, &[][..]),
    ] {
        let (_organization, _membership, access) =
            resolve_organization_access(&mut connection, &set, user, legacy.acme)
                .await
                .expect("resolution must run")
                .expect("an organization member reaches the organization");
        assert_access(&set, &access, keys);
    }
    for user in [legacy.suspended, legacy.team_admin, legacy.other_admin] {
        assert!(
            resolve_organization_access(&mut connection, &set, user, legacy.acme)
                .await
                .expect("resolution must run")
                .is_none(),
            "a suspension, a team-only member, and a stranger reach nothing",
        );
    }
    let (_organization, _membership, access) =
        resolve_organization_access(&mut connection, &set, legacy.other_admin, legacy.other)
            .await
            .expect("resolution must run")
            .expect("the other organization keeps its own administrator");
    assert_access(&set, &access, &["admin"]);

    // ---------------------------------------------------------------------
    // Teams: a team's roles were the team membership's alone, and still are.
    // ---------------------------------------------------------------------
    for (team, user, keys) in [
        (legacy.general, legacy.admin, &["admin"][..]),
        (legacy.general, legacy.team_admin, &["admin"][..]),
        (legacy.general, legacy.editor, &["editor"][..]),
        (legacy.atlas_crew, legacy.editor, &["default"][..]),
    ] {
        let (_team, _membership, access) =
            resolve_team_access(&mut connection, &set, user, team, &[])
                .await
                .expect("resolution must run")
                .expect("a team member reaches their team");
        assert_access(&set, &access, keys);
    }
    assert!(
        resolve_team_access(&mut connection, &set, legacy.suspended, legacy.general, &[])
            .await
            .expect("resolution must run")
            .is_none(),
        "a suspension still closes every team in the organization",
    );

    // ---------------------------------------------------------------------
    // Sub-tenants: the union of every applying grant, as before.
    // ---------------------------------------------------------------------
    for (sub_tenant, user, reach, keys) in [
        (
            legacy.atlas,
            legacy.admin,
            Reach::Administrator,
            &["admin"][..],
        ),
        (legacy.atlas, legacy.team_admin, Reach::Team, &["admin"][..]),
        (
            legacy.atlas,
            legacy.editor,
            Reach::Organization,
            &["default", "editor"][..],
        ),
        (
            legacy.atlas,
            legacy.biller,
            Reach::Organization,
            &["billing", "default"][..],
        ),
        // The guest's organization `editor` never cascaded; their grant here
        // is what they hold.
        (legacy.atlas, legacy.guest, Reach::Grant, &["editor"][..]),
        (legacy.main, legacy.plain, Reach::Organization, &[][..]),
    ] {
        let (_sub_tenant, standing) =
            resolve_sub_tenant_access(&mut connection, &set, user, sub_tenant)
                .await
                .expect("resolution must run")
                .expect("each of these reached the sub-tenant before");
        assert_eq!(standing.reach, reach);
        assert_access(&set, &standing.access, keys);
    }
    for (sub_tenant, user) in [
        (legacy.main, legacy.guest),
        (legacy.atlas, legacy.suspended),
    ] {
        assert!(
            resolve_sub_tenant_access(&mut connection, &set, user, sub_tenant)
                .await
                .expect("resolution must run")
                .is_none(),
            "a guest's other sub-tenant and a suspension stay closed",
        );
    }

    // ---------------------------------------------------------------------
    // Invitations in flight: the team one's roles wait on its membership,
    // the organization one's in its offer, and both claims grant them.
    // ---------------------------------------------------------------------
    let offered_by_organization_invitation: Vec<Option<String>> = invitation_roles::table
        .inner_join(roles::table)
        .select(roles::key)
        .order(roles::key.asc())
        .load(&mut connection)
        .await
        .expect("the offer must load");
    assert_eq!(
        offered_by_organization_invitation,
        [Some("billing".to_owned()), Some("ghost".to_owned())],
    );

    let harness = Harness::boot(&database).await;
    let team_invitee = register(&harness.router, TEAM_INVITEE).await;
    let organization_invitee = register(&harness.router, ORGANIZATION_INVITEE).await;
    for (cookie, token) in [
        (&team_invitee, TEAM_TOKEN),
        (&organization_invitee, ORGANIZATION_TOKEN),
    ] {
        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            "/tenancy/invitations/claim",
            Some(&json!({ "token": token })),
            Some(cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }
    let team_invitee_id = account(&mut connection, TEAM_INVITEE).await;
    let (_team, _membership, access) =
        resolve_team_access(&mut connection, &set, team_invitee_id, legacy.general, &[])
            .await
            .expect("resolution must run")
            .expect("the claim joined the team");
    assert_access(&set, &access, &["editor"]);
    let organization_invitee_id = account(&mut connection, ORGANIZATION_INVITEE).await;
    let (_organization, _membership, access) =
        resolve_organization_access(&mut connection, &set, organization_invitee_id, legacy.acme)
            .await
            .expect("resolution must run")
            .expect("the claim joined the organization");
    assert_access(&set, &access, &["billing", "ghost"]);
}

#[tokio::test]
async fn going_back_narrows_and_never_widens() {
    let Some(database) = TestDatabase::create_bare("roles_migration_flow").await else {
        return;
    };
    apply_before_runtime_roles(database.url()).await;
    let pool = database.pool().await;
    let mut connection = pool.get().await.expect("a connection");
    let legacy = seed_legacy_rows(&mut connection).await;
    anubis::db::run_pending_migrations(database.url())
        .await
        .expect("the runtime roles migration must apply over legacy rows");

    // The organization edits its editor role after the migration: from then on
    // no key means what that role means.
    diesel::update(
        roles::table
            .filter(roles::organization_id.eq(legacy.acme))
            .filter(roles::key.eq("editor")),
    )
    .set(roles::permissions.eq(Some(vec!["Project.read".to_owned()])))
    .execute(&mut connection)
    .await
    .expect("the role must be editable");
    drop(connection);

    revert_through_runtime_roles(database.url()).await;

    let mut connection = pool.get().await.expect("a connection");
    let organization_keys: Vec<(Uuid, Vec<String>)> = diesel::sql_query(
        "SELECT user_id, roles FROM organization_memberships WHERE organization_id = $1",
    )
    .bind::<diesel::sql_types::Uuid, _>(legacy.acme)
    .load::<KeysRow>(&mut connection)
    .await
    .expect("the restored column must read")
    .into_iter()
    .map(|row| (row.user_id, row.roles))
    .collect();
    let keys_of = |user: Uuid| {
        organization_keys
            .iter()
            .find(|(held_by, _keys)| *held_by == user)
            .map(|(_user, keys)| keys.clone())
            .unwrap_or_default()
    };
    assert_eq!(keys_of(legacy.admin), ["admin", "default"]);
    assert_eq!(keys_of(legacy.biller), ["billing", "default"]);
    assert_eq!(
        keys_of(legacy.guest),
        ["default"],
        "an edited role has no key granting the same and no more, so it is dropped",
    );

    let team_keys: Vec<KeysRow> = diesel::sql_query(
        "SELECT user_id, roles FROM team_memberships WHERE team_id = $1 AND user_id = $2",
    )
    .bind::<diesel::sql_types::Uuid, _>(legacy.general)
    .bind::<diesel::sql_types::Uuid, _>(legacy.editor)
    .load(&mut connection)
    .await
    .expect("the restored column must read");
    assert_eq!(team_keys[0].roles, ["default"]);
}

#[derive(QueryableByName)]
struct KeysRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    user_id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Array<diesel::sql_types::Text>)]
    roles: Vec<String>,
}

/// Asserts that `access` is exactly what holding `keys` meant before.
///
/// Before, a member held their keys and nothing else; the administrator key
/// passed every framework check by name. Now every member also holds the
/// default role, and the administrator role carries the framework's
/// permissions as permissions. Any difference beyond those two is access the
/// migration lost or invented.
fn assert_access(set: &RoleSet, access: &Access, keys: &[&str]) {
    let mut expected: BTreeSet<Permission> = set.permissions_of("default");
    for key in keys {
        expected.extend(set.permissions_of(key));
    }
    let administrator = keys.contains(&"admin");
    if administrator {
        expected.extend(
            [
                (MEMBERSHIP, Action::Create),
                (MEMBERSHIP, Action::Update),
                (MEMBERSHIP, Action::Destroy),
                (ROLE, Action::Create),
                (ROLE, Action::Update),
                (ROLE, Action::Destroy),
                (SUBSCRIPTION, Action::Update),
            ]
            .into_iter()
            .map(|(subject, action)| Permission::new(subject, action)),
        );
    }

    assert_eq!(access.permissions(), &expected, "keys {keys:?}");
    assert_eq!(access.is_administrator(), administrator, "keys {keys:?}");
}

/// The keys of an organization's roles, from the bottom of the ladder up.
async fn ladder(connection: &mut AsyncPgConnection, organization: Uuid) -> Vec<String> {
    roles::table
        .filter(roles::organization_id.eq(organization))
        .order(roles::position.asc())
        .select(roles::key)
        .load::<Option<String>>(connection)
        .await
        .expect("the ladder must load")
        .into_iter()
        .flatten()
        .collect()
}

async fn account(connection: &mut AsyncPgConnection, email: &str) -> Uuid {
    anubis::schema::users::table
        .filter(anubis::schema::users::email.eq(email))
        .select(anubis::schema::users::id)
        .first(connection)
        .await
        .expect("the account must exist")
}

/// Applies every framework migration that comes before the one under test.
async fn apply_before_runtime_roles(url: &str) {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut connection = AsyncConnectionWrapper::<AsyncPgConnection>::establish(&url)
            .expect("the migration connection must open");
        let mut migrations: Vec<Box<dyn Migration<Pg>>> =
            MigrationSource::<Pg>::migrations(&anubis::db::MIGRATIONS)
                .expect("the embedded migrations must list");
        migrations.sort_by(|left, right| left.name().version().cmp(&right.name().version()));
        // Applying one migration at a time skips the bookkeeping table a full
        // pass creates first, so it is created here.
        MigrationConnection::setup(&mut connection)
            .expect("the migration bookkeeping table must be creatable");

        let mut reached = false;
        for migration in &migrations {
            if migration.name().to_string().ends_with(RUNTIME_ROLES) {
                reached = true;
                break;
            }
            connection
                .run_migration(migration.as_ref())
                .expect("an earlier migration must apply");
        }
        assert!(reached, "the runtime roles migration must exist");
    })
    .await
    .expect("the migration task must not panic");
}

/// Reverts migrations, newest first, until the one under test is undone.
async fn revert_through_runtime_roles(url: &str) {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut connection = AsyncConnectionWrapper::<AsyncPgConnection>::establish(&url)
            .expect("the migration connection must open");
        let names: Vec<(String, String)> =
            MigrationSource::<Pg>::migrations(&anubis::db::MIGRATIONS)
                .expect("the embedded migrations must list")
                .iter()
                .map(|migration| {
                    (
                        migration.name().version().to_string(),
                        migration.name().to_string(),
                    )
                })
                .collect();

        loop {
            let reverted = connection
                .revert_last_migration(anubis::db::MIGRATIONS)
                .expect("a migration must revert")
                .to_string();
            let reverted_name = names
                .iter()
                .find(|(version, _name)| *version == reverted)
                .map(|(_version, name)| name.clone())
                .unwrap_or_default();
            if reverted_name.ends_with(RUNTIME_ROLES) {
                break;
            }
        }
    })
    .await
    .expect("the migration task must not panic");
}

/// The SHA-256 the framework stores for an invitation token.
fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

/// Writes the rows an application stamped before runtime roles would hold.
///
/// Raw SQL, because the `roles` columns these rows carry no longer exist in
/// the framework's schema module.
async fn seed_legacy_rows(connection: &mut AsyncPgConnection) -> Legacy {
    let legacy = Legacy {
        acme: Uuid::new_v4(),
        other: Uuid::new_v4(),
        main: Uuid::new_v4(),
        atlas: Uuid::new_v4(),
        general: Uuid::new_v4(),
        atlas_crew: Uuid::new_v4(),
        admin: Uuid::new_v4(),
        team_admin: Uuid::new_v4(),
        editor: Uuid::new_v4(),
        biller: Uuid::new_v4(),
        guest: Uuid::new_v4(),
        suspended: Uuid::new_v4(),
        plain: Uuid::new_v4(),
        other_admin: Uuid::new_v4(),
    };
    let pending_membership = Uuid::new_v4();
    let other_team = Uuid::new_v4();
    let other_sub_tenant = Uuid::new_v4();

    let statements = [
        format!(
            "INSERT INTO users (id, email, password_hash) VALUES \
             ('{}', 'legacy-admin@example.com', 'x'), \
             ('{}', 'legacy-team-admin@example.com', 'x'), \
             ('{}', 'legacy-editor@example.com', 'x'), \
             ('{}', 'legacy-biller@example.com', 'x'), \
             ('{}', 'legacy-guest@example.com', 'x'), \
             ('{}', 'legacy-suspended@example.com', 'x'), \
             ('{}', 'legacy-plain@example.com', 'x'), \
             ('{}', 'legacy-other-admin@example.com', 'x')",
            legacy.admin,
            legacy.team_admin,
            legacy.editor,
            legacy.biller,
            legacy.guest,
            legacy.suspended,
            legacy.plain,
            legacy.other_admin,
        ),
        format!(
            "INSERT INTO organizations (id, name) VALUES ('{}', 'Acme'), ('{}', 'Other')",
            legacy.acme, legacy.other,
        ),
        format!(
            "INSERT INTO sub_tenants (id, organization_id, name) VALUES \
             ('{}', '{}', 'Main'), ('{}', '{}', 'Atlas'), ('{}', '{}', 'Main')",
            legacy.main, legacy.acme, legacy.atlas, legacy.acme, other_sub_tenant, legacy.other,
        ),
        format!(
            "INSERT INTO teams (id, organization_id, name, sub_tenant_id) VALUES \
             ('{}', '{}', 'General', NULL), ('{}', '{}', 'Atlas crew', '{}'), \
             ('{}', '{}', 'General', NULL)",
            legacy.general,
            legacy.acme,
            legacy.atlas_crew,
            legacy.acme,
            legacy.atlas,
            other_team,
            legacy.other,
        ),
        // A duplicated key, a guest holding an organization role, a suspended
        // administrator, and a member holding nothing.
        format!(
            "INSERT INTO organization_memberships \
             (organization_id, user_id, roles, access, suspended_at) VALUES \
             ('{acme}', '{}', '{{admin}}', 'full', NULL), \
             ('{acme}', '{}', '{{default}}', 'full', NULL), \
             ('{acme}', '{}', '{{billing,default,billing}}', 'full', NULL), \
             ('{acme}', '{}', '{{editor}}', 'guest', NULL), \
             ('{acme}', '{}', '{{admin}}', 'full', now()), \
             ('{acme}', '{}', '{{}}', 'full', NULL), \
             ('{}', '{}', '{{admin}}', 'full', NULL)",
            legacy.admin,
            legacy.editor,
            legacy.biller,
            legacy.guest,
            legacy.suspended,
            legacy.plain,
            legacy.other,
            legacy.other_admin,
            acme = legacy.acme,
        ),
        format!(
            "INSERT INTO sub_tenant_memberships (sub_tenant_id, user_id, roles) VALUES \
             ('{}', '{}', '{{editor}}'), ('{}', '{}', '{{}}')",
            legacy.atlas, legacy.guest, legacy.main, legacy.plain,
        ),
        // The team administrator holds no organization membership at all,
        // which a team-only invitation produces.
        format!(
            "INSERT INTO team_memberships (id, team_id, user_id, roles) VALUES \
             (gen_random_uuid(), '{general}', '{}', '{{admin}}'), \
             (gen_random_uuid(), '{general}', '{}', '{{admin}}'), \
             (gen_random_uuid(), '{general}', '{}', '{{editor}}'), \
             (gen_random_uuid(), '{general}', '{}', '{{editor}}'), \
             (gen_random_uuid(), '{}', '{}', '{{default}}'), \
             (gen_random_uuid(), '{}', '{}', '{{admin}}'), \
             ('{}', '{general}', NULL, '{{editor}}')",
            legacy.admin,
            legacy.team_admin,
            legacy.editor,
            legacy.suspended,
            legacy.atlas_crew,
            legacy.editor,
            other_team,
            legacy.other_admin,
            pending_membership,
            general = legacy.general,
        ),
        format!(
            "INSERT INTO invitations \
             (email, organization_id, team_id, team_membership_id, roles, invited_by, \
              token_hash, expires_at) VALUES \
             ('{TEAM_INVITEE}', '{acme}', '{}', '{}', '{{editor}}', '{}', '{}', \
              now() + interval '1 day'), \
             ('{ORGANIZATION_INVITEE}', '{acme}', NULL, NULL, '{{billing,ghost}}', '{}', '{}', \
              now() + interval '1 day')",
            legacy.general,
            pending_membership,
            legacy.admin,
            token_hash(TEAM_TOKEN),
            legacy.admin,
            token_hash(ORGANIZATION_TOKEN),
            acme = legacy.acme,
        ),
    ];

    for statement in statements {
        diesel::sql_query(&statement)
            .execute(connection)
            .await
            .unwrap_or_else(|error| panic!("{error}: {statement}"));
    }

    // Sanity: the rows really are the legacy shape this suite claims.
    let legacy_columns: i64 = diesel::sql_query(
        "SELECT count(*) AS count FROM information_schema.columns \
         WHERE column_name = 'roles' AND table_name IN \
         ('organization_memberships', 'sub_tenant_memberships', 'team_memberships', 'invitations')",
    )
    .get_result::<CountRow>(connection)
    .await
    .expect("the catalog must read")
    .count;
    assert_eq!(
        legacy_columns, 4,
        "the rows were written before the migration"
    );

    legacy
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}
