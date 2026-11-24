//! Writing the ledger: each entry and the balance it moves, under one row lock.
//!
//! Every write to a wallet, a spend, a credit, a refund, or a settings change,
//! first takes the wallet's row lock with [`lock`] or [`lock_or_create`], then
//! reads what it needs, then writes. The lock is what makes the cached balance
//! exact: two writers to one wallet are ordered rather than each moving a
//! balance the other already moved, and an idempotency key checked under the
//! lock cannot be written twice by two writers who both found it free. No
//! call to Stripe is ever made while a wallet is locked, so the lock is held
//! for a handful of statements.

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::Error;
use super::model::{EntryKind, Wallet, WalletEntry};
use crate::billing::Subject;
use crate::billing::plans::WalletTerms;
use crate::schema::{wallet_entries, wallets};

/// One entry about to be appended.
#[derive(Debug)]
pub(super) struct NewEntry<'a> {
    pub(super) kind: EntryKind,
    /// Positive for a credit, negative for a debit.
    pub(super) amount_micros: i64,
    pub(super) requested_micros: Option<i64>,
    pub(super) key: &'a str,
    pub(super) payment_intent: Option<&'a str>,
    pub(super) description: Option<&'a str>,
    pub(super) subject: Option<&'a Subject>,
}

/// Locks the organization's wallet for the rest of the transaction, if it has one.
///
/// Every write locks first, so this is also where a wallet created in one
/// currency is refused once `billing.yml` names another: its amounts are
/// micro-units of the first, and reading them as the second would misvalue
/// money already sold.
///
/// # Errors
/// Returns an [`Error`] when the query fails, or when the wallet holds another
/// currency than `terms`.
pub(super) async fn lock(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    terms: &WalletTerms,
) -> Result<Option<Wallet>, Error> {
    let wallet: Option<Wallet> = wallets::table
        .find(organization_id)
        .select(Wallet::as_select())
        .for_update()
        .first(connection)
        .await
        .optional()?;
    if let Some(held) = &wallet
        && held.currency != terms.currency()
    {
        return Err(Error::internal(format!(
            "organization {organization_id}'s wallet holds {}, and config/billing.yml now sells \
             credit in {}; a wallet's currency cannot change, so nothing is written to it",
            held.currency,
            terms.currency(),
        )));
    }
    Ok(wallet)
}

/// Creates the organization's wallet when it has none, then locks it.
///
/// Two first credits at once both insert; the second waits for the first's
/// row and then inserts nothing, and both lock the one that exists.
///
/// # Errors
/// Returns an [`Error`] when a write fails, or when the wallet holds another
/// currency than `terms`.
pub(super) async fn lock_or_create(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    terms: &WalletTerms,
) -> Result<Wallet, Error> {
    diesel::insert_into(wallets::table)
        .values((
            wallets::organization_id.eq(organization_id),
            wallets::currency.eq(terms.currency()),
        ))
        .on_conflict(wallets::organization_id)
        .do_nothing()
        .execute(connection)
        .await?;

    lock(connection, organization_id, terms)
        .await?
        .ok_or_else(|| {
            Error::internal(format!(
                "organization {organization_id}'s wallet vanished between creating and locking it"
            ))
        })
}

/// Appends one entry and moves the balance by its amount.
///
/// The caller holds `wallet`'s lock and has checked the entry's key is free;
/// `wallet` is brought up to date with the balance written.
///
/// # Errors
/// Returns an [`Error`] when the balance would overflow or a write fails.
pub(super) async fn append(
    connection: &mut AsyncPgConnection,
    wallet: &mut Wallet,
    entry: NewEntry<'_>,
) -> Result<WalletEntry, Error> {
    let after = wallet
        .balance_micros
        .checked_add(entry.amount_micros)
        .ok_or_else(|| {
            Error::invalid(format!(
                "an entry of {} micro-units would overflow organization {}'s balance",
                entry.amount_micros, wallet.organization_id,
            ))
        })?;

    let written = diesel::insert_into(wallet_entries::table)
        .values((
            wallet_entries::organization_id.eq(wallet.organization_id),
            wallet_entries::kind.eq(entry.kind.as_str()),
            wallet_entries::amount_micros.eq(entry.amount_micros),
            wallet_entries::balance_after_micros.eq(after),
            wallet_entries::requested_micros.eq(entry.requested_micros),
            wallet_entries::idempotency_key.eq(entry.key),
            wallet_entries::stripe_payment_intent_id.eq(entry.payment_intent),
            wallet_entries::description.eq(entry.description),
            wallet_entries::subject_type.eq(entry.subject.map(Subject::kind)),
            wallet_entries::subject_id.eq(entry.subject.map(Subject::id)),
        ))
        .returning(WalletEntry::as_returning())
        .get_result(connection)
        .await?;

    diesel::update(wallets::table.find(wallet.organization_id))
        .set(wallets::balance_micros.eq(after))
        .execute(connection)
        .await?;
    wallet.balance_micros = after;

    Ok(written)
}
