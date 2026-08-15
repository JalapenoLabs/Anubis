//! The framework's mounted route surface, for `anubis routes`.
//!
//! Axum routers cannot be introspected after construction, so the framework
//! keeps this manifest alongside them: one entry per route the framework
//! mounts, exactly as a starter application composes them (`/auth`,
//! `/tenancy`, `/billing`, `/developers`, `/api/v1`, the framework's own
//! receiver under `/webhooks`, the public avatar route, and the probes
//! [`crate::server::harden`] adds).
//!
//! Drift cannot happen silently: a unit test in this module composes the real
//! routers and sends a request for every entry, so a route that is renamed,
//! removed, or switched to another method fails the build until the manifest
//! is updated. Routes under `/api/v1` are additionally covered by the OpenAPI
//! document, which the CLI merges into the printed table at runtime.
//!
//! Application-defined routes live in the application's own router and are
//! not visible to the CLI; `anubis scaffold` output registers its routes here
//! only when they are framework-mounted.

/// One framework-mounted route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteEntry {
    /// The HTTP method, uppercase.
    pub method: &'static str,
    /// The mounted path, with `{param}` placeholders.
    pub path: &'static str,
    /// The framework area that serves the route.
    pub area: &'static str,
}

/// Every route the framework mounts, in presentation order.
///
/// # Examples
/// ```
/// let routes = anubis::manifest::framework_routes();
/// assert!(routes.iter().any(|route| route.path == "/auth/login"));
/// ```
#[must_use]
pub fn framework_routes() -> &'static [RouteEntry] {
    FRAMEWORK_ROUTES
}

