//! The prepaid credit wallet: a balance per organization, and the ledger it is the sum of.
//!
//! A usage-priced application sells credit up front and spends it as usage
//! accrues. Customers top up through Stripe Checkout, or let auto-recharge
//! charge a saved card when the balance falls below a threshold they choose;
//! the application charges for usage with [`Wallets::spend`] from its own
//! metering; Stripe stays the record of every payment and refund.
//!
//! ```ignore
//! let wallets = anubis::billing::Wallets::new(plans.clone(), &config);
//!
//! // Inside the transaction that records an hour of a server's usage:
//! let spent = wallets
//!     .spend(&mut connection, organization_id, Spend {
//!         amount_micros: 87_500,
//!         key: &format!("server:{server_id}:hour:{hour}"),
//!         description: Some("Server alpha, one hour awake"),
//!         subject: Some(&Subject::new("Server", server_id)),
//!     })
//!     .await?;
//! if spent.uncovered_micros() > 0 {
//!     // The wallet ran dry part way through this hour: stop the server.
//! }
//! ```
//!
//! # Amounts
//!
//! Every amount is an integer number of micro-units of the wallet's currency
//! (1 USD is 1,000,000), so a charge for one minute never rounds to zero. The
//! unit is spelled into every name that carries one: `balance_micros`,
//! `amount_micros`. Stripe charges in the currency's smallest unit, and
//! [`crate::billing::currency`] is the one place the two meet.
//!
//! # The ledger is the truth, the balance its cache
//!
//! `wallet_entries` is append-only: credits positive, spends and refunds
//! negative, each carrying the balance it left. `wallets.balance_micros` is
//! their sum, moved in the same transaction as each entry under the wallet's
//! row lock, so a concurrent spend and top-up are ordered rather than either
//! losing the other's update.
//!
//! # A spend drains to zero, and never below
//!
//! A spend the balance covers takes its amount. A spend larger than the
//! balance takes what is left, leaving exactly zero, and says how much it
//! could not cover; one made at zero or below is refused with nothing
//! written. Metering charges for usage that already happened, so refusing it
//! whole would leave a remainder no charge ever fits, and an organization
//! holding a fraction of a cent would count as paying forever. The
//! application learns the wallet ran dry from the spend itself, in the same
//! call, which is the earliest anything could tell it: a positive
//! [`Spent::uncovered_micros`], or a refusal that [`Error::is_insufficient`]
//! reports. That is where it stops usage.
//!
//! Only a refund takes the balance below zero: the money has already left at
//! Stripe, so the debit cannot be refused, and the organization's next credit
//! pays it back first.
//!
//! # Retries
//!
//! A spend names its own idempotency key, unique per organization, so a meter
//! that retries after a lost answer charges once: the same key and amount
//! answers with the entry already written, and the same key with another
//! amount is refused as a conflict. A refusal for lack of credit writes
//! nothing, so retrying it after a top-up charges then.

mod ledger;
mod model;
mod notices;
pub(super) mod payments;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Debug, Display, Formatter};
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use self::ledger::NewEntry;
use super::model::Subject;
use super::plans::{PlanSet, WalletTerms};
use super::{QUEUE, currency};
use crate::audit;
use crate::config::AppConfig;
use crate::http::ApiError;
use crate::jobs::Job;
use crate::schema::wallets;

#[doc(inline)]
pub use model::{EntryKind, Wallet, WalletEntry};

/// The longest idempotency key a spend may carry, Stripe's own ceiling.
const MAX_KEY_LENGTH: usize = 255;

/// How long an armed recharge may stay unresolved before a spend arms it again.
///
/// The recharge job retries on the queue's own backoff for about ten minutes;
/// an attempt armed for an hour has been dead-lettered, or is waiting on a
/// payment Stripe is still processing. Arming it again re-runs the same
/// attempt, which reads Stripe back before it sends anything, so it can never
/// charge twice.
const RECHARGE_STALE_AFTER: TimeDelta = TimeDelta::hours(1);

/// The background job that charges a wallet's saved card.
///
/// Queued by the spend that leaves the balance below the auto-recharge
/// threshold, in that spend's transaction, which also arms the attempt it
/// will run; and by turning auto-recharge on below the threshold. The payload
/// is the organization alone: the attempt, its amount, and its key are on the
/// wallet row.
#[derive(Debug, Serialize, Deserialize)]
pub struct RechargeWallet {
    /// The organization whose wallet to recharge.
    pub organization_id: Uuid,
}

