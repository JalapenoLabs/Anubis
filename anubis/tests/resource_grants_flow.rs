//! Resource grants, end to end: an organization shares the machines it owns.
//!
//! The story the issue tells. Team A reaches every machine in the
//! organization and team C reaches only two of them; what each member may do
//! with a machine still comes from their roles. Then the organization's own
//! standing, a member leaving, a grant withdrawn, and a machine deleted, each
//! read back through the routes an application writes and the access screen's
//! listing. The adversarial and concurrency suites take the same fleet apart.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::json;

use support::TestDatabase;
use support::machines::{self, Fleet, KIND};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative, read top to bottom"
)]
async fn an_organization_shares_its_machines_team_by_team() {
    let Some(database) = TestDatabase::create("resource_grants_flow").await else {
        return;
    };
    let fleet = Fleet::open(&database).await;
    let [first, second, third, fourth] = fleet.machines;

    // ------------------------------------------------------------------
    // The access screen lists the kind, the teams, and nothing shared yet.
    // ------------------------------------------------------------------
    let (status, body) = fleet.grants(&fleet.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["kinds"],
        json!([{ "kind": KIND, "reaches": true, "may_manage": true }]),
    );
    assert_eq!(body["teams"].as_array().map(Vec::len), Some(3));
    assert_eq!(body["grants"], json!([]));

    // ------------------------------------------------------------------
    // Team A reaches every machine; team C reaches two.
    // ------------------------------------------------------------------
    assert_eq!(
        fleet.share(&fleet.owner, fleet.team_a, true, &[]).await,
        StatusCode::OK
    );
    assert_eq!(
        fleet
            .share(&fleet.owner, fleet.team_c, false, &[first, second])
            .await,
        StatusCode::OK
    );

    let (_ada_email, ada) = fleet
        .join(("team_id", fleet.team_a), &[fleet.operator])
        .await;
    let (_cleo_email, cleo) = fleet
        .join(("team_id", fleet.team_c), &[fleet.operator])
        .await;
    let (_dex_email, dex) = fleet.join(("team_id", fleet.team_c), &[]).await;

    for machine in fleet.machines {
        let (status, body) = fleet.open_machine(&ada, machine).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "team A reaches every machine: {body}"
        );
    }
    assert_eq!(fleet.open_machine(&cleo, first).await.0, StatusCode::OK);
    assert_eq!(fleet.open_machine(&cleo, second).await.0, StatusCode::OK);
    assert_eq!(
        fleet.open_machine(&cleo, third).await.0,
        StatusCode::NOT_FOUND,
        "team C reaches only what it was granted",
    );

    assert_eq!(
        fleet.listed(&ada).await,
        (
            StatusCode::OK,
            vec![
                "Machine 0".to_owned(),
                "Machine 1".to_owned(),
                "Machine 2".to_owned(),
                "Machine 3".to_owned(),
            ],
        ),
    );
    assert_eq!(
        fleet.listed(&cleo).await,
        (
            StatusCode::OK,
            vec!["Machine 0".to_owned(), "Machine 1".to_owned()],
        ),
    );

    // ------------------------------------------------------------------
    // What a member may do still comes from their roles.
    // ------------------------------------------------------------------
    let rename = json!({ "name": "Renamed" });
    let (status, body) = fleet
        .send(
            "PATCH",
            &format!("/account/machines/{first}"),
            Some(&rename),
            &cleo,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an Operator updates machines: {body}"
    );
    let (status, _body) = fleet
        .send(
            "PATCH",
            &format!("/account/machines/{first}"),
            Some(&rename),
            &dex,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "reaching a machine is not being allowed to change it",
    );
    let (status, _body) = fleet
        .send("DELETE", &format!("/account/machines/{first}"), None, &cleo)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an Operator does not delete");

    // ------------------------------------------------------------------
    // The default role is everybody's and reaches nothing; an organization
    // grant that reads machines reaches every one of them.
    // ------------------------------------------------------------------
    let (olga_email, olga) = fleet
        .join(("organization_id", fleet.organization), &[])
        .await;
    assert_eq!(
        fleet.open_machine(&olga, first).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fleet.listed(&olga).await, (StatusCode::OK, Vec::new()));

    let viewer = fleet.create_role("Viewer", &["Machine.read"]).await;
    let membership = fleet.organization_membership(&olga_email).await;
    let (status, body) = fleet
        .send(
            "PATCH",
            &format!(
                "/tenancy/organizations/{}/members/{membership}",
                fleet.organization
            ),
            Some(&json!({ "role_ids": [viewer] })),
            &fleet.owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(fleet.open_machine(&olga, fourth).await.0, StatusCode::OK);
    assert_eq!(fleet.listed(&olga).await.1.len(), 4);
    let (status, body) = fleet.grants(&olga).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["kinds"][0]["may_manage"], false,
        "reading every machine is not sharing them",
    );

    // ------------------------------------------------------------------
    // The access screen reads back what was shared, by name.
    // ------------------------------------------------------------------
    let (status, body) = fleet.grants(&fleet.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let shared_with_c = body["grants"]
        .as_array()
        .expect("grants are listed")
        .iter()
        .find(|grant| grant["team_id"] == fleet.team_c.to_string())
        .expect("team C's grant is listed")
        .clone();
    assert_eq!(shared_with_c["every"], false);
    assert_eq!(
        shared_with_c["resources"],
        json!([
            { "value": second, "label": "Machine 1" },
            { "value": first, "label": "Renamed" },
        ]),
    );

    // ------------------------------------------------------------------
    // Leaving the team takes the reach it gave.
    // ------------------------------------------------------------------
    let (status, _body) = fleet
        .send(
            "POST",
            &format!("/tenancy/teams/{}/leave", fleet.team_a),
            None,
            &ada,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        fleet.open_machine(&ada, first).await.0,
        StatusCode::NOT_FOUND,
        "a former member reaches nothing the team was granted",
    );
    assert_eq!(fleet.listed(&ada).await.0, StatusCode::NOT_FOUND);

    // ------------------------------------------------------------------
    // Withdrawing a grant is replacing it with nothing, and it is audited.
    // ------------------------------------------------------------------
    assert_eq!(
        fleet.share(&fleet.owner, fleet.team_c, false, &[]).await,
        StatusCode::OK
    );
    assert_eq!(
        fleet.open_machine(&cleo, first).await.0,
        StatusCode::NOT_FOUND
    );

    let mut connection = fleet.harness.pool.get().await.expect("a connection");
    let recorded: Vec<(Option<String>, serde_json::Value)> = {
        use anubis::schema::audit_events;
        audit_events::table
            .filter(audit_events::organization_id.eq(fleet.organization))
            .filter(audit_events::action.eq(anubis::audit::TEAM_ACCESS_CHANGED))
            .order(audit_events::created_at.asc())
            .select((audit_events::subject_label, audit_events::changes))
            .load(&mut connection)
            .await
            .expect("the audit log must read")
    };
    let labels: Vec<Option<&str>> = recorded
        .iter()
        .map(|(label, _changes)| label.as_deref())
        .collect();
    assert_eq!(labels, [Some("Team A"), Some("Team C"), Some("Team C")]);
    assert_eq!(
        recorded[2].1,
        json!({
            KIND: {
                "old": { "every": false, "resources": ["Machine 1", "Renamed"] },
                "new": { "every": false, "resources": [] },
            },
        }),
        "the withdrawal names what the team lost, by name",
    );

    // ------------------------------------------------------------------
    // A deleted machine takes its grants with it, and one added later is not
    // one a team was granted by name.
    // ------------------------------------------------------------------
    assert_eq!(
        fleet
            .share(&fleet.owner, fleet.team_c, false, &[second, third])
            .await,
        StatusCode::OK
    );
    let (status, body) = fleet
        .send(
            "DELETE",
            &format!("/account/machines/{third}"),
            None,
            &fleet.owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(fleet.grant_rows(fleet.team_c).await, 1);
    machines::insert(&fleet.harness.pool, fleet.organization, "Machine 4").await;
    assert_eq!(
        fleet.listed(&cleo).await,
        (StatusCode::OK, vec!["Machine 1".to_owned()]),
    );
}
