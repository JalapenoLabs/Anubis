-- The prepaid credit wallet: one balance per organization, and the ledger it
-- is the sum of. See `docs/billing.md`.
--
-- Every amount is integer micro-units of the wallet's currency (1 USD is
-- 1,000,000), so a charge for one minute of usage never rounds to zero.

-- One wallet per organization, created by its first credit. The balance is the
-- sum of the organization's ledger, cached here and moved in the same
-- transaction as every entry, under this row's lock, so the two never drift.
CREATE TABLE wallets (
    organization_id UUID PRIMARY KEY REFERENCES organizations(id) ON DELETE CASCADE,
    -- The currency every amount here and in the ledger is micro-units of,
    -- written when the wallet is created. A `billing.yml` naming another one
    -- is refused at the next write rather than reinterpreting money already
    -- sold.
    currency TEXT NOT NULL,
    -- Below zero only after a refund returned credit that was already spent:
    -- the money has left at Stripe, so the debit cannot be refused. A spend
    -- never takes the balance below zero.
    balance_micros BIGINT NOT NULL DEFAULT 0,
    -- Auto-recharge: when a spend leaves the balance below the threshold, the
    -- saved card is charged the amount. Both or neither.
    auto_recharge_threshold_micros BIGINT,
    auto_recharge_amount_micros BIGINT,
    -- The card a top-up saved for charging off-session, and when the payment
    -- that saved it was created, so an older payment's event never replaces a
    -- newer card.
    recharge_payment_method_id TEXT,
    recharge_payment_method_saved_at TIMESTAMPTZ,
    -- The recharge in flight, armed by the spend that took the balance below
    -- the threshold. While it is set no other spend arms one, which is what
    -- keeps one dip to one charge. The id names the attempt in the payment's
    -- metadata and, with the revision, keys the request at Stripe. When it was
    -- armed bounds the search for a payment whose answer was lost; when its
    -- job was last queued is what tells a stuck attempt from a running one.
    recharge_attempt_id UUID,
    recharge_attempt_amount_micros BIGINT,
    recharge_attempt_revision INTEGER NOT NULL DEFAULT 0,
    recharge_armed_at TIMESTAMPTZ,
    recharge_queued_at TIMESTAMPTZ,
    -- When a run last sent the attempt to Stripe. While a request under its
    -- key may still be open, the attempt is never replaced by one under
    -- another key, which Stripe would carry out as a second charge.
    recharge_charge_started_at TIMESTAMPTZ,
    -- Why the last recharge failed, kept for the screen after a decline turned
    -- auto-recharge off.
    recharge_failed_at TIMESTAMPTZ,
    recharge_failure TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT wallets_auto_recharge_complete CHECK (
        (auto_recharge_threshold_micros IS NULL AND auto_recharge_amount_micros IS NULL)
        OR (
            auto_recharge_threshold_micros >= 0
            AND auto_recharge_amount_micros > 0
            AND recharge_payment_method_id IS NOT NULL
        )
    ),
    CONSTRAINT wallets_recharge_attempt_complete CHECK (
        (recharge_attempt_id IS NULL AND recharge_attempt_amount_micros IS NULL
            AND recharge_armed_at IS NULL AND recharge_queued_at IS NULL
            AND recharge_charge_started_at IS NULL)
        OR (recharge_attempt_id IS NOT NULL AND recharge_attempt_amount_micros > 0
            AND recharge_armed_at IS NOT NULL AND recharge_queued_at IS NOT NULL)
    )
);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON wallets
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- The ledger: append-only, credits positive and debits negative. Nothing in
-- the framework updates or deletes a row, so the table carries no
-- `updated_at`; a refund that fails after it was debited is a new, compensating
-- entry rather than an edit.
CREATE TABLE wallet_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES wallets(organization_id) ON DELETE CASCADE,
    -- Assigned under the wallet's row lock, so it orders one organization's
    -- entries exactly as their balances chain. `created_at` cannot: it is the
    -- start of the writing transaction, and a transaction that started first
    -- may take the lock second.
    position BIGINT GENERATED ALWAYS AS IDENTITY,
    kind TEXT NOT NULL CONSTRAINT wallet_entries_kind CHECK (
        kind IN ('top_up', 'recharge', 'spend', 'refund', 'refund_reversal')
    ),
    amount_micros BIGINT NOT NULL,
    -- The balance this entry left, so every line of the ledger reads on its own.
    balance_after_micros BIGINT NOT NULL,
    -- What a spend asked for. A spend larger than the balance takes what is
    -- left and no more, so this can exceed the amount taken.
    requested_micros BIGINT,
    -- A spend's key is the application's; a credit's is the Stripe payment it
    -- came from, and a refund's is Stripe's refund. Unique per kind, so a
    -- retried meter or a redelivered event lands once.
    idempotency_key TEXT NOT NULL,
    stripe_payment_intent_id TEXT,
    description TEXT,
    -- The record a spend paid for, named the way the audit log names one.
    subject_type TEXT,
    subject_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT wallet_entries_sign CHECK (
        (kind IN ('top_up', 'recharge', 'refund_reversal') AND amount_micros > 0)
        OR (kind IN ('spend', 'refund') AND amount_micros < 0)
    ),
    -- The one rule that makes a prepaid balance prepaid: a spend never leaves
    -- the wallet below zero.
    CONSTRAINT wallet_entries_spend_covered CHECK (
        kind <> 'spend' OR (balance_after_micros >= 0 AND requested_micros >= -amount_micros)
    ),
    CONSTRAINT wallet_entries_subject_complete CHECK (
        (subject_type IS NULL) = (subject_id IS NULL)
    )
);

CREATE UNIQUE INDEX wallet_entries_key_idx
    ON wallet_entries (organization_id, kind, idempotency_key);

-- A payment credits once, whichever path saw it first: its Checkout session,
-- its own event, the recharge that created it, or reconciliation.
CREATE UNIQUE INDEX wallet_entries_credited_payment_idx
    ON wallet_entries (stripe_payment_intent_id)
    WHERE kind IN ('top_up', 'recharge');

-- Every read is "this organization's ledger, newest first".
CREATE INDEX wallet_entries_ledger_idx
    ON wallet_entries (organization_id, position DESC);