impl Job for RechargeWallet {
    /// Namespaced, for the same reason [`super::ProcessStripeEvent`] is.
    const KIND: &'static str = "anubis.billing.recharge_wallet";
    const QUEUE: &'static str = QUEUE;
}

/// Spends an organization's credit, and reads its balance.
///
/// Every method takes the caller's own connection, which is what lets a spend
/// commit with the application's own record of the usage it charges for: a
/// rolled-back meter spends nothing. Keep the transaction short, because a
/// spend holds the wallet's row lock until it commits. Cloning is cheap; the
/// plans are behind a handle.
#[derive(Clone)]
pub struct Wallets {
    plans: Arc<PlanSet>,
    /// Whether `STRIPE_SECRET_KEY` is set, without which nothing recharges.
    billing_enabled: bool,
}

/// One charge against the wallet, as the application's metering computed it.
#[derive(Debug, Clone, Copy)]
pub struct Spend<'a> {
    /// What the usage cost, in micro-units; above zero.
    pub amount_micros: i64,
    /// The caller's idempotency key, unique per organization: the same key is
    /// the same charge, however many times it is retried. At most 255
    /// characters; derive it from what is being charged for, such as a record
    /// and the hour it ran.
    pub key: &'a str,
    /// What the charge is for, as the ledger screen shows it.
    pub description: Option<&'a str>,
    /// The record the usage belongs to, named the way the audit log names one.
    pub subject: Option<&'a Subject>,
}

/// What a spend did: the entry it wrote, or the one an earlier attempt wrote.
#[derive(Debug, Clone)]
pub struct Spent {
    entry: WalletEntry,
    replayed: bool,
}

impl Spent {
    /// The ledger entry recording the spend.
    #[must_use]
    pub fn entry(&self) -> &WalletEntry {
        &self.entry
    }

    /// What the spend took, in micro-units.
    #[must_use]
    pub fn charged_micros(&self) -> i64 {
        -self.entry.amount_micros
    }

    /// What the spend asked for and the wallet could not cover.
    ///
    /// Above zero only when the wallet ran dry with this spend, which left the
    /// balance at exactly zero. This is the application's cue to stop usage.
    #[must_use]
    pub fn uncovered_micros(&self) -> i64 {
        self.entry
            .requested_micros
            .unwrap_or_default()
            .saturating_add(self.entry.amount_micros)
    }

    /// The balance the spend left, in micro-units.
    #[must_use]
    pub fn balance_micros(&self) -> i64 {
        self.entry.balance_after_micros
    }

    /// Whether an earlier attempt under the same key already wrote the entry.
    #[must_use]
    pub fn is_replay(&self) -> bool {
        self.replayed
    }
}

impl Wallets {
    /// Builds the wallet surface over an application's plans and configuration.
    #[must_use]
    pub fn new(plans: PlanSet, config: &AppConfig) -> Self {
        Self {
            plans: Arc::new(plans),
            billing_enabled: config.stripe.is_some(),
        }
    }

    /// Charges the organization's credit for usage.
    ///
    /// Drains to zero and never below: see the [module docs](self). When the
    /// spend leaves the balance below the low-balance mark it tells the billing
    /// members, and when it leaves it below the auto-recharge threshold it arms
    /// a recharge, once per dip, both in the caller's transaction.
    ///
    /// # Errors
    /// Returns an [`Error`] that [`Error::is_insufficient`] reports when the
    /// balance is already zero or below, [`Error::is_conflict`] when the key
    /// was used for a different spend, [`Error::is_invalid`] for an amount
    /// below one or a key that is empty or too long, [`Error::is_unconfigured`]
    /// when `billing.yml` sells no credit, and a database failure otherwise.
    pub async fn spend(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
        spend: Spend<'_>,
    ) -> Result<Spent, Error> {
        let terms = self.terms()?;
        if spend.amount_micros < 1 {
            return Err(Error::invalid(format!(
                "a spend charges at least one micro-unit, not {}",
                spend.amount_micros,
            )));
        }
        if spend.key.trim().is_empty() || spend.key.len() > MAX_KEY_LENGTH {
            return Err(Error::invalid(format!(
                "a spend's idempotency key is 1 to {MAX_KEY_LENGTH} characters",
            )));
        }

        connection
            .transaction::<_, Error, _>(async |connection| {
                let Some(mut wallet) = ledger::lock(connection, organization_id).await? else {
                    return Err(Error::insufficient(organization_id, 0));
                };
                if let Some(entry) =
                    WalletEntry::find(connection, organization_id, EntryKind::Spend, spend.key)
                        .await?
                {
                    return replay(entry, &spend);
                }
                if wallet.balance_micros <= 0 {
                    return Err(Error::insufficient(organization_id, wallet.balance_micros));
                }

                let before = wallet.balance_micros;
                let charged = spend.amount_micros.min(before);
                let entry = ledger::append(
                    connection,
                    &mut wallet,
                    NewEntry {
                        kind: EntryKind::Spend,
                        amount_micros: -charged,
                        requested_micros: Some(spend.amount_micros),
                        key: spend.key,
                        payment_intent: None,
                        description: spend.description,
                        subject: spend.subject,
                    },
                )
                .await?;

                self.after_debit(connection, &wallet, before, terms).await?;
                Ok(Spent {
                    entry,
                    replayed: false,
                })
            })
            .await
    }