/// The manifest itself. Update it in the same change as any router edit; the
/// `every_manifest_entry_is_mounted` test fails the build on mismatch.
static FRAMEWORK_ROUTES: &[RouteEntry] = &[
    // Liveness and readiness, mounted by `anubis::server::harden`.
    RouteEntry {
        method: "GET",
        path: "/healthz",
        area: "server",
    },
    RouteEntry {
        method: "GET",
        path: "/readyz",
        area: "server",
    },
    // Public profile pictures.
    RouteEntry {
        method: "GET",
        path: "/users/{user_id}/avatar",
        area: "auth",
    },
    // The realtime channel socket. A plain GET is answered `426 Upgrade
    // Required` by axum once the session is accepted.
    RouteEntry {
        method: "GET",
        path: "/realtime",
        area: "realtime",
    },
    // Registration and sessions. Discovery comes first because a sign-up
    // screen reads it before it offers the form.
    RouteEntry {
        method: "GET",
        path: "/auth/registration",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/register",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/login",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/logout",
        area: "auth",
    },
    RouteEntry {
        method: "GET",
        path: "/auth/me",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/verify-email/request",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/verify-email/confirm",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/password-reset/request",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/password-reset/confirm",
        area: "auth",
    },
    // Profile and account management.
    RouteEntry {
        method: "PATCH",
        path: "/auth/profile",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/profile/avatar",
        area: "auth",
    },
    RouteEntry {
        method: "DELETE",
        path: "/auth/profile/avatar",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/change-password",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/change-email/request",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/change-email/confirm",
        area: "auth",
    },
    RouteEntry {
        method: "GET",
        path: "/auth/sessions",
        area: "auth",
    },
    RouteEntry {
        method: "DELETE",
        path: "/auth/sessions/{session_id}",
        area: "auth",
    },
    RouteEntry {
        method: "DELETE",
        path: "/auth/account",
        area: "auth",
    },
    // Multi-factor authentication.
    RouteEntry {
        method: "GET",
        path: "/auth/mfa",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/mfa/totp/setup",
        area: "auth",
    },
    RouteEntry {
        method: "GET",
        path: "/auth/mfa/totp/qr.svg",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/mfa/totp/confirm",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/mfa/totp/disable",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/mfa/verify",
        area: "auth",
    },
    // Passwordless email sign-in codes.
    RouteEntry {
        method: "POST",
        path: "/auth/email-code/request",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/email-code/verify",
        area: "auth",
    },
    // Passkeys.
    RouteEntry {
        method: "GET",
        path: "/auth/passkeys",
        area: "auth",
    },
    RouteEntry {
        method: "DELETE",
        path: "/auth/passkeys/{passkey_id}",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/passkeys/register/start",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/passkeys/register/finish",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/passkeys/login/start",
        area: "auth",
    },
    RouteEntry {
        method: "POST",
        path: "/auth/passkeys/login/finish",
        area: "auth",
    },
    // OAuth sign-in over OpenID Connect. Discovery answers JSON; the two flow
    // routes are browser navigations and answer with a redirect, to the
    // provider or back to the sign-in page.
    RouteEntry {
        method: "GET",
        path: "/auth/oauth/providers",
        area: "auth",
    },
    RouteEntry {
        method: "GET",
        path: "/auth/oauth/{provider}/start",
        area: "auth",
    },
    RouteEntry {
        method: "GET",
        path: "/auth/oauth/{provider}/callback",
        area: "auth",
    },
    // Tenancy.
    RouteEntry {
        method: "GET",
        path: "/tenancy/memberships",
        area: "tenancy",
    },
    RouteEntry {
        method: "GET",
        path: "/tenancy/teams/{team_id}/members",
        area: "tenancy",
    },
    RouteEntry {
        method: "GET",
        path: "/tenancy/organizations/{organization_id}/members",
        area: "tenancy",
    },
    RouteEntry {
        method: "POST",
        path: "/tenancy/invitations",
        area: "tenancy",
    },
    RouteEntry {
        method: "POST",
        path: "/tenancy/invitations/claim",
        area: "tenancy",
    },
    // Tenancy management.
    RouteEntry {
        method: "POST",
        path: "/tenancy/organizations",
        area: "tenancy",
    },
    RouteEntry {
        method: "PATCH",
        path: "/tenancy/organizations/{organization_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/organizations/{organization_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "POST",
        path: "/tenancy/organizations/{organization_id}/teams",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/organizations/{organization_id}/teams/{team_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/organizations/{organization_id}/members/{membership_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "POST",
        path: "/tenancy/organizations/{organization_id}/leave",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/organizations/{organization_id}/invitations/{invitation_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "PATCH",
        path: "/tenancy/teams/{team_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "POST",
        path: "/tenancy/teams/{team_id}/leave",
        area: "tenancy",
    },
    RouteEntry {
        method: "PATCH",
        path: "/tenancy/teams/{team_id}/members/{membership_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/teams/{team_id}/members/{membership_id}",
        area: "tenancy",
    },
    RouteEntry {
        method: "DELETE",
        path: "/tenancy/teams/{team_id}/invitations/{invitation_id}",
        area: "tenancy",
    },
    // Billing: the plan an organization is on, and the two Stripe redirects.
    RouteEntry {
        method: "GET",
        path: "/billing/organizations/{organization_id}",
        area: "billing",
    },
    RouteEntry {
        method: "POST",
        path: "/billing/organizations/{organization_id}/checkout",
        area: "billing",
    },
    RouteEntry {
        method: "POST",
        path: "/billing/organizations/{organization_id}/portal",
        area: "billing",
    },
    RouteEntry {
        method: "POST",
        path: "/billing/organizations/{organization_id}/reconcile",
        area: "billing",
    },
    // The framework's own incoming receiver, mounted under `/webhooks` beside
    // whatever an application scaffolded for itself.
    RouteEntry {
        method: "POST",
        path: "/webhooks/stripe-billing",
        area: "billing",
    },
    // Developer platform applications.
    RouteEntry {
        method: "GET",
        path: "/developers/teams/{team_id}/platform-applications",
        area: "developers",
    },
    RouteEntry {
        method: "POST",
        path: "/developers/teams/{team_id}/platform-applications",
        area: "developers",
    },
    RouteEntry {
        method: "DELETE",
        path: "/developers/teams/{team_id}/platform-applications/{application_id}",
        area: "developers",
    },
    RouteEntry {
        method: "POST",
        path: "/developers/teams/{team_id}/platform-applications/{application_id}/rotate-token",
        area: "developers",
    },
    // Outgoing webhook subscriptions and their delivery log.
    RouteEntry {
        method: "GET",
        path: "/developers/teams/{team_id}/webhook-endpoints",
        area: "developers",
    },
    RouteEntry {
        method: "POST",
        path: "/developers/teams/{team_id}/webhook-endpoints",
        area: "developers",
    },
    RouteEntry {
        method: "PATCH",
        path: "/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}",
        area: "developers",
    },
    RouteEntry {
        method: "DELETE",
        path: "/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}",
        area: "developers",
    },
    RouteEntry {
        method: "GET",
        path: "/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries",
        area: "developers",
    },
    RouteEntry {
        method: "POST",
        path: "/developers/teams/{team_id}/webhook-endpoints/{endpoint_id}/deliveries/\
                {delivery_id}/redeliver",
        area: "developers",
    },
    // API contract endpoints. The versioned operations themselves come from
    // the OpenAPI document and are merged in by the CLI.
    RouteEntry {
        method: "GET",
        path: "/api/v1/openapi.json",
        area: "api",
    },
    RouteEntry {
        method: "GET",
        path: "/api/v1/docs",
        area: "api",
    },
];

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use diesel_async::AsyncPgConnection;
    use diesel_async::pooled_connection::AsyncDieselConnectionManager;
    use diesel_async::pooled_connection::deadpool::Pool;
    use tower::ServiceExt;

    use super::framework_routes;
    use crate::config::AppConfig;
    use crate::roles::RoleSet;

    const ROLES: &str = "
roles:
  default:
    models:
      Team: [read]
  editor:
    includes: [default]
    models:
      Team: [update]
  billing:
    models: {}
  admin:
    includes: [editor, billing]
    models:
      Team: [manage]
";

    /// The smallest valid plan set: one free plan and nothing sold.
    const PLANS: &str = "
plans:
  - key: free
    name: Free
";

    /// A placeholder for `{param}` segments; requests only need to route.
    const DUMMY_ID: &str = "00000000-0000-0000-0000-000000000000";

    /// Composes the routers exactly as the starter application does, over a
    /// lazy pool pointing at an unreachable address. Requests must route
    /// (anything but 404/405); handlers may then fail on auth or the pool.
    fn composed_router() -> Router {
        let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(
            "postgres://nobody:nobody@127.0.0.1:1/unreachable",
        );
        let pool = Pool::builder(manager)
            .build()
            .expect("building a lazy pool never touches the network");
        let config = AppConfig::from_lookup(|_| None).expect("defaults are valid");
        let roles = RoleSet::from_yaml(ROLES).expect("test roles are valid");
        let mailer = crate::mail::Mailer::log();

        Router::new()
            .merge(crate::server::health_router(pool.clone()))
            .merge(crate::auth::avatar_router(pool.clone()))
            .merge(crate::realtime::router(
                pool.clone(),
                crate::realtime::Channels::in_process(),
            ))
            .nest(
                "/auth",
                crate::auth::router(pool.clone(), mailer.clone(), &config),
            )
            .nest(
                "/tenancy",
                crate::tenancy::router(
                    pool.clone(),
                    mailer,
                    roles.clone(),
                    Some(crate::billing::PlanSet::from_yaml(PLANS).expect("test plans are valid")),
                    &config,
                ),
            )
            .nest(
                "/billing",
                crate::billing::router(
                    pool.clone(),
                    roles.clone(),
                    crate::billing::PlanSet::from_yaml(PLANS).expect("test plans are valid"),
                    &config,
                ),
            )
            .nest(
                "/webhooks",
                crate::billing::webhook_router(pool.clone(), &config),
            )
            .nest(
                "/developers",
                crate::api::management::router(pool.clone(), roles.clone()),
            )
            .nest(
                "/developers",
                crate::webhooks::router(pool.clone(), roles.clone(), &config),
            )
            .nest("/api/v1", crate::api::v1::router(pool.clone()))
            .layer(crate::guard::layer(pool, roles))
    }

    fn concrete_path(path: &str) -> String {
        path.split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    DUMMY_ID
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Sends one request and returns its status, or `None` when the handler
    /// blocked on the unreachable pool. An unmatched route answers 404
    /// instantly, so both a response and a block prove the route is mounted.
    async fn probe(router: Router, request: Request<Body>) -> Option<StatusCode> {
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(2), router.oneshot(request)).await;
        match outcome {
            Err(_) => None,
            Ok(response) => Some(response.expect("the router is infallible").status()),
        }
    }

    #[tokio::test]
    async fn every_manifest_entry_is_mounted() {
        let router = composed_router();
        for entry in framework_routes() {
            let request = Request::builder()
                .method(entry.method)
                .uri(concrete_path(entry.path))
                .body(Body::empty())
                .expect("manifest entries are valid requests");
            let Some(status) = probe(router.clone(), request).await else {
                continue;
            };
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{} {} is in the manifest but not mounted",
                entry.method,
                entry.path,
            );
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{} {} is mounted with a different method than the manifest",
                entry.method,
                entry.path,
            );
        }
    }

    #[tokio::test]
    async fn every_openapi_operation_is_mounted() {
        let document =
            serde_json::to_value(crate::api::v1::openapi()).expect("the document serializes");
        let paths = document["paths"]
            .as_object()
            .expect("the document has paths");

        // A path item also carries non-operation keys such as `parameters`.
        let methods = [
            "get", "put", "post", "delete", "options", "head", "patch", "trace",
        ];

        let router = composed_router();
        for (path, operations) in paths {
            let operations = operations.as_object().expect("path items are objects");
            for method in operations
                .keys()
                .filter(|key| methods.contains(&key.as_str()))
            {
                let method = Method::from_bytes(method.to_uppercase().as_bytes())
                    .expect("OpenAPI methods are valid HTTP methods");
                // Documented paths already carry the /api/v1 prefix.
                let request = Request::builder()
                    .method(method.clone())
                    .uri(concrete_path(path))
                    .body(Body::empty())
                    .expect("documented paths are valid requests");
                let Some(status) = probe(router.clone(), request).await else {
                    continue;
                };
                assert_ne!(
                    status,
                    StatusCode::NOT_FOUND,
                    "{method} {path} is documented but not mounted",
                );
            }
        }
    }
}
