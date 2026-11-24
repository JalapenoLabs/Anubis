//! Money moving at Stripe: wallet payments credited, refunds debited, recharges charged.
//!
//! Every path that learns about a wallet payment, its Checkout session's
//! event, the payment's own events, the recharge that created it, and
//! reconciliation, ends in [`converge_payment`], so a payment credits exactly
//! once however many of them see it and in whatever order. Each reads the
//! payment back from Stripe first: an event's body is a snapshot, and the
//! amount credited is always the amount Stripe says it received, never a
//! number a browser or an event carried.
//!
//! # Refunds are entries, never edits
//!
//! A refund is debited once, keyed by Stripe's refund id, while Stripe says
//! the money is leaving or has left (`pending`, `requires_action`,
//! `succeeded`). A refund that later fails or is cancelled is credited back by
//! a second entry, keyed by the same id, so the ledger stays append-only and
//! every state a refund passes through is recorded once. The payment is
//! converged first, so a refund event processed before the payment's own
//! credit still finds the credit it returns money from.
//!
//! # Recharges
//!
//! [`recharge`] runs one armed attempt (see [`super::arm_recharge`]). The
//! attempt's id travels in the payment's metadata and keys the request, so
//! before it charges anything it looks for a payment the attempt already
//! made, and an answer Stripe never delivered is found again rather than
//! charged twice. A definite refusal, a declined card above all, ends the
//! attempt and turns auto-recharge off with a notice to the billing members,
//! so a card that keeps declining is never charged in a loop.

use chrono::{DateTime, TimeDelta, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::ledger::{self, NewEntry};
use super::model::{EntryKind, Wallet, WalletEntry};
use super::notices;
use crate::audit;
use crate::billing::currency;
use crate::billing::lifecycle::Error;
use crate::billing::plans::{PlanSet, WalletTerms};
use crate::billing::stripe::{
    self, NewRecharge, ORGANIZATION_METADATA, PaymentIntent, RECHARGE_ATTEMPT_METADATA,
    WALLET_METADATA, WALLET_RECHARGE, WALLET_TOP_UP,
};
use crate::billing::subscriptions::stored_customer_id;
use crate::db::DbPool;
use crate::schema::{organizations, wallets};

/// How far before an attempt was armed its payment may have been created.
///
/// Clocks disagree a little, and the search costs nothing more for a wider
/// window, so a payment created moments before the row says it was armed is
/// still found.
const READ_BACK_SLACK: TimeDelta = TimeDelta::minutes(5);

/// What a wallet payment was for, read off the metadata the framework put on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    TopUp,
    Recharge,
}

impl Purpose {
    fn of(payment: &PaymentIntent) -> Option<Self> {
        match payment.metadata(WALLET_METADATA)? {
            WALLET_TOP_UP => Some(Self::TopUp),
            WALLET_RECHARGE => Some(Self::Recharge),
            _other => None,
        }
    }

    fn kind(self) -> EntryKind {
        match self {
            Self::TopUp => EntryKind::TopUp,
            Self::Recharge => EntryKind::Recharge,
        }
    }
}

/// Whether a converge should read the payment's refunds too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::billing) enum Refunds {
    /// A refund event named the payment: read every refund it has.
    Read,
    /// Read them only when the payment's expanded charge says it was refunded.
    WhenRefunded,
    /// The payment's own event: nothing about refunds.
    Skip,
}

/// Brings one payment, and with `refunds` its refunds, into the wallet's ledger.
///
/// A payment that is not a wallet's is left alone, which is every payment a
/// subscription's invoice makes.
///
/// # Errors
/// Returns an [`Error`] when the payment cannot be recorded: credit sold in no
/// wallet, a currency the wallet does not hold, a customer this application
/// does not know, or a database or Stripe failure.
pub(in crate::billing) async fn converge_payment(
    connection: &mut AsyncPgConnection,
    plans: &PlanSet,
    stripe: &stripe::Client,
    payment: &PaymentIntent,
    refunds: Refunds,
) -> Result<(), Error> {
    let Some(purpose) = Purpose::of(payment) else {
        tracing::debug!(
            billing.payment.id = payment.id,
            "payment {{billing.payment.id}} is not a wallet's, so it is left alone",
        );
        return Ok(());
    };
    let terms = plans.wallet().ok_or_else(|| {
        Error::data(format!(
            "payment {} bought credit, and config/billing.yml has no wallet section to credit it to",
            payment.id,
        ))
    })?;
    let organization_id = organization_paying(connection, payment).await?;

    match payment.status.as_str() {
        "succeeded" => credit(connection, terms, organization_id, payment, purpose).await?,
        // Still being collected; its succeeded event credits it.
        "processing" => return Ok(()),
        status => {
            if purpose == Purpose::Recharge {
                let reason = format!("Stripe left the recharge payment {status}");
                fail_attempt(connection, terms, organization_id, payment, &reason).await?;
            }
            return Ok(());
        }
    }

    let read_refunds = match refunds {
        Refunds::Read => true,
        Refunds::WhenRefunded => payment
            .amount_refunded()
            .is_none_or(|refunded| refunded > 0),
        Refunds::Skip => false,
    };
    if read_refunds {
        let held = stripe.list_refunds(&payment.id).await?;
        converge_refunds(connection, terms, organization_id, payment, &held).await?;
    }
    Ok(())
}

