DROP TABLE subscriptions;

DROP INDEX organizations_stripe_customer_idx;
ALTER TABLE organizations DROP COLUMN stripe_customer_id;
