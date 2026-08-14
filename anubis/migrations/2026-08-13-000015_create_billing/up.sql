-- Billing: an organization's Stripe customer, and the subscription it holds.
--
-- Billing attaches to the organization, never to a user and never to a team,
-- because the organization is the tenant that owns teams and pays for them.
-- See `docs/billing.md` and `docs/tenancy.md`.

-- The Stripe customer this organization is billed as. Null until the first
-- checkout creates one, which is why billing needs no signup-time API call:
-- an organization that never pays never reaches Stripe at all.
ALTER TABLE organizations ADD COLUMN stripe_customer_id TEXT;

-- One customer belongs to one organization. The index is partial because null
-- is the normal state and a unique index over nulls would be free of meaning.
CREATE UNIQUE INDEX organizations_stripe_customer_idx
    ON organizations (stripe_customer_id)
    WHERE stripe_customer_id IS NOT NULL;

-- The subscription an organization holds, mirroring Stripe's own record of it.
-- Stripe is the system of record for money; this table is the projection the
-- application authorizes and renders from, kept current by incoming webhooks.
--
-- The free plan is the absence of a row. Nothing is written when an
-- organization is created, nothing has to be backfilled when a plan is added
-- or renamed, and a cancellation ends in the same state a new organization
-- starts in.
CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- The `config/billing.yml` plan this subscription buys. A key rather than a
    -- foreign key: plans live in configuration, and the file is validated at
    -- boot, so a plan a row names either exists or the application never
    -- started.
    plan_key TEXT NOT NULL,
    -- Stripe's own id, e.g. `sub_1Q...`. Unique, so processing the same event
    -- twice cannot produce a second row.
    stripe_subscription_id TEXT NOT NULL,
    -- Stripe's status vocabulary, stored verbatim: incomplete,
    -- incomplete_expired, trialing, active, past_due, canceled, unpaid, paused.
    -- Translating it would only lose the distinctions support asks about.
    status TEXT NOT NULL,
    -- Which of the plan's prices was bought: monthly or yearly.
    billing_interval TEXT NOT NULL,
    -- Seats bought, which is the line item's quantity. One for flat pricing.
    quantity INTEGER NOT NULL DEFAULT 1,
    -- When the paid-for period ends, which is when Stripe next bills or, for a
    -- subscription set to cancel, when access stops.
    current_period_end TIMESTAMPTZ,
    cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX subscriptions_stripe_id_idx
    ON subscriptions (stripe_subscription_id);

-- An organization holds at most one subscription that is not over. Terminal
-- statuses are excluded so the history of what was cancelled stays readable,
-- and the list here is the same one `SubscriptionStatus::is_terminal` answers
-- with: change one and change the other.
CREATE UNIQUE INDEX subscriptions_live_organization_idx
    ON subscriptions (organization_id)
    WHERE status NOT IN ('canceled', 'incomplete_expired');

-- Every read that is not by Stripe id is "this organization's subscriptions".
CREATE INDEX subscriptions_organization_idx
    ON subscriptions (organization_id, created_at DESC);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON subscriptions
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