/// The organization whose customer made a payment.
///
/// The customer is the binding: credit only ever lands in the wallet of the
/// organization whose Stripe customer paid. The metadata's organization, when
/// present, has to agree, which catches a payment created by hand in the
/// Stripe dashboard for the wrong customer.
async fn organization_paying(
    connection: &mut AsyncPgConnection,
    payment: &PaymentIntent,
) -> Result<Uuid, Error> {
    let customer = payment.customer.as_deref().unwrap_or_default();
    let organization_id: Option<Uuid> = organizations::table
        .filter(organizations::stripe_customer_id.eq(customer))
        .select(organizations::id)
        .first(connection)
        .await
        .optional()?;
    let Some(organization_id) = organization_id else {
        return Err(Error::data(format!(
            "wallet payment {} was made by customer {customer:?}, which names no organization here",
            payment.id,
        )));
    };

    let announced = payment.metadata(ORGANIZATION_METADATA);
    if announced.is_some_and(|announced| announced != organization_id.to_string()) {
        return Err(Error::data(format!(
            "wallet payment {} names organization {:?} in its metadata, and was made by the \
             customer of organization {organization_id}; credit it by hand once it is understood",
            payment.id,
            announced.unwrap_or_default(),
        )));
    }
    Ok(organization_id)
}

/// Credits one succeeded payment, once, and finishes what it was for.
async fn credit(
    connection: &mut AsyncPgConnection,
    terms: &WalletTerms,
    organization_id: Uuid,
    payment: &PaymentIntent,
    purpose: Purpose,
) -> Result<(), Error> {
    let micros = micros_of(
        terms,
        &payment.id,
        payment.amount_received,
        &payment.currency,
    )?;
    let attempt = payment
        .metadata(RECHARGE_ATTEMPT_METADATA)
        .and_then(|value| value.parse::<Uuid>().ok());
    let saved_at = DateTime::from_timestamp(payment.created, 0).unwrap_or_else(Utc::now);

    connection
        .transaction::<_, Error, _>(async |connection| {
            let mut wallet = ledger::lock_or_create(connection, organization_id).await?;
            let kind = purpose.kind();
            if WalletEntry::find(connection, organization_id, kind, &payment.id)
                .await?
                .is_none()
            {
                let before = wallet.balance_micros;
                let entry = ledger::append(
                    connection,
                    &mut wallet,
                    NewEntry {
                        kind,
                        amount_micros: micros,
                        requested_micros: None,
                        key: &payment.id,
                        payment_intent: Some(&payment.id),
                        description: None,
                        subject: None,
                    },
                )
                .await?;
                notices::record_entry(connection, &entry, before).await?;
                tracing::info!(
                    organization.id = %organization_id,
                    billing.payment.id = payment.id,
                    billing.wallet.credited = micros,
                    "payment {{billing.payment.id}} credited {{billing.wallet.credited}} \
                     micro-units to organization {{organization.id}}",
                );
            }

            if purpose == Purpose::TopUp
                && payment.setup_future_usage.as_deref() == Some("off_session")
                && let Some(card) = payment.payment_method.as_deref()
            {
                // Only a newer payment's card replaces the saved one, so an
                // older top-up's event arriving late keeps the newer card.
                diesel::update(
                    wallets::table.find(organization_id).filter(
                        wallets::recharge_payment_method_saved_at
                            .is_null()
                            .or(wallets::recharge_payment_method_saved_at.le(saved_at)),
                    ),
                )
                .set((
                    wallets::recharge_payment_method_id.eq(card),
                    wallets::recharge_payment_method_saved_at.eq(saved_at),
                ))
                .execute(connection)
                .await?;
            }
            if purpose == Purpose::Recharge
                && attempt.is_some()
                && attempt == wallet.recharge_attempt_id
            {
                disarm(connection, organization_id).await?;
            }
            Ok(())
        })
        .await
}

