//! Plan limits: what a plan allows, and what happens at the ceiling.
//!
//! A plan's limits live in `config/billing.yml` (see [`crate::billing::plans`])
//! and mean nothing until something checks them. [`Limits`] is that something:
//! it resolves the plan an organization is on and answers whether one more
//! record fits.
//!
//! ```ignore
//! let limits = anubis::billing::Limits::new(plans.clone());
//!
//! // At the choke point, before the insert:
//! let held = Project::count_for_team(&mut connection, team.id).await?;
//! limits.check(&mut connection, organization_id, "projects", held).await?;
//! ```
//!
//! # Hard and soft
//!
//! A limit is hard unless `billing.yml` marks it soft. A hard limit that is
//! reached refuses the creation with `409 Conflict` and a message naming the
//! plan, which is a message worth upgrading over. A soft limit never refuses:
//! [`Limits::check`] returns `Ok`, the record is created, and the screen is
//! left to report that the organization is over. That split is Bullet Train's,
//! and the reasoning is the same: some ceilings are a product decision and some
//! are a conversation.
//!
//! # Seats are the framework's; everything else is the application's
//!
//! [`SEATS`] is the one limit the framework meters and enforces itself, at the
//! one place a person joins an organization: creating an invitation. It has to
//! be, because memberships are the framework's own table and no application
//! code runs there.
//!
//! Every other limit names something only the application can count, so the
//! application calls [`Limits::check`] at its own creation choke points. The
//! scaffolder will eventually write that call for a model whose limit name is
//! its snake-case plural; until then it is one line in a generated handler.
//! See `docs/scaffolding.md`.
//!
//! # A seat is a person, counted once
//!
//! [`seats_used`] counts the distinct email addresses that can reach an
//! organization: everyone holding an organization membership, everyone holding
//! a claimed membership in one of its teams, and every invitation still
//! claimable. Counting addresses rather than rows is what keeps one person who
//! belongs to three teams from paying for three seats, and what makes
//! re-inviting somebody who is already a member free.
//!
//! An expired invitation holds no seat: it can no longer be claimed, so
//! charging for it would bill for nothing and block an admin who cannot see
//! why.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::lifecycle::SyncSeats;
use super::model::Subscription;
use super::plans::{Plan, PlanSet};
use crate::http::ApiError;
use crate::schema::{invitations, organization_memberships, team_memberships, teams, users};

/// The limit naming how many people may reach an organization.
///
/// The framework's own vocabulary, and the only limit name it reserves: it is
/// enforced at invitation creation and it is the quantity a per-seat price is
/// bought with. An application is free to meter anything else it likes.
pub const SEATS: &str = "seats";

/// Resolves and enforces an organization's plan limits.
///
/// Every method takes the caller's own connection rather than a pool, which is
/// what lets a check run inside the transaction that is about to insert: a
/// limit read outside the write it guards is a limit two requests can pass at
/// once. Cloning is cheap; the plans are behind a handle.
#[derive(Clone)]
pub struct Limits {
    plans: Arc<PlanSet>,
}

impl Limits {
    /// Builds the limit surface over an application's validated plans.
    #[must_use]
    pub fn new(plans: PlanSet) -> Self {
        Self {
            plans: Arc::new(plans),
        }
    }

    /// The plans these limits come from.
    #[must_use]
    pub fn plans(&self) -> &PlanSet {
        &self.plans
    }

