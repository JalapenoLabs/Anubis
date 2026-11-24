DROP TABLE subscription_items;

DROP INDEX subscriptions_live_subject_idx;
DROP INDEX subscriptions_live_plan_idx;

-- The earlier schema cannot say what a subject subscription pays for, so those
-- rows leave with the columns that described them.
DELETE FROM subscriptions WHERE subject_type IS NOT NULL;

ALTER TABLE subscriptions DROP CONSTRAINT subscriptions_buy_a_plan_or_a_subject;
ALTER TABLE subscriptions ALTER COLUMN plan_key SET NOT NULL;
ALTER TABLE subscriptions
    DROP COLUMN subject_id,
    DROP COLUMN subject_type;

CREATE UNIQUE INDEX subscriptions_live_organization_idx
    ON subscriptions (organization_id)
    WHERE status NOT IN ('canceled', 'incomplete_expired');
