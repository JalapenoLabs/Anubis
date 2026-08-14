//! The `HypotheticalSenderWebhook` receiver, end to end against a real Postgres
//! database.
//!
//! One narrative covers everything a scaffolded webhook receiver owes its
//! application: a posted event is stored with the headers that explain it, a
//! processing job is queued in the same transaction, the signature check
//! accepts what the provider signed and refuses everything else, and a body
//! that is not JSON is refused rather than stored as something unreadable.
//! When `anubis scaffold webhook` transforms the living template, it transforms
//! this narrative with it, so every generated receiver arrives with the same
//! proof.
//!
//! This narrative reads the table directly, unlike every other one, because a
//! received webhook is stored for the application to process rather than served
//! back to anybody.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::jobs::Job;
use anubis::schema::jobs;
use anubis::webhooks::signature;
use anubis_starter::scaffolding::hypothetically_remote::{
    HypotheticalSenderWebhook, ProcessHypotheticalSenderWebhook, SIGNATURE_HEADER, verify_signature,
};
use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::{boot, send};
use tower::ServiceExt;
use uuid::Uuid;

/// Where Hypothetical Sender posts its events.
const ENDPOINT: &str = "/webhooks/hypothetical-sender";

/// A secret only this test knows, standing in for the deployment's own.
const SECRET: &str = "whsec_hypothetical_sender_fixture";

#[tokio::test]
async fn a_hypothetical_sender_webhook_is_stored_and_queued_for_processing() {
    let Some((router, _outbox)) = boot().await else {
        eprintln!("skipping hypothetical_sender_webhooks_flow test: DATABASE_URL is not set");
        return;
    };
    let pool = support::pool().await;
    let mut connection = pool.get().await.expect("a pooled connection");

    let run = Uuid::new_v4();
    let event = json!({
        "id": format!("evt_{run}"),
        "type": "hypothetical.thing.happened",
        "data": { "amount": 4_200 },
    });

    // A provider posts an event, with no session and no bearer token.
    let (status, body) = send(&router, "POST", ENDPOINT, Some(&event), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let stored_id = body["id"]
        .as_str()
        .and_then(|id| id.parse::<Uuid>().ok())
        .expect("the response names the row it stored");

    // The row holds the request: the payload byte for byte, the headers that
    // explain where it came from, and nothing decided about it yet.
    let record = HypotheticalSenderWebhook::find(&mut connection, stored_id)
        .await
        .expect("the webhook must be stored");
    assert_eq!(record.payload, event);
    assert_eq!(record.headers["content-type"], json!("application/json"));
    assert!(
        record.processed_at.is_none(),
        "nothing has processed it yet"
    );
    assert!(record.error.is_none());
    // No signing secret is configured in this test process, so the check cannot
    // pass. It is recorded rather than enforced: the row is evidence either way.
    assert!(!record.verified);

    // The job was queued in the same transaction as the row, so the two can
    // never disagree about whether the event will be dealt with.
    let queued: Vec<(String, Value)> = jobs::table
        .filter(jobs::kind.eq(ProcessHypotheticalSenderWebhook::KIND))
        .select((jobs::queue, jobs::payload))
        .load(&mut connection)
        .await
        .expect("the jobs table must be readable");
    let (queue, payload) = queued
        .into_iter()
        .find(|(_queue, payload)| payload["webhook_id"] == json!(stored_id))
        .expect("storing a webhook must queue the job that processes it");
    assert_eq!(queue, "incoming_webhooks", "its own queue, not the default");
    assert_eq!(payload["webhook_id"], json!(stored_id));

    // The signature check accepts what the provider signed, and refuses a
    // request signed with the wrong secret, over the wrong bytes, or not at all.
    let body = serde_json::to_vec(&event).expect("the event serializes");
    let signed = signature::sign_hmac_sha256(SECRET, &body);
    assert!(verify_signature(SECRET, &signature_headers(&signed), &body));
    assert!(!verify_signature(
        "whsec_elsewhere",
        &signature_headers(&signed),
        &body
    ));
    assert!(!verify_signature(
        SECRET,
        &signature_headers(&signed),
        b"{}"
    ));
    assert!(!verify_signature(SECRET, &HeaderMap::new(), &body));

    // A body the payload column cannot hold is refused rather than stored as
    // something nothing can read: every provider sends JSON, so this is a
    // misconfigured endpoint rather than an event.
    let (status, body) = post_raw(&router, "not json at all").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["message"], json!("Expected a JSON body."));
}

/// A header map carrying one signature, the way the provider sends it.
fn signature_headers(signed: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        SIGNATURE_HEADER,
        HeaderValue::from_str(signed).expect("a hex signature is a valid header value"),
    );
    headers
}

/// Posts a raw body, which [`send`] cannot do because it serializes JSON.
async fn post_raw(router: &axum::Router, body: &'static str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(ENDPOINT)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();

    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