/// Debits every refund Stripe holds for a payment once, and credits back any that failed.
async fn converge_refunds(
    connection: &mut AsyncPgConnection,
    terms: &WalletTerms,
    organization_id: Uuid,
    payment: &PaymentIntent,
    refunds: &[stripe::Refund],
) -> Result<(), Error> {
    connection
        .transaction::<_, Error, _>(async |connection| {
            let mut wallet = ledger::lock_or_create(connection, organization_id).await?;
            // Oldest first, so the ledger reads in the order Stripe refunded.
            for refund in refunds.iter().rev() {
                let micros = micros_of(terms, &refund.id, refund.amount, &refund.currency)?;
                let debited =
                    WalletEntry::find(connection, organization_id, EntryKind::Refund, &refund.id)
                        .await?
                        .is_some();
                let entry = match refund.status.as_deref() {
                    Some("pending" | "requires_action" | "succeeded") if !debited => {
                        Some((EntryKind::Refund, -micros))
                    }
                    Some("failed" | "canceled")
                        if debited
                            && WalletEntry::find(
                                connection,
                                organization_id,
                                EntryKind::RefundReversal,
                                &refund.id,
                            )
                            .await?
                            .is_none() =>
                    {
                        Some((EntryKind::RefundReversal, micros))
                    }
                    _settled => None,
                };
                let Some((kind, amount_micros)) = entry else {
                    continue;
                };

                let before = wallet.balance_micros;
                let written = ledger::append(
                    connection,
                    &mut wallet,
                    NewEntry {
                        kind,
                        amount_micros,
                        requested_micros: None,
                        key: &refund.id,
                        payment_intent: Some(&payment.id),
                        description: None,
                        subject: None,
                    },
                )
                .await?;
                notices::record_entry(connection, &written, before).await?;
            }
            Ok(())
        })
        .await
}

/// Micro-units of what Stripe reports, refusing a currency the wallet does not hold.
fn micros_of(
    terms: &WalletTerms,
    object: &str,
    minor: i64,
    currency_code: &str,
) -> Result<i64, Error> {
    if currency_code != terms.currency() {
        return Err(Error::data(format!(
            "{object} moved {currency_code}, and the wallet holds {}; it cannot be recorded",
            terms.currency(),
        )));
    }
    currency::from_minor(minor, currency_code)
        .filter(|micros| *micros > 0)
        .ok_or_else(|| {
            Error::data(format!(
                "{object} reports an amount of {minor}, which cannot be recorded"
            ))
        })
}

/// Runs one armed recharge; see the [module docs](self).
///
/// # Errors
/// Returns an [`Error`] when the outcome is unknown, so the queue retries the
/// job after reading Stripe back; a decline is not an error, because nothing
/// is left to retry.
pub(in crate::billing) async fn recharge(
    pool: &DbPool,
    plans: &PlanSet,
    stripe: &stripe::Client,
    organization_id: Uuid,
) -> Result<(), Error> {
    let Some(terms) = plans.wallet() else {
        return Ok(());
    };
    let mut connection = pool
        .get()
        .await
        .map_err(|source| Error::data(format!("no database connection: {source}")))?;
    let Some(wallet) = Wallet::find(&mut connection, organization_id).await? else {
        return Ok(());
    };
    let (Some(attempt), Some(amount), Some(armed_at)) = (
        wallet.recharge_attempt_id,
        wallet.recharge_attempt_amount_micros,
        wallet.recharge_armed_at,
    ) else {
        // Nothing armed: an earlier run of this job, or the payment's own
        // event, settled the attempt already.
        return Ok(());
    };
    let Some(customer) = stored_customer_id(&mut connection, organization_id).await? else {
        disarm(&mut connection, organization_id).await?;
        return Ok(());
    };

    // Look before charging: an earlier run of this attempt may have charged
    // already and lost the answer.
    let search = Search {
        customer: &customer,
        attempt,
        armed_at,
    };
    if let Some(found) = search.run(stripe).await? {
        return converge_payment(&mut connection, plans, stripe, &found, Refunds::Skip).await;
    }
    let Some(card) = wallet
        .recharge_payment_method_id
        .as_deref()
        .filter(|_card| wallet.auto_recharge().is_some())
    else {
        // Auto-recharge was turned off after this attempt was armed, and the
        // attempt never charged: there is nothing to do.
        disarm(&mut connection, organization_id).await?;
        return Ok(());
    };
    let amount_minor = currency::to_minor(amount, terms.currency()).ok_or_else(|| {
        Error::data(format!(
            "the armed recharge of {amount} micro-units is not an amount Stripe can charge",
        ))
    })?;

    let charged = stripe
        .create_recharge(
            &NewRecharge {
                customer_id: &customer,
                payment_method_id: card,
                amount_minor,
                currency: terms.currency(),
                description: terms.name(),
                organization_id,
                attempt_id: attempt,
            },
            wallet.recharge_attempt_revision,
        )
        .await;
    let payment = match charged {
        Ok(payment) => payment,
        Err(refused) if refused.is_refusal() => {
            fail_armed(
                &mut connection,
                terms,
                organization_id,
                attempt,
                refused.message(),
            )
            .await?;
            return Ok(());
        }
        Err(unknown) => {
            // The answer never arrived, Stripe answered with a server error, or
            // another run holds the key. Stripe may have charged; reading back
            // says whether.
            if let Some(found) = search.run(stripe).await? {
                found
            } else {
                if unknown.is_indeterminate() {
                    // Stripe saved this answer under the key and would replay
                    // it; nothing was charged, so the next run is a new request.
                    diesel::update(
                        wallets::table
                            .find(organization_id)
                            .filter(wallets::recharge_attempt_id.eq(attempt)),
                    )
                    .set(
                        wallets::recharge_attempt_revision
                            .eq(wallets::recharge_attempt_revision + 1),
                    )
                    .execute(&mut connection)
                    .await?;
                }
                return Err(unknown.into());
            }
        }
    };

    converge_payment(&mut connection, plans, stripe, &payment, Refunds::Skip).await
}

