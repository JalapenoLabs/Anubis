-- Many subscriptions per organization, each with its line items.
--
-- An organization holds at most one subscription that buys a plan from
-- `config/billing.yml`, and any number that pay for one of the application's
-- own records (a server, a workspace, a seat on something), named by
-- `subject_type` and `subject_id`. Every subscription carries its line items,
-- one per Stripe price, so an add-on is an item beside the thing it adds to.
-- See `docs/billing.md`.

-- What a subscription pays for: one of the application's records, or nothing,
-- which is the organization's plan. The pair mirrors `audit_events`: a model
-- name and a row id, because the record lives in a table the framework does
-- not own and cannot reference.
ALTER TABLE subscriptions
    ADD COLUMN subject_type TEXT,
    ADD COLUMN subject_id UUID;

-- A subscription that pays for a record buys no plan, so its `plan_key` is
-- null; one that pays for no record is the plan subscription and always names
-- its plan. One rule rather than two, so neither half can drift: plan
-- resolution reads `plan_key` only where `subject_type` is null.
ALTER TABLE subscriptions ALTER COLUMN plan_key DROP NOT NULL;
ALTER TABLE subscriptions
    ADD CONSTRAINT subscriptions_buy_a_plan_or_a_subject CHECK (
        (subject_type IS NULL AND subject_id IS NULL AND plan_key IS NOT NULL)
        OR (subject_type IS NOT NULL AND subject_id IS NOT NULL AND plan_key IS NULL)
    );

-- The one-live-subscription rule becomes two. The terminal statuses are the
-- ones `SubscriptionStatus::is_terminal` answers with, in both indexes: change
-- one and change all three.
DROP INDEX subscriptions_live_organization_idx;

-- One live plan subscription per organization, which is what keeps plan
-- resolution single-valued and checkout's "already subscribed" answer true.
CREATE UNIQUE INDEX subscriptions_live_plan_idx
    ON subscriptions (organization_id)
    WHERE subject_type IS NULL
        AND status NOT IN ('canceled', 'incomplete_expired');

-- One live subscription per subject, across every organization: a record is
-- paid for once. Two purchases of one subject completing at the same moment
-- each try to write a live row, and this is what lets exactly one of them.
CREATE UNIQUE INDEX subscriptions_live_subject_idx
    ON subscriptions (subject_type, subject_id)
    WHERE subject_type IS NOT NULL
        AND status NOT IN ('canceled', 'incomplete_expired');

-- A subscription's line items, mirroring Stripe's. Written only beside the
-- subscription they belong to, in the same guarded write, so an event that is
-- too old to change the subscription cannot change its items either.
CREATE TABLE subscription_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    -- Stripe's id for the item, e.g. `si_1Q...`. Unique, so converging the
    -- same subscription twice updates the item rather than adding another.
    stripe_item_id TEXT NOT NULL,
    -- The price the item bills, which is how an application names an add-on.
    stripe_price_id TEXT NOT NULL,
    -- How many of the price. Null for a metered price, which bills usage
    -- rather than a quantity.
    quantity INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX subscription_items_stripe_id_idx
    ON subscription_items (stripe_item_id);

-- Every read is "this subscription's items".
CREATE INDEX subscription_items_subscription_idx
    ON subscription_items (subscription_id);

CREATE TRIGGER set_updated_at BEFORE UPDATE ON subscription_items
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
