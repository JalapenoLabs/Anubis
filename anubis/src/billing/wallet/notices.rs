//! What a wallet tells people: notifications to its billing members, and audit rows.
//!
//! Both ride the connection of the write they describe, so a notice or a
//! record commits with the entry that caused it, or with nothing.

use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::Error;
use super::model::{EntryKind, WalletEntry};
use crate::audit::{self, Changes};
use crate::billing::BILLING_ROLE;
use crate::billing::currency;
use crate::notifications::{self, NewNotification};
use crate::schema::{organization_memberships, organizations};
use crate::tenancy::ADMIN_ROLE;

/// How the audit log names a ledger entry as the thing an act happened to.
const ENTRY_SUBJECT: &str = "WalletEntry";

/// How the audit log names a wallet's settings as the thing changed.
const WALLET_SUBJECT: &str = "Wallet";

/// The people a wallet notice goes to: whoever may spend the organization's money.
///
/// Organization members holding the `admin` or the `billing` role, the same
/// authority the billing endpoints ask for; a suspended membership hears
/// nothing, because a suspension is a deny everywhere.
async fn billing_members(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(
            organization_memberships::roles
                .overlaps_with(vec![ADMIN_ROLE.to_owned(), BILLING_ROLE.to_owned()]),
        )
        .filter(organization_memberships::suspended_at.is_null())
        .select(organization_memberships::user_id)
        .order(organization_memberships::user_id.asc())
        .load(connection)
        .await
}

/// Tells the billing members one thing about the wallet.
async fn tell(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    kind: &str,
    title: &str,
    body: &str,
) -> Result<(), Error> {
    let href = format!("/organizations/{organization_id}/billing");
    for user_id in billing_members(connection, organization_id).await? {
        notifications::notify(
            connection,
            NewNotification {
                user_id,
                team_id: None,
                kind,
                title,
                body: Some(body),
                href: Some(&href),
            },
        )
        .await?;
    }
    Ok(())
}

/// The organization's name, which a stored English notice reads by.
async fn organization_name(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
) -> QueryResult<String> {
    organizations::table
        .find(organization_id)
        .select(organizations::name)
        .first(connection)
        .await
}

/// Tells the billing members a spend took the balance below the low mark.
pub(super) async fn low_balance(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    balance_micros: i64,
    currency_code: &str,
) -> Result<(), Error> {
    let name = organization_name(connection, organization_id).await?;
    let body = format!(
        "{name} has {} of credit left. Add credit, or turn on auto-recharge, to keep paying for \
         usage.",
        currency::display(balance_micros, currency_code),
    );
    tell(
        connection,
        organization_id,
        notifications::WALLET_LOW_BALANCE,
        "Credit is running low",
        &body,
    )
    .await
}

/// Tells the billing members a recharge was refused and auto-recharge is off.
pub(super) async fn recharge_failed(
    connection: &mut AsyncPgConnection,
    organization_id: Uuid,
    amount_micros: i64,
    currency_code: &str,
) -> Result<(), Error> {
    let name = organization_name(connection, organization_id).await?;
    let body = format!(
        "Charging {name}'s saved card {} was refused, so auto-recharge is off. Add credit with a \
         card that works, then turn it back on.",
        currency::display(amount_micros, currency_code),
    );
    tell(
        connection,
        organization_id,
        notifications::WALLET_RECHARGE_FAILED,
        "Auto-recharge was turned off",
        &body,
    )
    .await
}

/// Records one entry Stripe caused in the audit log, attributed to the system.
///
/// Spends are not recorded here: the ledger is their record, and a meter
/// charging every minute would bury every other act in the log.
pub(super) async fn record_entry(
    connection: &mut AsyncPgConnection,
    entry: &WalletEntry,
    balance_before_micros: i64,
) -> Result<(), Error> {
    let action = match entry.kind() {
        Some(EntryKind::TopUp) => audit::WALLET_TOPPED_UP,
        Some(EntryKind::Recharge) => audit::WALLET_RECHARGED,
        Some(EntryKind::Refund) => audit::WALLET_REFUNDED,
        Some(EntryKind::RefundReversal) => audit::WALLET_REFUND_REVERSED,
        Some(EntryKind::Spend) | None => return Ok(()),
    };

    audit::record(
        connection,
        &audit::Context::system(),
        &audit::Event::new(action, ENTRY_SUBJECT)
            .organization(entry.organization_id)
            .subject(entry.id)
            .label(&entry.idempotency_key)
            .changes(Changes::new().field(
                "balance_micros",
                balance_before_micros,
                entry.balance_after_micros,
            )),
    )
    .await?;
    Ok(())
}

/// Records a change to the auto-recharge settings, by a person or by a refusal.
pub(super) async fn record_settings(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    action: &str,
    organization_id: Uuid,
    before: Option<(i64, i64)>,
    after: Option<(i64, i64)>,
) -> Result<(), Error> {
    let threshold = |settings: Option<(i64, i64)>| settings.map(|(threshold, _amount)| threshold);
    let amount = |settings: Option<(i64, i64)>| settings.map(|(_threshold, amount)| amount);

    audit::record(
        connection,
        context,
        &audit::Event::new(action, WALLET_SUBJECT)
            .organization(organization_id)
            .subject(organization_id)
            .changes(
                Changes::new()
                    .field("threshold_micros", threshold(before), threshold(after))
                    .field("amount_micros", amount(before), amount(after)),
            ),
    )
    .await?;
    Ok(())
}