    /// The organization's balance in micro-units, zero when it has no wallet.
    ///
    /// # Errors
    /// Returns an [`Error`] when the read fails.
    pub async fn balance(
        &self,
        connection: &mut AsyncPgConnection,
        organization_id: Uuid,
    ) -> Result<i64, Error> {
        Ok(balance_of(connection, organization_id).await?)
    }

    /// The wallet's terms, or the honest answer that this application sells no credit.
    fn terms(&self) -> Result<&WalletTerms, Error> {
        self.plans.wallet().ok_or_else(Error::unconfigured)
    }

    /// What a debit owes the people and the recharge behind the wallet.
    ///
    /// The balance fell from `before` to the wallet's. Crossing the low mark
    /// tells the billing members, unless auto-recharge is on, because then the
    /// recharge is the answer and a failed one says so itself. Landing below
    /// the threshold arms a recharge unless one is already in flight.
    async fn after_debit(
        &self,
        connection: &mut AsyncPgConnection,
        wallet: &Wallet,
        before: i64,
        terms: &WalletTerms,
    ) -> Result<(), Error> {
        let after = wallet.balance_micros;
        match wallet.auto_recharge() {
            Some((threshold, _amount)) if self.billing_enabled && after < threshold => {
                arm_recharge(connection, wallet).await?;
            }
            Some(_settings) => {}
            None => {
                if let Some(low) = terms.low_balance_micros()
                    && before >= low
                    && after < low
                {
                    notices::low_balance(
                        connection,
                        wallet.organization_id,
                        after,
                        terms.currency(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Wallets {
    /// Names whether credit is sold rather than every plan, which would be noise.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wallets")
            .field("wallet", &self.plans.wallet().is_some())
            .field("billing_enabled", &self.billing_enabled)
            .finish_non_exhaustive()
    }
}

/// Answers a spend whose key is already written.
fn replay(entry: WalletEntry, spend: &Spend<'_>) -> Result<Spent, Error> {
    let same_subject = entry.subject_type.as_deref() == spend.subject.map(Subject::kind)
        && entry.subject_id == spend.subject.map(Subject::id);
    if entry.requested_micros != Some(spend.amount_micros) || !same_subject {
        return Err(Error::conflict(format!(
            "the spend key {:?} was already used for a different spend; a key names one charge",
            spend.key,
        )));
    }
    Ok(Spent {
        entry,
        replayed: true,
    })
}

/// Arms a recharge on a wallet below its threshold, unless one is already in flight.
///
/// Runs under the wallet's lock, inside the transaction that took the balance
/// below the threshold, so of every spend landing there at once exactly one
/// finds the wallet unarmed: one dip, one charge. A recharge armed longer than
/// [`RECHARGE_STALE_AFTER`] ago is queued again under the same attempt, which
/// reads Stripe back before it charges anything.
pub(super) async fn arm_recharge(
    connection: &mut AsyncPgConnection,
    wallet: &Wallet,
) -> Result<(), Error> {
    let Some((_threshold, amount)) = wallet.auto_recharge() else {
        return Ok(());
    };
    let now = Utc::now();
    match wallet.recharge_queued_at {
        Some(queued_at) if now - queued_at < RECHARGE_STALE_AFTER => return Ok(()),
        Some(_stale) => {
            diesel::update(wallets::table.find(wallet.organization_id))
                .set(wallets::recharge_queued_at.eq(now))
                .execute(connection)
                .await?;
        }
        None => {
            diesel::update(wallets::table.find(wallet.organization_id))
                .set((
                    wallets::recharge_attempt_id.eq(Uuid::new_v4()),
                    wallets::recharge_attempt_amount_micros.eq(amount),
                    wallets::recharge_armed_at.eq(now),
                    wallets::recharge_queued_at.eq(now),
                ))
                .execute(connection)
                .await?;
        }
    }

    crate::jobs::enqueue_query(
        connection,
        &RechargeWallet {
            organization_id: wallet.organization_id,
        },
    )
    .await?;
    Ok(())
}

/// The organization's balance in micro-units, zero when it has no wallet.
///
/// # Errors
/// Returns the underlying Diesel error when the query fails.
pub(super) async fn balance_of(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<i64> {
    Ok(wallets::table
        .find(organization_id)
        .select(wallets::balance_micros)
        .first::<i64>(connection)
        .await
        .optional()?
        .unwrap_or_default())
}

/// Turns auto-recharge on or changes it (`Some((threshold, amount))`), or turns it off.
///
/// Records the change in the audit log, attributed by `context`, and arms a
/// recharge at once when the balance is already below the new threshold.
///
/// # Errors
/// Returns an [`Error`] that is [`Error::is_invalid`] for an amount Stripe
/// cannot charge, below the wallet's minimum top-up, or below the threshold,
/// which is what guarantees one recharge lifts a spent-down wallet back over
/// it; [`Error::is_conflict`] when turning it on without a saved card; and a
/// database failure otherwise.
pub(super) async fn set_auto_recharge(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    terms: &WalletTerms,
    organization_id: Uuid,
    settings: Option<(i64, i64)>,
) -> Result<Wallet, Error> {
    if let Some((threshold, amount)) = settings {
        validate_auto_recharge(terms, threshold, amount)?;
    }

    connection
        .transaction::<_, Error, _>(async |connection| {
            let wallet = ledger::lock_or_create(connection, organization_id).await?;
            if settings.is_some() && wallet.recharge_payment_method_id.is_none() {
                return Err(Error::conflict(
                    "Auto-recharge charges a saved card. Top up once and save the card first.",
                ));
            }
            let before = wallet.auto_recharge();
            let updated: Wallet = diesel::update(wallets::table.find(organization_id))
                .set((
                    wallets::auto_recharge_threshold_micros
                        .eq(settings.map(|(threshold, _amount)| threshold)),
                    wallets::auto_recharge_amount_micros
                        .eq(settings.map(|(_threshold, amount)| amount)),
                    wallets::recharge_failed_at.eq(None::<chrono::DateTime<Utc>>),
                    wallets::recharge_failure.eq(None::<String>),
                ))
                .returning(Wallet::as_returning())
                .get_result(connection)
                .await?;

            if before != settings {
                notices::record_settings(
                    connection,
                    context,
                    audit::WALLET_AUTO_RECHARGE_CHANGED,
                    organization_id,
                    before,
                    settings,
                )
                .await?;
            }
            if let Some((threshold, _amount)) = settings
                && updated.balance_micros < threshold
            {
                arm_recharge(connection, &updated).await?;
            }
            Ok(updated)
        })
        .await
}

/// Refuses auto-recharge settings that could not work, before anything is written.
fn validate_auto_recharge(terms: &WalletTerms, threshold: i64, amount: i64) -> Result<(), Error> {
    if threshold < 0 {
        return Err(Error::invalid("The threshold cannot be below zero."));
    }
    if amount < terms.minimum_top_up_micros() {
        return Err(Error::invalid(format!(
            "A recharge buys at least {}, the smallest top-up.",
            currency::display(terms.minimum_top_up_micros(), terms.currency()),
        )));
    }
    if amount < threshold {
        return Err(Error::invalid(
            "A recharge must buy at least the threshold, so one recharge lifts the balance back \
             over it.",
        ));
    }
    if currency::to_minor(amount, terms.currency()).is_none() {
        return Err(Error::invalid(format!(
            "Stripe cannot charge {} micro-units of {}: use whole cents, at most {} of them.",
            amount,
            terms.currency(),
            currency::MAX_CHARGE_MINOR,
        )));
    }
    Ok(())
}

/// What went wrong with a wallet.
#[derive(Debug)]
enum ErrorKind {
    /// `billing.yml` sells no credit.
    Unconfigured,
    /// The request is one that can never succeed as written.
    Invalid,
    /// The balance is zero or below, so a spend has nothing to take.
    Insufficient {
        /// The balance when the spend was refused.
        balance_micros: i64,
    },
    /// The request disagrees with what is already written.
    Conflict,
    /// The database refused, or the framework broke an invariant of its own.
    Internal,
}

/// A wallet operation that could not be done.
///
/// The `is_*` methods say which, and the [`From`] into [`ApiError`] applies
/// them, so an application's handler forwards one with `?`: insufficient is
/// `402`, invalid `400`, a conflict `409`, unconfigured `404`, and anything
/// else `500` with the detail in the log.
pub struct Error {
    kind: ErrorKind,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// Returns `true` when the balance was already zero or below, so the spend took nothing.
    #[must_use]
    pub fn is_insufficient(&self) -> bool {
        matches!(self.kind, ErrorKind::Insufficient { .. })
    }

    /// The balance a refused spend found, in micro-units.
    #[must_use]
    pub fn balance_micros(&self) -> Option<i64> {
        match &self.kind {
            ErrorKind::Insufficient { balance_micros } => Some(*balance_micros),
            _other => None,
        }
    }

    /// Returns `true` when the request disagrees with what is already written.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        matches!(self.kind, ErrorKind::Conflict)
    }

    /// Returns `true` when the request can never succeed as written.
    #[must_use]
    pub fn is_invalid(&self) -> bool {
        matches!(self.kind, ErrorKind::Invalid)
    }

    /// Returns `true` when `billing.yml` sells no credit.
    #[must_use]
    pub fn is_unconfigured(&self) -> bool {
        matches!(self.kind, ErrorKind::Unconfigured)
    }

    /// The failure on its own, without the backtrace [`Display`] adds.
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

    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, message)
    }

    pub(super) fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    pub(super) fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    fn unconfigured() -> Self {
        Self::new(
            ErrorKind::Unconfigured,
            "This application sells no credit: config/billing.yml has no wallet section.",
        )
    }

    fn insufficient(organization_id: Uuid, balance_micros: i64) -> Self {
        Self::new(
            ErrorKind::Insufficient { balance_micros },
            format!(
                "organization {organization_id} has no credit left to spend ({balance_micros} \
                 micro-units)"
            ),
        )
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl From<diesel::result::Error> for Error {
    fn from(source: diesel::result::Error) -> Self {
        Self::internal(format!("the wallet's database write failed: {source}"))
    }
}

impl From<Error> for ApiError {
    /// Renders a wallet failure as the answer a caller acts on.
    fn from(source: Error) -> Self {
        match source.kind {
            ErrorKind::Insufficient { .. } => Self::payment_required(
                "This organization's credit is used up. Add credit to continue.",
            ),
            ErrorKind::Invalid => Self::validation(source.message),
            ErrorKind::Conflict => Self::conflict(source.message),
            ErrorKind::Unconfigured => Self::not_found(),
            ErrorKind::Internal => {
                tracing::error!(
                    error.message = source.message,
                    "a wallet request failed: {{error.message}}",
                );
                Self::internal()
            }
        }
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
    use super::{RechargeWallet, validate_auto_recharge};
    use crate::billing::PlanSet;
    use crate::jobs::Job;

    fn plans() -> PlanSet {
        PlanSet::from_yaml(
            "
plans:
  - key: free
    name: Free
wallet:
  name: Account credit
  currency: usd
  minimum_top_up_micros: 10000000
",
        )
        .expect("the plans must parse")
    }

    #[test]
    fn auto_recharge_must_buy_enough_to_clear_its_own_threshold() {
        let plans = plans();
        let terms = plans.wallet().expect("credit is sold");

        validate_auto_recharge(terms, 5_000_000, 10_000_000).expect("a recharge clearing it");
        for (threshold, amount, why) in [
            (20_000_000, 10_000_000, "below its threshold"),
            (0, 5_000_000, "below the minimum top-up"),
            (-1, 10_000_000, "a negative threshold"),
            (0, 10_000_001, "a fraction of a cent"),
        ] {
            let refused = validate_auto_recharge(terms, threshold, amount).expect_err(why);
            assert!(refused.is_invalid(), "{why}: {refused}");
        }
    }

    #[test]
    fn the_recharge_job_is_namespaced_and_runs_on_the_billing_queue() {
        assert!(RechargeWallet::KIND.starts_with("anubis.billing."));
        assert_eq!(RechargeWallet::QUEUE, crate::billing::QUEUE);
    }
}