/// Where a recharge attempt's payment would be, if it made one.
struct Search<'a> {
    customer: &'a str,
    attempt: Uuid,
    armed_at: DateTime<Utc>,
}

impl Search<'_> {
    /// The payment the attempt made, read from the customer's recent payments.
    async fn run(&self, stripe: &stripe::Client) -> Result<Option<PaymentIntent>, Error> {
        let since = (self.armed_at - READ_BACK_SLACK).timestamp();
        let attempt = self.attempt.to_string();
        let recent = stripe
            .list_payment_intents(self.customer, Some(since))
            .await?;
        Ok(recent
            .into_iter()
            .find(|payment| payment.metadata(RECHARGE_ATTEMPT_METADATA) == Some(attempt.as_str())))
    }
}

/// Ends a recharge payment that Stripe left unpaid, when it is still the armed attempt.
async fn fail_attempt(
    connection: &mut AsyncPgConnection,
    terms: &WalletTerms,
    organization_id: Uuid,
    payment: &PaymentIntent,
    reason: &str,
) -> Result<(), Error> {
    let Some(attempt) = payment
        .metadata(RECHARGE_ATTEMPT_METADATA)
        .and_then(|value| value.parse::<Uuid>().ok())
    else {
        return Ok(());
    };
    fail_armed(connection, terms, organization_id, attempt, reason).await
}

/// Ends the armed attempt as failed and turns auto-recharge off, telling the billing members.
///
/// Conditional on `attempt` still being the armed one, so a late event about
/// an attempt already settled changes nothing. Turning auto-recharge off is
/// what keeps a declining card from being charged again on the next spend:
/// a person turns it back on, after a top-up saves a card that works.
async fn fail_armed(
    connection: &mut AsyncPgConnection,
    terms: &WalletTerms,
    organization_id: Uuid,
    attempt: Uuid,
    reason: &str,
) -> Result<(), Error> {
    connection
        .transaction::<_, Error, _>(async |connection| {
            let Some(wallet) = ledger::lock(connection, organization_id).await? else {
                return Ok(());
            };
            if wallet.recharge_attempt_id != Some(attempt) {
                return Ok(());
            }
            let amount = wallet.recharge_attempt_amount_micros.unwrap_or_default();
            diesel::update(wallets::table.find(organization_id))
                .set((
                    wallets::auto_recharge_threshold_micros.eq(None::<i64>),
                    wallets::auto_recharge_amount_micros.eq(None::<i64>),
                    wallets::recharge_failed_at.eq(Utc::now()),
                    wallets::recharge_failure.eq(reason),
                ))
                .execute(connection)
                .await?;
            disarm(connection, organization_id).await?;
            notices::record_settings(
                connection,
                &audit::Context::system(),
                audit::WALLET_AUTO_RECHARGE_FAILED,
                organization_id,
                wallet.auto_recharge(),
                None,
            )
            .await?;
            notices::recharge_failed(connection, organization_id, amount, terms.currency()).await?;
            tracing::warn!(
                organization.id = %organization_id,
                error.message = reason,
                "a recharge for organization {{organization.id}} failed, and auto-recharge is off: \
                 {{error.message}}",
            );
            Ok(())
        })
        .await
}

/// Clears the armed attempt, so the next dip below the threshold arms a new one.
async fn disarm(connection: &mut AsyncPgConnection, organization_id: Uuid) -> QueryResult<()> {
    diesel::update(wallets::table.find(organization_id))
        .set((
            wallets::recharge_attempt_id.eq(None::<Uuid>),
            wallets::recharge_attempt_amount_micros.eq(None::<i64>),
            wallets::recharge_attempt_revision.eq(0),
            wallets::recharge_armed_at.eq(None::<DateTime<Utc>>),
            wallets::recharge_queued_at.eq(None::<DateTime<Utc>>),
        ))
        .execute(connection)
        .await?;
    Ok(())
}