    /// The plan an organization is on right now.
    ///
    /// Total, like every other plan resolution here: a subscription that grants
    /// access names its plan, and everything else, including no subscription at
    /// all, is the free plan.
    ///
    /// # Errors
    /// Returns an [`Error`] when the subscription cannot be read.
    pub async fn plan_for(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> Result<&Plan, Error> {
        let subscription =
            Subscription::current_for_organization(connection, organization_id).await?;

        let Some(subscription) = subscription.filter(Subscription::grants_access) else {
            return Ok(self.plans.free());
        };

        Ok(self
            .plans
            .find(&subscription.plan_key)
            .unwrap_or_else(|| self.plans.free()))
    }

    /// Refuses one more record when a hard limit is already reached.
    ///
    /// `held` is how many the organization has now; the question this answers
    /// is whether one more fits. A limit the plan does not name is unlimited, a
    /// soft limit never refuses, and a hard limit refuses once `held` has
    /// reached its count.
    ///
    /// # Errors
    /// Returns an [`Error`] that [`Error::is_exceeded`] reports as a hard limit
    /// reached, which becomes a `409` with a message naming the plan, or a
    /// database failure, which becomes a `500`.
    pub async fn check(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        limit: &str,
        held: i64,
    ) -> Result<(), Error> {
        let plan = self.plan_for(connection, organization_id).await?;
        enforce(plan, limit, held.saturating_add(1))
    }

    /// How many seats the organization is using, claimed and invited alike.
    ///
    /// # Errors
    /// Returns an [`Error`] when the count cannot be read.
    pub async fn seats_used(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> Result<i64, Error> {
        Ok(seats_used(connection, organization_id).await?)
    }

    /// Refuses an invitation that would put an organization over its seats.
    ///
    /// `joining` is the address being invited, normalized. It is counted into
    /// the seats first, so re-inviting somebody who already holds a seat is
    /// free and the check is about the organization's resulting size rather
    /// than about the request.
    ///
    /// # Errors
    /// Returns an [`Error`] that [`Error::is_exceeded`] reports when the seats
    /// limit is hard and reached, or a database failure.
    pub async fn check_seats(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        joining: &str,
    ) -> Result<(), Error> {
        let plan = self.plan_for(connection, organization_id).await?;
        if plan.limit(SEATS).is_none() {
            // Unlimited seats, so the three queries below would be counted for
            // nothing. The common case on a paid plan.
            return Ok(());
        }

        let mut holders = seat_holders(connection, organization_id).await?;
        holders.insert(joining.to_owned());

        enforce(plan, SEATS, holders.len().try_into().unwrap_or(i64::MAX))
    }

    /// Queues the seat-count update Stripe needs after a membership change.
    ///
    /// A no-op unless some plan sells a per-seat price, because a flat-priced
    /// application has no quantity at Stripe for a membership to move. The
    /// insert rides `connection`, so a change that rolls back queues nothing.
    ///
    /// # Errors
    /// Returns the underlying Diesel error when the insert fails.
    pub async fn queue_seat_sync(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> QueryResult<()> {
        if !self.plans.sells_per_seat() {
            return Ok(());
        }

        crate::jobs::enqueue_query(connection, &SyncSeats { organization_id }).await?;
        Ok(())
    }
}

impl fmt::Debug for Limits {
    /// Names the plan count rather than every plan, which would be noise.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Limits")
            .field("plans", &self.plans.plans().len())
            .finish_non_exhaustive()
    }
}

/// Answers whether `resulting` records are allowed on `plan`.
///
/// One function for both entry points, so the hard-versus-soft decision and the
/// message a customer reads are written once.
fn enforce(plan: &Plan, limit: &str, resulting: i64) -> Result<(), Error> {
    let Some(allowed) = plan.limit(limit) else {
        return Ok(());
    };
    if resulting <= allowed.count() || !allowed.is_hard() {
        return Ok(());
    }

    Err(Error::new(
        ErrorKind::Exceeded,
        format!(
            "The {} plan's {limit} limit of {} is reached. Upgrade to add more.",
            plan.name(),
            allowed.count(),
        ),
    ))
}

/// How many seats an organization is using.
///
/// The free function behind [`Limits::seats_used`], so the seat-synchronizing
/// job can count without holding a [`Limits`] of its own.
///
/// # Errors
/// Returns the underlying Diesel error when a count query fails.
pub(super) async fn seats_used(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<i64> {
    let holders = seat_holders(connection, organization_id).await?;

    Ok(holders.len().try_into().unwrap_or(i64::MAX))
}

/// Every address that can reach the organization, each one once.
///
/// Three reads rather than one union query: each is a plain Diesel select over
/// an indexed column, and the set is what deduplicates a person who holds
/// several memberships. Addresses are normalized everywhere they are written
/// (`validate_email` lowercases and trims), so they compare as stored.
async fn seat_holders(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<BTreeSet<String>> {
    let organization_members: Vec<String> = organization_memberships::table
        .inner_join(users::table)
        .filter(organization_memberships::organization_id.eq(organization_id))
        .select(users::email)
        .load(connection)
        .await?;

    // Claimed team memberships only: an unclaimed one is the placeholder an
    // invitation pre-created, and the invitation below is what counts it.
    let team_members: Vec<String> = team_memberships::table
        .inner_join(teams::table)
        .inner_join(users::table)
        .filter(teams::organization_id.eq(organization_id))
        .select(users::email)
        .load(connection)
        .await?;

    let invited: Vec<String> = invitations::table
        .filter(invitations::organization_id.eq(organization_id))
        .filter(invitations::expires_at.gt(Utc::now()))
        .select(invitations::email)
        .load(connection)
        .await?;

    Ok(organization_members
        .into_iter()
        .chain(team_members)
        .chain(invited)
        .collect())
}

/// Why a limit could not be honored.
#[derive(Debug)]
enum ErrorKind {
    /// A hard limit is reached, and the caller should be told to upgrade.
    Exceeded,
    /// The database refused a read the check needed.
    Database,
}

/// A limit that refused, or a read that failed on the way to asking.
///
/// [`Error::is_exceeded`] separates the two, and the [`From`] into
/// [`ApiError`] applies it: a reached limit is a `409` carrying the message
/// verbatim, because it names the plan and the number and is exactly what a
/// customer needs to read. Anything else is a `500` with the detail logged.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// Returns `true` when a hard limit refused, rather than a query failing.
    #[must_use]
    pub fn is_exceeded(&self) -> bool {
        matches!(self.kind, ErrorKind::Exceeded)
    }

    /// The message, which for a reached limit is written for a customer.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            backtrace: Backtrace::capture(),
        }
    }
}

impl From<diesel::result::Error> for Error {
    fn from(source: diesel::result::Error) -> Self {
        Self::new(
            ErrorKind::Database,
            format!("a limit could not be read: {source}"),
        )
    }
}

impl From<Error> for ApiError {
    /// Renders a limit failure as the answer a caller acts on.
    ///
    /// A reached limit is a `409`: the request is well formed and only the
    /// organization's current plan refuses it, which is what `409` says.
    fn from(source: Error) -> Self {
        if source.is_exceeded() {
            return Self::conflict(source.message);
        }

        tracing::error!(
            error.message = source.message,
            "a plan limit check failed: {{error.message}}",
        );
        Self::internal()
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{Error, ErrorKind, SEATS, enforce};
    use crate::billing::PlanSet;
    use crate::http::ApiError;

    const PLANS: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: 2
      projects: 3
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
    limits:
      projects:
        count: 10
        enforcement: soft
";

    fn plans() -> PlanSet {
        PlanSet::from_yaml(PLANS).expect("the plans must parse")
    }

    #[test]
    fn a_hard_limit_allows_up_to_its_count_and_refuses_past_it() {
        let plans = plans();
        let free = plans.free();

        for resulting in [1, 2] {
            assert!(
                enforce(free, SEATS, resulting).is_ok(),
                "{resulting} seats must be allowed when 2 are",
            );
        }
        let refused = enforce(free, SEATS, 3).expect_err("the third seat is refused");
        assert!(refused.message().contains("seats"), "got: {refused}");

        let refused = enforce(free, "projects", 4).expect_err("the fourth project is refused");
        assert!(refused.is_exceeded());
        assert!(refused.message().contains("Free"), "got: {refused}");
        assert!(refused.message().contains('3'), "got: {refused}");
        assert!(refused.message().contains("Upgrade"), "got: {refused}");
    }

    #[test]
    fn a_soft_limit_never_refuses() {
        let plans = plans();
        let pro = plans.find("pro").expect("pro must be defined");

        assert!(
            enforce(pro, "projects", 11).is_ok(),
            "a soft limit lets the record through and leaves the screen to warn",
        );
    }

    #[test]
    fn an_unnamed_limit_is_unlimited() {
        let plans = plans();
        let pro = plans.find("pro").expect("pro must be defined");

        assert!(
            enforce(pro, SEATS, 10_000).is_ok(),
            "pro names no seat limit, so it has none",
        );
        assert!(
            enforce(plans.free(), "webhooks", 10_000).is_ok(),
            "free names no webhook limit either",
        );
    }

    #[test]
    fn a_reached_limit_is_a_conflict_and_a_failed_read_is_not() {
        let refused: ApiError = enforce(plans().free(), "projects", 9)
            .expect_err("the ninth project is refused")
            .into();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        assert!(refused.message().contains("Free"), "the plan is named");

        let failed: ApiError = Error::new(ErrorKind::Database, "the socket closed").into();
        assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !failed.message().contains("socket"),
            "the cause belongs in the log, not the body",
        );
    }

    #[test]
    fn a_plan_that_allows_none_of_something_refuses_the_first() {
        let plans = PlanSet::from_yaml(
            "
plans:
  - key: free
    name: Free
    limits:
      projects: 0
",
        )
        .expect("the plans must parse");

        let refused = enforce(plans.free(), "projects", 1).expect_err("zero means zero");
        assert!(refused.is_exceeded());
    }
}
