# Billing

Anubis bills organizations through Stripe: for a plan, for as many of the application's own records as it sells one subscription per (a server, a workspace), each with its add-ons as line items, and for prepaid credit the application spends as usage accrues. Plans and the terms credit is sold on live in one configuration file, money lives at Stripe, and the application keeps only the projection it has to authorize from and the ledger of the credit it holds.

This mirrors Bullet Train's billing experience (`bullet_train-billing` plus `bullet_train-billing-stripe`) with one file of products and prices, Stripe Checkout for the purchase, the Stripe Billing customer portal for every change after it, and per-plan usage limits. What differs is where things attach: Bullet Train subscribes a Team, Anubis subscribes an **Organization**, because the organization is the tenant that owns teams and pays for them. See [tenancy.md](tenancy.md).

## The module

Everything below is built: plans in configuration, the Stripe client, checkout and the customer portal, subscriptions per record with their line items, the webhook lifecycle and reconciliation, limit enforcement with per-seat pricing, the prepaid credit wallet with top-ups, auto-recharge, refunds, and a spend API, paid-status plan resolution, and the billing screen. Run against Stripe's test mode until a live key and a live endpoint are configured.

## The model

- **Plans** are configuration, not rows. `config/billing.yml` names every plan, the Stripe price behind each billing interval, and the limits the plan grants. The backend embeds the file with `include_str!` and validates it at boot, exactly as it treats `config/roles.yml`.
- **A subscription** is a row in `subscriptions`, mirroring Stripe's own record: Stripe id, status, interval, quantity, period end, and whether it cancels at the end of the period. It is one of two kinds:
  - the **plan subscription**, which names a plan key and no subject. An organization holds at most one live.
  - a **subject subscription**, which pays for one of the application's records, named by `subject_type` and `subject_id` (a model name and a row id, the pair the audit log names a subject with), and names no plan. An organization holds any number, one live per record.
- **Line items** are rows in `subscription_items`, one per Stripe price: the Stripe item id, the price, and the quantity (null for a metered price). An add-on is an item beside the thing it adds to.
- **The Stripe customer** is stored on the organization, in `organizations.stripe_customer_id`. It is null until the first checkout, so an organization that never pays never reaches Stripe at all. One customer pays for every subscription the organization holds, and for its credit.
- **The wallet** is a row in `wallets`, one per organization, holding the balance of prepaid credit and the auto-recharge settings; **the ledger** is `wallet_entries`, append-only, which the balance is the sum of. See [the wallet](#the-prepaid-credit-wallet).

### The free plan is the absence of a plan subscription

Exactly one plan in `billing.yml` sells no prices, leaving aside the [paid plan](#which-plan-is-in-force), and that is the free plan. An organization paying for nothing is on it. Nothing is written at signup, nothing is backfilled when a plan is renamed, and a cancellation lands back where a new organization starts.

The payoff is that plan resolution is **total**: every organization is always on a plan, so no handler, no limit check, and no UI branch has to answer "what does an unsubscribed organization get". `PlanSet::free()` returns a plan rather than an option because validation guarantees there is one.

### Which plan is in force

One function decides it, and every caller goes through it: `Limits::plan_for`, the billing endpoints, and seat enforcement all agree because none of them works it out for itself.

The rule is three steps, in order, and `limits::plan_in_force` is the whole of it:

1. **The plan subscription decides.** A live plan subscription that grants access puts its plan in force; the database holds an organization to one, so the answer is single-valued however many subscriptions it pays for. An explicit purchase of a plan is never overruled by anything else the organization pays for.
2. **Paying for anything puts it on the paid plan**, when `billing.yml` names one with `paid_plan`: a live subscription for one of its records that grants access, or a wallet balance above zero. A balance of exactly zero, or below zero after a refund, pays for nothing. Without `paid_plan`, records and credit never move the plan, because each buys its own thing rather than the organization's limits.
3. **Otherwise, the free plan.**

A plan subscription that grants nothing, or names a plan the file no longer defines (logged for an operator), decides nothing, so the organization falls through to the next step. The billing `GET` says which step decided in `plan_source` (`plan_subscription`, `paid_status`, or `free`), which is how the screen explains the plan to every member.

Limits follow the plan, seats included: an organization whose credit runs out drops to the free plan's seats at that moment. Nobody is removed; the next invitation past the free plan's seats is refused, exactly as it is for an organization that cancels.

Resolution reads only what can matter: the plan subscription always, and the records and the wallet only when `paid_plan` is named and no plan subscription grants access. It is one `EXISTS` and one primary-key read.

### Statuses

`status` holds Stripe's own vocabulary verbatim: `incomplete`, `incomplete_expired`, `trialing`, `active`, `past_due`, `canceled`, `unpaid`, `paused`. Translating it would only lose the distinctions support is asked about.

Two predicates read that column, and both are in `anubis::billing::SubscriptionStatus`:

- `is_terminal()` is `canceled` or `incomplete_expired`, the two statuses that mean the subscription is over. Two partial unique indexes exclude them, and between them they are the live-subscription rule, enforced in the database rather than in a handler because two events arriving at once would otherwise each write a row and neither would know:
  - `subscriptions (organization_id) WHERE subject_type IS NULL AND status NOT IN (...)`: **one live plan subscription per organization**.
  - `subscriptions (subject_type, subject_id) WHERE subject_type IS NOT NULL AND status NOT IN (...)`: **one live subscription per record**, across every organization, because a record is paid for once.

  A check constraint holds the two kinds apart: a subscription names a plan and no subject, or a subject and no plan. Change the predicate and change both indexes in the same migration.
- `grants_access()` is `trialing`, `active`, or `past_due`. `past_due` counts on purpose: Stripe is still retrying the card, and cutting a paying customer off during a retry window is how a failed payment becomes a cancelled account. An organization whose subscription grants nothing falls back to the free plan.

## config/billing.yml

```yaml
plans:
  - key: free
    name: Free
    description: Everything a small group needs to try the product.
    limits:
      seats: 3
      projects: 3

  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_1QpbQSKKAAAAAAAAAAAAAAAA
        amount: 2900
        currency: usd
        per_seat: true
      yearly:
        stripe_price_id: price_1QpbQSKKBBBBBBBBBBBBBBBB
        amount: 29000
        currency: usd
        per_seat: true
    limits:
      seats: 25
      projects:
        count: 100
        enforcement: soft
```

A sequence rather than a map, because file order is presentation order: a pricing page renders the plans as the file lists them.

Two top-level keys beside `plans` are optional, and absent both mean what they always meant:

```yaml
# The plan an organization without a plan subscription is on while it pays
# for anything else: a subscription for one of its records, or credit.
paid_plan: member

# Prepaid credit. Every amount is micro-units of the currency: 1000000 is $1.00.
wallet:
  name: Account credit            # what Stripe Checkout calls a top-up
  currency: usd
  minimum_top_up_micros: 10000000 # $10.00, the smallest top-up and the smallest recharge
  low_balance_micros: 2000000     # optional: tell the billing members below $2.00
```

The paid plan may sell no prices of its own, which is the shape of an application that sells per-record subscriptions or credit and no plan at all: a free plan of one seat, and a paid plan of a hundred that paying for anything reaches. The paid plan is never the free plan, so the free plan is the one price-less plan `paid_plan` does not name.

`anubis::billing::PlanSet::from_yaml` rejects, with a message naming the plan:

- an empty plan list, or a document with unknown fields;
- a plan key or limit name that is not lowercase letters, digits, and underscores (they are stored on subscriptions and read by the frontend);
- a plan with no name, a negative amount, a negative limit, or a currency that is not a lowercase ISO 4217 code;
- a Stripe price id used by two plans, because a price is what identifies the plan a subscription bought;
- a plan list without **exactly one** free plan;
- a `paid_plan` naming no plan;
- wallet terms Stripe could not charge: no name, a currency that is not a lowercase ISO 4217 code, a minimum top-up that is not a positive whole cent (a whole unit for a zero-decimal currency such as `jpy`) or is past Stripe's eight-digit ceiling, or a negative low-balance mark.

Plans deliberately do not inherit from one another the way roles do. Each states its limits in full, because a pricing table is read across and a limit inherited from a plan three rows up is a limit nobody can see. There is therefore no cycle to detect, only the duplicates above.

### Amounts

`amount` and `currency` are what the pricing page shows, in the currency's smallest unit (2900 is $29.00). Stripe's price is the source of truth for what a customer is actually charged; only `stripe_price_id` is ever sent to Stripe. Keeping the amount in the file is what lets a pricing page render without an API call, and it is the same trade Bullet Train's `products.yml` makes.

## Limits

The limit vocabulary is the application's own: `seats`, `projects`, whatever the product meters. Names are lowercase words with underscores, and plural reads best, because a refusal renders the name verbatim ("The Free plan's projects limit of 3 is reached").

**A limit a plan does not name is unlimited.** That is the convention rather than a magic number, because "unlimited" written as `-1` is a value every caller has to remember to special-case.

### Hard and soft

A limit written as a bare number is **hard**. Writing it as a mapping opts into **soft**:

```yaml
limits:
  seats: 25                # hard: the invitation past it is refused
  projects:
    count: 100
    enforcement: soft      # allowed past it, and reported as over
```

The two spellings mean the same count, so the terse one stays the common case and the verbose one is only paid for where the choice is being made. The split is Bullet Train's (`enforcement: hard | soft`), and so is the behavior: a hard limit disables the create, a soft one warns and lets it through.

- **Hard**: `Limits::check` answers `409 Conflict` with a message naming the plan, the limit, and the number: *"The Free plan's projects limit of 3 is reached. Upgrade to add more."* `409` rather than `403`, because the request is well formed and only the organization's current plan refuses it. The message is written for a customer and is returned verbatim, which is what makes it worth upgrading over.
- **Soft**: the check returns `Ok`, the record is created, and the screen reports the overage. The plan's limits carry their `enforcement` into the API body and into the generated TypeScript, so the UI knows which of the two it is looking at without asking.

### Seats are the framework's; everything else is the application's

`seats` is the one limit the framework meters and enforces itself, at the one place a person joins an organization: **creating an invitation**. It has to be, because memberships are the framework's own table and no application code runs there. The check is inside the transaction that writes the invitation, after locking the organization row, so two admins inviting at the same instant are ordered rather than each seeing room for one more.

**A seat is a person, counted once.** `anubis::billing::Limits::seats_used` counts the distinct email addresses that can reach an organization: everyone holding an organization membership, everyone holding a claimed membership in one of its teams, and every invitation still claimable. Counting addresses rather than rows is what keeps one person who belongs to three teams from paying for three seats, and what makes re-inviting somebody who already holds a seat free. An expired invitation holds no seat: it can no longer be claimed, so charging for it would bill for nothing and block an admin who cannot see why.

Every other limit names something only the application can count, so the application calls the check at its own creation choke points:

```rust
let limits = anubis::billing::Limits::new(plans.clone());

// In the create handler, before the insert:
let held = Project::count_for_team(&mut connection, team.id).await?;
limits.check(&mut connection, organization_id, "projects", held).await?;
```

`check` takes the caller's own connection rather than a pool, which is what lets it run inside the transaction that is about to insert: a limit read outside the write it guards is a limit two requests can pass at once. `anubis::billing::limits::Error` converts into `ApiError`, so the call is one line and a `?`.

The scaffolder does not write that line yet: the limit a model is metered by is a product decision (the name, whether it is metered at all, whether the count is per team or per organization) and a template that guessed would generate a check nobody asked for. See [scaffolding.md](scaffolding.md#plan-limits).

## Per-seat pricing

A price marked `per_seat: true` is charged per seat rather than per organization. Three things follow, and nothing else changes:

1. **Checkout buys the seats in use.** The line item's quantity is `seats_used` at the moment of purchase, never below one, so the first invoice is right without waiting for a membership to change.
2. **Membership changes update Stripe.** Creating, claiming, or revoking an invitation, removing a member, leaving, and deleting a team each queue `anubis::billing::SyncSeats` on the `billing` queue, inside the transaction that made the change. A change that rolls back queues nothing. The job carries the organization and nothing else, so a burst of invitations converges on one number rather than replaying every intermediate one.
3. **The job is a no-op unless it has work.** It returns without calling Stripe when billing is off, when the organization has no plan subscription that grants access, when the plan's price is not per-seat, or when the quantity is already right. `PlanSet::sells_per_seat` is checked before queueing at all, so a flat-priced application never writes a job row.

The update is `POST /v1/subscriptions/{id}` with `items[0][id]` and `items[0][quantity]`, because Stripe carries the quantity on the line item rather than on the subscription. The item is the one billing the plan's per-seat price, found by its price, so add-ons beside it on the same subscription are never mistaken for the seats. Nothing about proration is sent, which leaves **Stripe's default, `create_prorations`**: a seat added mid-period is charged for the rest of that period, and a seat removed earns a credit against the next invoice. That is what customers expect from per-seat billing, and changing it is an account-level decision rather than a framework one. The request carries an idempotency key naming the subscription row's last write, so a retried job is one change at Stripe, and a refusal moves that revision so the next attempt is not handed the same refusal back.

The answer Stripe gives is written straight into the projection, exactly as reconciliation is: the row a screen renders agrees with the invoice the customer will get. The job reads the subscription back first and sends nothing when Stripe already bills those seats or the subscription is over, writing what Stripe holds instead: a retry whose earlier attempt's answer was lost would otherwise resend the update and receive that attempt's saved answer, stale by then. When an answer still turns out to be such a replay, read back and billing other seats, the job fails after moving the row's revision, so the queue's retry sends the seats as a new request.

## Subscriptions per record

An application that sells one subscription per thing (each always-on server its own subscription, at its own size and region) holds a **subject subscription** for each, beside the organization's plan subscription. `anubis::billing::Subscriptions` is what it calls to start, change, and cancel them:

```rust
let subscriptions = anubis::billing::Subscriptions::new(pool.clone(), plans.clone(), &config);
let server = anubis::billing::Subject::new("Server", server_id);

// Start: answers a Checkout session; the browser goes to its URL.
let session = subscriptions
    .checkout(organization_id, NewSubscription {
        subject: &server,
        line_items: &[LineItem::new(size_price, 1), LineItem::new(backups_price, 1)],
        return_path: &format!("/servers/{server_id}"),
        email: Some(&user.email),
    })
    .await?;

// Change: an add-on bought or dropped mid-period, on Stripe's proration.
subscriptions
    .change_items(&audit_context, organization_id, &server, &[ItemChange::set(logs_price, 1)])
    .await?;

// End it, now or when the paid-for period does.
subscriptions.cancel(&audit_context, organization_id, &server, Cancellation::AtPeriodEnd).await?;
```

### Why the application owns the routes

The framework serves no endpoint that starts or changes a subject subscription, and that is deliberate. Three questions decide whether one is allowed, and only the application can answer them: whether the record belongs to the organization, who may spend money on it, and which Stripe prices it is sold at. The last is the sharpest: an endpoint taking price ids from a browser would let a customer buy any price in the Stripe account. So the application's own route authorizes the caller against its own record and its own permission model, computes the prices, and calls `Subscriptions`, which owns doing it correctly at Stripe. The framework's `/billing` endpoints list every subscription the organization holds, with its items, and reconcile them. A route that spends money on a record should require what the framework's own billing writes require, the `admin` or `billing` organization role, unless the application has a reason to grant it more narrowly.

Every method names the organization, and a record that organization holds no live subscription for answers as not found, so a record another tenant pays for cannot be changed through this one.

### Starting one

`checkout` opens a Stripe Checkout session in subscription mode, with every line item and with `organization_id`, `subject_type`, and `subject_id` in the session's and the subscription's metadata. Nothing is written: the subscription exists at Stripe once the customer pays, and arrives as an event like any other, naming its record. The return links are built from `APP_URL` and the application's `return_path`, so they can never leave the application. It refuses, before Stripe hears of it, a record that already has a live subscription (`409`), a return path that is not a path under `APP_URL`, and line items Stripe would refuse: none, more than twenty (Stripe's ceiling), a price named twice, a quantity below one, or a price that does not bill every month or every year, read from Stripe before the session is opened (`400`). Checkout is where the rule is enforced, because nothing is written yet; the projection records whatever Stripe reports, so a subscription created at Stripe on a price billing every three months is still recorded (on its interval's unit, `monthly`), and so is its cancellation. A subscription billing weekly or daily cannot be recorded at all and fails its event's job with a message naming it. A metered price cannot be bought or added through this API, since every line carries a quantity and Stripe refuses one on a metered price.

Two purchases of one record that both complete (two tabs, two checkouts) are the one case refusing up front cannot cover. The record's unique index lets exactly one of them become the live row; the other event's job fails with a message naming the record, because which payment the customer keeps is a decision a person makes.

### Changing one

`change_items` takes changes that say where an item should end up rather than what to do to it: `ItemChange::set(price, quantity)` bills that many of a price, adding the item or setting its quantity, and `ItemChange::remove(price)` stops billing it. All of them go to Stripe as one update, so a swap is one change with one set of prorations, and Stripe's default proration applies: an item added mid-period is charged for the rest of it, and one removed earns a credit. Asking for what is already there sends nothing, and a change that would leave no items is refused: that is a cancellation. Every price a change sets is read from Stripe first, and one that does not bill every month or every year is refused (`400`) before Stripe hears of the change, exactly as at checkout, because a subscription moved onto it could be charged and then neither recorded nor cancelled in the projection.

`cancel` ends one with `Cancellation::AtPeriodEnd`, which sets `cancel_at_period_end` and leaves it running to the end of the paid-for period, or `Cancellation::Now`, a `DELETE` on Stripe's defaults (no credit for the unused time, no final invoice). Cancelling one already set to end, or already over at Stripe, sends nothing.

Each change runs the same way:

1. **Lock.** The subscription's row is locked (`SELECT ... FOR UPDATE SKIP LOCKED`) for the change's whole length, so two changes to one subscription never interleave. A change asked for while the row is being written (by another change, or briefly by an event's write) answers `409` at once, to retry, rather than waiting: a waiter would hold a pooled connection, and callers retrying a slow change could pile them up until the pool ran dry for every tenant. A webhook writing the same row waits for the lock and then applies or not by its own timestamp.
2. **Read back.** The subscription is read from Stripe, not from the row, and the change is reduced to the smallest request that reaches it. A subscription Stripe says is over answers `409` for a change, with its terminal state written into the projection.
3. **Send, keyed.** The request carries an idempotency key derived from the request and the row's last write, so a retry of a change whose answer was lost is one change at Stripe, and the same change asked for again after the projection moved on is a second one. Stripe saves a key's first answer, a refusal or a `500` included, and replays it to every retry for a day, so a change Stripe refuses moves the row's revision: the next attempt is a new request, and the read-back before it keeps a fresh key from repeating a change that did apply. An outcome Stripe leaves unknown is not taken for a failed change: an answer that never arrives, a server error (which Stripe documents as possibly carried out), or an earlier attempt still being carried out under the key. The subscription is read back, and a change Stripe shows in place is written and recorded like any other; after a server error that shows nothing, the revision moves so the next attempt is a new request. A change Stripe completes only after that read reaches the projection through its event, without an audit row. A replayed answer (`Idempotent-Replayed`) describes the subscription as it was when the key was first used, so it is read back too, and a change it shows no longer holds answers `409` to retry under a fresh key.
4. **Write back and record.** Stripe's answer is written into the projection, items included, and the change is recorded in the audit log (`subscription.items_changed` with the items before and after, or `subscription.canceled`), on the organization, attributed by the `audit::Context` the caller passes, all in the transaction that holds the lock.

Each change runs on a task of its own. A request that times out, or a client that hangs up, drops the future awaiting the change; were the change that future, its transaction would be dropped open, and the pool hands a connection back without closing it until it is next checked out, so the row would stay locked against every later change and event for that subscription. Spawned, the change always reaches its commit or its rollback, and one Stripe applied is always written back, whether or not anyone is still waiting for the answer.

If the local write fails after Stripe answered, the change still happened at Stripe, which is the system of record: the event Stripe sends about it repairs the projection, and so does reconciliation.

### Errors

`anubis::billing::subscriptions::Error` says which failure it is, and converts into `ApiError`, so an application handler forwards one with `?`:

| Predicate | Answer | When |
|---|---|---|
| `is_unconfigured()` | `503`, naming `STRIPE_SECRET_KEY` | Billing is off |
| `is_not_found()` | `404` | The organization holds no live subscription for that record |
| `is_invalid()` | `400` | A change or purchase Stripe would refuse |
| `is_conflict()` | `409` | The record already has a live subscription, its subscription has ended at Stripe, its row is being written, Stripe answered an earlier attempt of the change, or another purchase is creating the organization's Stripe customer; retrying may work for the last three |
| `is_transport()` | `503` | Stripe could not be reached; retrying may work |
| none of these | `500`, detail in the log | Stripe refused, or the database did |

## The prepaid credit wallet

A usage-priced application sells credit up front and spends it as usage accrues: customers top up, the application charges each hour a server ran, and Stripe stays the record of every payment and refund. The wallet is on when `billing.yml` has a `wallet` section, and an application that sells no credit pays nothing for it.

### Amounts

Every amount is an integer number of **micro-units** of the wallet's currency: 1,000,000 is $1.00, so an hour at $0.0875 is 87,500 and a minute never rounds to zero. The unit is in every name that carries one (`balance_micros`, `amount_micros`), in the tables, the Rust API, the JSON, and `billing.yml` alike, so no two amounts that meet are ever in different units. Stripe charges in the currency's smallest unit instead, and `anubis::billing::currency` is the one place the two meet, following Stripe's own list of zero-decimal and three-decimal currencies: a top-up or a recharge is a whole cent (a whole yen, ten fils), at most Stripe's eight digits.

### The ledger, and the balance it caches

`wallet_entries` is append-only. Credits are positive, spends and refunds negative, and every entry carries the balance it left, so each line of the ledger reads on its own. Nothing updates or deletes a row: a refund that fails after it was debited is a second, compensating entry.

| Kind | Sign | Written by |
|---|---|---|
| `top_up` | + | A payment through Stripe Checkout, keyed by its PaymentIntent |
| `recharge` | + | Auto-recharge's off-session charge, keyed by its PaymentIntent |
| `spend` | - | The application's metering, keyed by the application |
| `refund` | - | A Stripe refund of a wallet payment, keyed by the refund |
| `refund_reversal` | + | That refund failing or being cancelled after it was debited |

`wallets.balance_micros` is the sum of the ledger, moved in the same transaction as each entry under the wallet's row lock. Every write takes that lock first, reads, then writes, and none calls Stripe while holding it, so two writers to one wallet are ordered rather than one overwriting the other's balance, and an idempotency key checked under the lock cannot be written twice. Unique indexes back both keys: one per kind and key per organization, and one credit per PaymentIntent across every path that could credit it.

### Spending

The application calls `anubis::billing::Wallets` from its own metering, on its own connection, so a spend commits with the application's record of the usage it charges for and a rolled-back meter spends nothing:

```rust
let wallets = anubis::billing::Wallets::new(plans.clone(), &config);

let spent = wallets
    .spend(&mut connection, organization_id, Spend {
        amount_micros: 87_500,
        key: &format!("server:{server_id}:hour:{hour}"),
        description: Some("Server alpha, one hour awake"),
        subject: Some(&Subject::new("Server", server_id)),
    })
    .await?;
if spent.uncovered_micros() > 0 {
    // The credit ran out during this hour: stop the server.
}
```

**A spend drains to zero, and never below.** A spend the balance covers takes its amount. One larger than the balance takes what is left, leaving exactly zero, and reports how much it could not cover; one made at zero or below is refused (`402`, `Error::is_insufficient`) with nothing written. Metering charges for usage that already happened, so refusing it whole would leave a remainder that no later charge fits, and an organization holding a fraction of a cent would count as paying forever. An overdraft would be credit extended, which needs a limit, a policy, and collections. **The application learns the wallet ran dry from the spend itself**, in the same call: that is the earliest anything could tell it, and it is exactly where usage is stopped. A balance the application wants to read ahead of time is `Wallets::balance`, and every spend answers with the balance it left.

**Only a refund takes the balance below zero.** The money has already left at Stripe, so the debit cannot be refused; the organization pays for nothing while it owes, and its next credit pays the debt first.

**The key makes a retry one charge.** It is unique per organization and names the charge, so it is best derived from what is being charged for. The same key and amount answers with the entry already written (`Spent::is_replay`); the same key for a different amount or record is refused as a conflict, because a meter reusing keys is a bug worth hearing about. A refusal for lack of credit writes nothing, so the same key charges once credit arrives.

Keep the transaction short: a spend holds the wallet's row lock until the caller commits.

### Top-ups

`POST /billing/organizations/{id}/wallet/top-up` takes `{"amount_micros": 25000000, "save_card": true}` and answers with a Stripe Checkout URL. The session is `mode=payment`, one line priced inline in the wallet's currency and named by `wallet.name`, on the organization's Stripe customer, with `organization_id` and `wallet=top_up` on the payment's metadata. `save_card` adds `payment_intent_data[setup_future_usage]=off_session`, which is how a card auto-recharge can charge gets saved. Below `minimum_top_up_micros`, or not a whole cent, answers `400`; the minimum exists because a card charge carries a fixed fee. Stripe returns the browser to the billing page with `?top_up=success` or `?top_up=canceled`, trusted for nothing but a sentence.

Nothing is credited by the request. The payment arrives as events, `checkout.session.completed`, `checkout.session.async_payment_succeeded` for a method that settles later, and `payment_intent.succeeded`; each reads the payment back from Stripe and credits **the amount Stripe says it received**, never an amount an event body or a browser carried, once, keyed by the PaymentIntent, whichever of them lands first. The payment's customer is the binding: credit only lands in the wallet of the organization whose Stripe customer paid, and a payment whose metadata names another organization fails its job for a person to look at. A payment in a currency the wallet does not hold fails the same way. A top-up that saved its card makes that card the wallet's recharge card, unless a newer top-up already saved one, so a late event never brings back an older card.

### Auto-recharge

`PUT .../wallet/auto-recharge` takes `{"threshold_micros": 5000000, "amount_micros": 10000000}`: when a spend leaves the balance below the threshold, the saved card is charged the amount. It needs a saved card (`409` until a top-up saves one), an amount at least the minimum top-up and at least the threshold, which is what guarantees one recharge lifts a spent-down wallet back over it, and is audited as `wallet.auto_recharge_changed`. `DELETE` turns it off. Turning it on below the threshold recharges at once.

**One dip, one charge.** The spend that lands below the threshold arms an attempt on the wallet row, inside its own transaction under the wallet's lock, and queues `RechargeWallet`. Every spend landing there while the attempt is armed finds it armed and arms nothing, so ten spends crossing at once queue one job. The attempt's id travels in the payment's metadata and, with a revision, keys the request at Stripe. The job reads Stripe back before it charges: it lists the customer's payments since the attempt was armed and looks for one carrying its id, so a run whose answer was lost, or two workers running one attempt, find the charge rather than make another. Then it charges the card off-session (`off_session=true`, `confirm=true`, redirects refused), and the payment is credited exactly as a top-up is.

| Stripe's answer | What happens |
|---|---|
| Succeeded | Credited once, and the attempt is cleared |
| Processing | The attempt stays armed; `payment_intent.succeeded` or `payment_intent.payment_failed` settles it |
| Declined (`402`, a `card_error`), or another definite refusal | The attempt ends, **auto-recharge turns off**, `wallet.auto_recharge_failed` is audited, and the billing members are told. A declining card is never charged in a loop: a person turns it back on, after a top-up saves a card that works |
| No answer, a key still in use | Read back; a charge found is credited, and otherwise the job fails and the queue retries under the same key |
| A server error | Read back; Stripe saves that answer under the key and would replay it, so with nothing found the attempt's revision moves and the retry is a new request |

An attempt left armed for an hour, because its job was dead-lettered or its payment is still processing, is queued again by the next spend below the threshold, under the same attempt, which reads Stripe back first. A refund never arms a recharge: charging a customer again for the credit they were just refunded is not what they asked for.

### Refunds

A refund is made at Stripe (the dashboard, or support), and arrives as `charge.refunded`, `refund.created`, and as it changes, `refund.updated`, `charge.refund.updated`, or `refund.failed`. Every one of them names the payment, and processing reads the payment and **every refund of it** back from Stripe, so which event this is, and the order they come in, never matters:

- a refund that is `pending`, `requires_action`, or `succeeded` is debited once, keyed by its id: the money is leaving or has left;
- a refund that later `failed` or was `canceled`, after its debit, is credited back once by a `refund_reversal`;
- the payment itself is converged first, so a refund processed before its own payment's credit still finds the credit it returns money from.

Refunds and reversals are audited as `wallet.refunded` and `wallet.refund_reversed`.

### Notices and the audit log

Notices go to the organization's **billing members**: everyone holding the `admin` or `billing` organization role whose membership is not suspended, linking to the billing page.

| Kind | When |
|---|---|
| `wallet.low_balance` | A spend takes the balance from at or above `low_balance_micros` to below it, once per dip. Not sent while auto-recharge is on, because the recharge is the answer, and a failed one says so itself |
| `wallet.recharge_failed` | A recharge was refused and auto-recharge turned off |

Credits, refunds, reversals, and auto-recharge changes and failures are audited on the organization, in the transaction that wrote them. Spends are not: the ledger is their record, and a meter charging every minute would bury every other act in the log. Every notice and audit row rides the connection of the write it describes, so it commits with the entry, or with nothing.

### Reading the wallet

The billing `GET` answers the wallet to `admin` and `billing` (see [the endpoints](#endpoints)), and `GET .../wallet/entries?page=&limit=` pages the ledger, newest first, by `position`, the order the balances chain in. Both are the billing roles' only: the balance says what the organization spends, and a spend names the record it paid for, which a member may not reach. Reconciliation converges the wallet's payments too: a top-up whose events never arrived is credited, and a refund whose events were missed is debited, reading each payment's refunds only when its charge says it was refunded.

### Without Stripe

With `STRIPE_SECRET_KEY` unset, the wallet still reads and the ledger still pages; a top-up and auto-recharge answer `503` naming the variable; and credit already held is still spent, because metering does not stop when a deployment cannot sell more for a while. Nothing is armed, since there is no card to charge.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `STRIPE_SECRET_KEY` | unset | Stripe secret key, `sk_live_...` or a restricted `rk_live_...`; setting it enables billing |
| `STRIPE_WEBHOOK_SECRET` | unset | Secret Stripe signs billing events with, `whsec_...`; setting it enables the receiver |
| `STRIPE_API_BASE` | `https://api.stripe.com` | Where Stripe's API lives; for tests and Stripe-compatible mocks only |

`STRIPE_SECRET_KEY` is the switch, and it follows the same posture as `SMTP_URL`:

- **Unset** (the development default): every organization is on the free plan, `GET` keeps answering with `billing_enabled: false`, and checkout and portal answer `503` naming the variable. The UI hides what cannot work.
- **Production without it**: the application boots and warns at startup. An application that does not charge yet should not be blocked on billing configuration.
- **Malformed**: a publishable key (`pk_...`) is refused by name at boot, because it is the key people reach for first and every call made with it would fail at Stripe.

`STRIPE_WEBHOOK_SECRET` is the second switch, and it governs the receiver alone:

- **Unset**: the receiver answers `503` naming the variable, and subscriptions stop being kept current. A production deployment with billing on and this off is warned at startup, because it is the one configuration that takes money and hears nothing back.
- **Set without `STRIPE_SECRET_KEY`**: refused at boot. The receiver reads each subscription back from Stripe, which needs the key, so half-configured billing never starts.
- **Set to an API key**: refused at boot by name. It is the paste people get wrong, and every real event would be rejected by a receiver that took it.

Both values are bearer credentials for an account that moves money, so neither is ever echoed in an error, a log line, or a `Debug` rendering.

`anubis doctor` validates `config/billing.yml` with the parser the server boots with, and reports its absence as "billing is off for this application" rather than as a problem.

## Endpoints

Two mounts, both in the application's `main.rs` and both listed by `anubis routes`: the organization-scoped surface under `/billing`, and the receiver under `/webhooks`.

| Route | Guard | Effect |
|---|---|---|
| `GET /billing/organizations/{organization_id}` | org member | The plan in force, the plan subscription behind it, the seats in use, whether billing is configured, and, for `admin` or `billing`, every live subscription with its line items |
| `POST /billing/organizations/{organization_id}/checkout` | org `admin` or `billing` | Opens a Stripe Checkout session, answers with its URL |
| `POST /billing/organizations/{organization_id}/portal` | org `admin` or `billing` | Opens the Stripe customer portal, answers with its URL |
| `POST /billing/organizations/{organization_id}/reconcile` | org `admin` or `billing` | Reads Stripe and corrects every subscription and wallet payment this application shows |
| `POST /billing/organizations/{organization_id}/wallet/top-up` | org `admin` or `billing` | Opens a Stripe Checkout session that buys credit, answers with its URL |
| `PUT /billing/organizations/{organization_id}/wallet/auto-recharge` | org `admin` or `billing` | Turns auto-recharge on, or changes it |
| `DELETE /billing/organizations/{organization_id}/wallet/auto-recharge` | org `admin` or `billing` | Turns auto-recharge off |
| `GET /billing/organizations/{organization_id}/wallet/entries` | org `admin` or `billing` | One page of the wallet's ledger, newest first |
| `POST /webhooks/stripe-billing` | a Stripe signature | Stores one Stripe event and queues the job that acts on it |

Reading is open to every member of the organization because the plan and its limits explain what the whole organization can do. The list of every subscription is the exception, served to `admin` and `billing` alone: a record's subscription names the record, its prices, and its status, and a member, a guest above all, may not reach every record the organization pays for. Spending money takes the `admin` role or the `billing` role, which the starter's `roles.yml` ships described as "can manage billing and subscriptions"; repairing the projection takes the same authority, because it is the same subject. A member of another organization gets `404` on every one, the same as a member of none: the `OrganizationMember` guard never reveals that an organization exists.

The receiver is the exception to all of that: it is mounted under `/webhooks` rather than `/billing`, carries no guard, and authenticates its caller with a signature. See [the lifecycle](#the-subscription-lifecycle).

`GET` answers:

```json
{
  "plan": {
    "key": "pro",
    "name": "Pro",
    "description": null,
    "highlighted": true,
    "prices": {
      "monthly": {
        "stripe_price_id": "price_…", "amount": 2900, "currency": "usd", "per_seat": true
      }
    },
    "limits": { "seats": { "count": 25, "enforcement": "hard" } }
  },
  "subscription": {
    "id": "…", "organization_id": "…", "plan_key": "pro",
    "subject_type": null, "subject_id": null,
    "stripe_subscription_id": "sub_…", "status": "active",
    "billing_interval": "monthly", "quantity": 2,
    "current_period_end": "2026-09-13T00:00:00Z", "cancel_at_period_end": false,
    "items": [
      { "id": "…", "subscription_id": "…", "stripe_item_id": "si_…",
        "stripe_price_id": "price_…", "quantity": 2, "created_at": "…", "updated_at": "…" }
    ],
    "created_at": "…", "updated_at": "…"
  },
  "subscriptions": [
    { "…": "the plan subscription again, first" },
    { "plan_key": null, "subject_type": "Server", "subject_id": "…", "items": ["…"], "…": "…" }
  ],
  "billing_enabled": true,
  "seats_used": 2,
  "plan_source": "plan_subscription",
  "wallet": {
    "currency": "usd", "balance_micros": 12500000,
    "minimum_top_up_micros": 10000000, "low_balance_micros": 2000000,
    "auto_recharge": { "threshold_micros": 5000000, "amount_micros": 10000000 },
    "card_saved": true, "recharging": false, "recharge_failed_at": null
  }
}
```

`subscription` is the plan subscription, the one behind `plan`; `subscriptions` is everything the organization pays for, the plan subscription first and then its records, grouped by model and oldest first, and is `null` for a member without the `admin` or `billing` role. A screen that shows a record's subscription usually shows it where the record lives, and reads it from here or from `Subscription::live_for_subject`.

`plan_source` says which step of [plan resolution](#which-plan-is-in-force) decided the plan, and every member reads it. `wallet` is `null` when the application sells no credit, and for a member without the `admin` or `billing` role, for the same reason `subscriptions` is: the balance says what the organization spends, and the ledger names the records it spent on.

`seats_used` is the one usage figure the framework can count for every application, because memberships are its own table. A screen reads it against the plan's `seats` limit to show how full the organization is; an application's own limits are counted by the application.

`POST .../checkout` takes `{"plan_key": "pro", "interval": "monthly"}` and answers `{"url": "https://checkout.stripe.com/…"}`. It refuses, in this order:

| Answer | When |
|---|---|
| `403` | The caller holds neither `admin` nor `billing` |
| `503` | `STRIPE_SECRET_KEY` is unset |
| `400` | Unknown plan, the free plan, an unknown interval, or an interval that plan does not sell |
| `409` | The organization already has a live plan subscription; changing plans is the portal's job. Subscriptions for its records are no obstacle |

`POST .../portal` answers `409` when the organization has no Stripe customer yet, because there is nothing to manage before the first purchase.

Both write endpoints answer `503` when Stripe cannot be reached, which is a temporary state worth retrying, and `500` when Stripe refuses the call, which is a misconfiguration on this side (a price id that no longer exists, an account that cannot take payments) with the detail in the log.

### Return URLs

Stripe needs absolute URLs before a purchase begins, so the framework builds them from `APP_URL`: `<APP_URL>/organizations/<organization_id>/billing`, with `?checkout=success&session_id=…` or `?checkout=canceled` on the two checkout outcomes. The path is scoped to the organization because that is the page the browser should land on, and it deliberately avoids `/billing`, which is a reserved API prefix (see [architecture.md](architecture.md)): a client route there would answer JSON on a cold load.

## The subscription lifecycle

A purchase happens at Stripe, in a browser tab this application does not control, so nothing that comes back through the browser can be trusted. What is trusted is Stripe's own events, and the loop they drive is three steps: **store**, **process**, **reconcile**.

### The receiver

Stripe posts to `POST /webhooks/stripe-billing`.

It is **framework-mounted**, not scaffolded, because the subscription lifecycle is the framework's own feature: `anubis::billing::webhook_router` is nested under `/webhooks` by the application's `main.rs`, beside whatever `anubis scaffold webhook` generated for the application itself.

**The path is the coexistence story.** `anubis scaffold webhook Stripe` generates an application-owned receiver at `/webhooks/stripe`, for Stripe Connect, one-off payments, or anything else the framework knows nothing about. That receiver has its own table, its own job, and its own `STRIPE_WEBHOOK_SECRET`-shaped variable, and it never sees a billing event, because the two are separate endpoints registered separately in the Stripe dashboard. `anubis scaffold webhook StripeBilling` is refused by name, since two routers claiming one path panic at boot.

The endpoint does two things and no more: it writes the event down and queues a job, both in one transaction, then answers `200`. That is the store-then-process discipline [webhooks.md](webhooks.md) describes for generated receivers, and the reason is the same: work done inside the request can time out and be retried, work done after the `200` is the queue's, with its own backoff and its own dead-letter table.

| Column | What it holds |
|---|---|
| `stripe_event_id` | Stripe's own `evt_...`, **unique**, which is the whole idempotency guarantee |
| `event_type` | e.g. `customer.subscription.updated`, so the backlog reads without opening every document |
| `payload` | Stripe's JSON, exactly as it arrived |
| `stripe_created_at` | When Stripe created the event, which is how ordering is decided |
| `received_at` | When the request arrived |
| `processed_at` | When processing finished, or `NULL` while the row is still waiting |
| `error` | The most recent processing failure |

Stripe redelivers an event it did not hear a `2xx` for, sometimes while the first delivery is still being processed. The unique index is what turns two concurrent redeliveries into one row and one job; the endpoint answers `200` with the same row id both times, and queues nothing the second time.

### Verification is strict, and refused requests are not stored

The signature is Stripe's own scheme: `Stripe-Signature: t=<unix>,v1=<hex>`, where the signed message is `<t>.<body>` over the exact bytes that arrived. `anubis::webhooks::signature::verify_stripe` is that check, and it lives in the provider-agnostic signature module so an application receiving Stripe events for its own purposes does not write the parser a second time. It accepts several `v1` entries, which is what makes a secret rotation at Stripe seamless, ignores schemes it does not know, and refuses a timestamp further than five minutes from now, which is what stops a captured request from being replayed.

A request that does not verify is refused with `400`, per Stripe's own guidance, and **nothing is stored**. This is the one place the framework departs from what a generated receiver does, and the difference is knowledge: a generated `verify_signature` is a function the developer has to finish, so a check that is subtly wrong must not look like silence, and storing the request is the evidence that explains it. Here the scheme is known, implemented, and tested, so a request that fails is either an attacker or a mistyped secret. Neither is worth a row, the response message names the reason where Stripe's delivery log shows it, and a public unauthenticated endpoint that writes a row for anything that arrives is a liability rather than a diary.

With `STRIPE_WEBHOOK_SECRET` unset the endpoint answers `503` naming the variable, rather than `200`. A silent drop would look to Stripe like success and the events would never be redelivered.

### The events, and what each one writes

`anubis::billing::HANDLED_EVENTS` is the list to select when creating the endpoint at Stripe:

| Event | What the framework does |
|---|---|
| `checkout.session.completed` | For a subscription: resolves the organization, reads the new subscription back from Stripe, and upserts the row and its items. For a top-up: reads the payment back and credits it |
| `customer.subscription.created` | Same, for a subscription created anywhere else, including the Stripe dashboard |
| `customer.subscription.updated` | Same: status, period end, quantity, `cancel_at_period_end`, a plan change through the portal, and any item added, removed, or resized, which Stripe announces as an update |
| `customer.subscription.deleted` | Writes the terminal status from the event's own body |
| `checkout.session.async_payment_succeeded` | A top-up by a payment method that settles later settled: reads the payment back and credits it |
| `payment_intent.succeeded` | Reads the payment back; a top-up or a recharge is credited, once |
| `payment_intent.payment_failed` | Reads the payment back; a recharge Stripe left unpaid ends its attempt and turns auto-recharge off |
| `charge.refunded`, `charge.refund.updated`, `refund.created`, `refund.updated`, `refund.failed` | Reads every refund of the payment back and records each once; see [refunds](#refunds) |

Every other event type is **stored, marked processed, and ignored**, which is what makes "send all events" a safe way to configure the endpoint. A receiver that failed on the types it does not know would fail on every feature Stripe ships.

**The organization** comes from the subscription's `metadata[organization_id]`, which checkout put there, then from the checkout session's own metadata, and finally from `organizations.stripe_customer_id`, which is the mapping this application wrote itself when the customer was created. That last fallback is what makes a subscription started in the Stripe dashboard land correctly. An event that resolves to no organization fails the job and stays on the row as evidence.

**The record** comes from the subscription's `metadata[subject_type]` and `metadata[subject_id]`, which `Subscriptions::checkout` put there. Both or neither: a subscription naming half a record, or a record id that is not a UUID, fails the job, because recording it as the plan subscription would be a second one. A subscription naming a record buys no plan, and its prices are the application's own, so nothing looks them up; its interval is the unit of their recurrence, monthly or yearly, however many units pass between bills.

**The plan**, for a subscription naming no record, comes from the price, through `PlanSet::find_by_price_id`, because changing plan in the customer portal changes the price and leaves the metadata naming what was bought first. Any of its items may carry the plan's price, since a plan subscription can carry add-ons beside it, but only one plan's price may appear. A price `config/billing.yml` does not sell falls back to the plan the metadata names, and if that fails too the job fails with a message naming the price: the fix is to add the price to the file or to stop offering the product in the portal.

**The items** are written beside the subscription, in the same transaction and behind the same ordering guard: an item Stripe no longer lists is deleted, and every listed one is inserted or brought up to date by its Stripe item id. Stripe embeds a subscription's items as a list and sends only its first page when there are many; the client reads the rest before anything is written, and a list that still arrives partial fails rather than deleting items that exist.

### Reading rather than trusting

An event's body is a snapshot of the moment Stripe created it, and a job may run it minutes later. Stripe's guidance is to treat the payload as possibly stale, so the three non-terminal events above are treated as **notifications**: the event says which subscription changed, and `GET /v1/subscriptions/{id}` says what it looks like now. That is one of the two reads on the Stripe client, and it is the honest one.

`customer.subscription.deleted` is the exception, and it is trusted as it arrives. A cancelled subscription is over, nothing Stripe could say about it later would be newer, and re-reading it would spend a call to be told what the event already said.

### Out-of-order delivery

Stripe does not promise order. Every write carries the creating event's `created` timestamp into `subscriptions.stripe_event_at`, and the upsert applies only when the incoming event is **not older** than the one already applied:

```sql
INSERT INTO subscriptions (...) VALUES (...)
ON CONFLICT (stripe_subscription_id) DO UPDATE SET ...
WHERE subscriptions.stripe_event_at IS NULL
   OR subscriptions.stripe_event_at <= excluded.stripe_event_at
```

So a late `customer.subscription.updated` cannot undo a cancellation that arrived first, and since a subscription's items are written only when its row is, a late event cannot bring back an add-on that was removed either. Two events created in the same second both apply, which is safe precisely because each one writes the state read back from Stripe rather than its own body.

A write that read Stripe directly (reconciliation, seat synchronization, an application's change) is stamped with **the moment its read began, in whole seconds**, and one that did so without holding the subscription's row lock (reconciliation, seat synchronization) a second earlier still, so it **loses a tie** with an event: an event created in the second its read began may already be written when it lands, and a tie would let its older read win. Both halves matter. Anything Stripe recorded before the read is in what it read, so an older event is rightly ignored, while anything later may be missing from it, so a newer event must stay free to apply; letting an event win is always safe, because processing one reads Stripe back. A write stamped when it lands would instead overwrite a newer event with the older state it read first. And Stripe's `created` is whole seconds, so a write stamped with a fraction of a second would sort after a cancellation created later in the same second, dropping it and leaving a cancelled subscription granting access.

Whatever the stamps say, **a subscription that is over stays over**: the upsert never replaces a terminal status with a live one, because Stripe never revives a cancelled or expired subscription. A deletion is the last event Stripe sends for a subscription, so this is what keeps a late or skewed read from reviving one with nothing left to repair it.

The **live-subscription indexes** hold under all of this: the upsert is keyed on `stripe_subscription_id`, so a resubscription after a cancellation inserts a second row while the first is terminal, and a customer who somehow holds two live plan subscriptions, or two live subscriptions for one record, fails the job with a message naming which, because that is a decision a person has to make.

### Processing

The job is `anubis::billing::ProcessStripeEvent`, on its own `billing` queue so that Stripe replaying a day of events, or a Stripe API call that hangs, cannot hold up the work a person is waiting on. `anubis::billing::Reconciler` is the handler, and an application registers it on its worker beside the webhook deliverer:

```rust
let reconciler = anubis::billing::Reconciler::new(pool.clone(), plans.clone(), &config);
let worker = anubis::jobs::Worker::builder(pool.clone())
    .register(move |job: anubis::billing::ProcessStripeEvent| {
        let reconciler = reconciler.clone();
        async move { reconciler.process(job).await }
    })
    .build();
```

Delivery is at-least-once, like every job on this queue, so the handler is idempotent twice over: an event already stamped `processed_at` returns without acting, and the write itself is an upsert. A failure records the message on the event row **and** returns the error, so the queue retries on its widening backoff and eventually dead-letters the job with the row still holding the reason. See [jobs.md](jobs.md).

### Reconciliation

A missed event would otherwise leave the projection wrong forever: an endpoint misconfigured for a week, a deployment down while Stripe gave up retrying, a price that reached `config/billing.yml` after the first customer bought it.

`Reconciler::reconcile(organization_id)` is the repair, and it is the same convergence the events drive. It reads every subscription Stripe holds for the organization's customer (`GET /v1/subscriptions?customer=…&status=all`, following Stripe's pages to the end, because a customer paying per record holds a subscription per record), writes each one with its items, and answers with the organization's live subscriptions afterwards, plan subscription first. Finished subscriptions are written first, so a row this application still believes is live releases its slot before the live one takes it.

Each subscription converges in its own transaction. One that cannot be recorded (a price `config/billing.yml` does not sell, a second live subscription for one record) leaves its own row as it was and is named in the error, and every other subscription is still repaired: an organization paying for a hundred records is not held back by one.

`POST /billing/organizations/{id}/reconcile` exposes it, guarded like the other writes, and answers with the same body as the `GET`, so the screen that triggered it re-renders from the answer. An organization that never reached Stripe has nothing to converge and answers with what it already had. Being unable to reach Stripe answers `503`; anything else answers `500` with the detail in the log.

There is no periodic sweep yet, because there are no recurring schedules yet; that is tracked with the rest of the scheduling work in [jobs.md](jobs.md#roadmap). The function is written to be called from one the day it exists.

## Why a hand-written Stripe client

`anubis::billing::stripe::Client` is a few hundred lines over the `reqwest` client the framework already carries. The alternative considered was the `async-stripe` crate.

- **Surface area**: the whole integration is fourteen calls, seven writes and seven reads, every one answering flat JSON: create a customer, open a checkout session, open a portal session, update a subscription (its items, a seat count, or cancelling at period end), cancel one now, read one, list a customer's, read the rest of a long item list, read a price, open a top-up session, charge a saved card off-session, read a payment, list a customer's payments, and list a payment's refunds. A generated SDK models the entire Stripe API to serve fourteen calls.
- **Pure-Rust TLS is mandatory** here, and both options can satisfy it, but `reqwest` with rustls is already in the tree for outgoing webhook delivery, so this adds no dependency and no second TLS stack. `cargo tree -i openssl` matches nothing, and CI keeps it that way.
- **Compile time**: this workspace compiles in seconds and that is worth protecting. `async-stripe` is one of the larger crates in the ecosystem, and its API surface changes with Stripe's.
- **The escape hatch stays open**: the client is one module behind a small surface. An application that needs more of Stripe than the framework uses can depend on `async-stripe` itself without fighting anything here.

Conventions the client holds to:

- Requests are `application/x-www-form-urlencoded` with Stripe's nested parameter syntax (`metadata[organization_id]`, `line_items[0][price]`).
- Responses are parsed into the few fields the framework reads. Stripe adds fields constantly, and a struct insisting on all of them would break on their schedule rather than ours.
- Creating a customer carries an `Idempotency-Key` derived from the organization, so two checkouts started at the same instant converge on one customer instead of racing to create two. The store is then conditional on the column still being null, so neither request can overwrite the other. Two first purchases by two people send different emails under that one key, which Stripe refuses for the second; the second then uses the customer the first stored, or, when the first has not stored it yet, answers `409` so a retry finds it.
- Updating a subscription carries an `Idempotency-Key` hashed from the request and a revision the caller names, the subscription row's last write: a retry of one change is one change at Stripe, and the same change asked for after the row moved on is another. Stripe replays a key's first answer for a day, refusals included, so a refused update moves the row's revision. A cancellation needs no key, because Stripe treats a `DELETE` as idempotent by definition and ignores one. An answer Stripe marks `Idempotent-Replayed` is the saved answer to an earlier request, so the client reads the subscription back and answers with that instead.
- Checkout sessions carry `client_reference_id`, `metadata[organization_id]`, and what was bought, `metadata[plan_key]` for a plan or `metadata[subject_type]` and `metadata[subject_id]` for a record, with the same values under `subscription_data[metadata]`, so every event that follows identifies the tenant and what it pays for without a lookup table of Stripe ids.
- No `Stripe-Version` header is sent, so calls run on the account's default API version. Pinning one is a single constant when a deployment wants its upgrades to be deliberate. Because the version is the account's, a subscription's `current_period_end` is read from the subscription and from its first line item, which is where Stripe moved it in the 2025 versions.
- Ids that arrive in an event body are percent-encoded before they reach a path, because a path is never a place to paste input.

## Setting Stripe up

1. Create the products and prices in the Stripe dashboard (start in test mode: <https://dashboard.stripe.com/test/products>).
2. Copy each price id into `config/billing.yml`.
3. Put the secret key in `.env` as `STRIPE_SECRET_KEY` (<https://dashboard.stripe.com/test/apikeys>).
4. Activate the customer portal and choose which products it may switch between (<https://dashboard.stripe.com/test/settings/billing/portal>). List only plans this application defines, or a plan change will produce an event naming a price no plan sells. The portal lists every subscription the customer holds, records' included, so decide whether customers may cancel there too: a cancellation in the portal arrives as an event like any other, and the application learns of it by reading the projection.
5. Add a webhook endpoint at `<APP_URL>/webhooks/stripe-billing` (<https://dashboard.stripe.com/test/webhooks/create>), selecting the twelve events in `anubis::billing::HANDLED_EVENTS`. Selecting every event works too; the rest are stored and ignored.
6. Copy that endpoint's signing secret into `.env` as `STRIPE_WEBHOOK_SECRET`.
7. Repeat all of it in live mode before launch, including a live `STRIPE_SECRET_KEY` and the live endpoint's own signing secret. The two secrets are per mode and per endpoint, so nothing carries over.

In development, Stripe cannot reach `localhost`. Forward the events with `stripe listen --forward-to localhost:3000/webhooks/stripe-billing`, which prints a signing secret of its own for `STRIPE_WEBHOOK_SECRET`, or expose the port with a tunnel and register the tunnel's URL.

## Testing

Five narratives, all against a **mock Stripe**: a small axum server serving the endpoints the framework calls, pointed at with `STRIPE_API_BASE`, exactly as `oauth_flow.rs` serves a mock OpenID Connect provider. Nothing in the code under test is special-cased for a test.

- `anubis/tests/billing_flow.rs` covers the purchase: the plan resolved from configuration, the customer created and stored, the checkout session opened with the price the plan names, and the portal. The assertions read the real request bodies, down to the idempotency key and the `Authorization` header.
- `anubis/tests/billing_webhooks_flow.rs` covers the lifecycle: events signed with Stripe's scheme and the configured secret, a refused signature (missing, wrong, tampered, replayed) storing nothing, a redelivery becoming one row and one job, a full lifecycle from completed checkout through a plan change to cancellation and a fresh subscription, an out-of-order event changing nothing, reconciliation correcting a drifted row, and two organizations that never see each other's subscriptions.

- `anubis/tests/billing_subscriptions_flow.rs` covers several live subscriptions and their items, against a stateful mock that applies updates, keeps Stripe's idempotency cache, pages its lists, and embeds only the first page of a long item list: a plan subscription and two records held at once with the plan deciding the limits, a second live subscription for one plan or one record refused with a message naming it, a record bought through Checkout and arriving as an event, add-ons bought, resized, swapped, and removed mid-period with keyed requests and audit rows, a repeated change sending nothing, a late event leaving items alone and a newer one winning, a redelivery changing nothing twice, a record another organization pays for answering not found, the list of records withheld from a member without billing authority, both cancellations, changes refused with `503` and nothing written when Stripe is unset or unreachable, a change Stripe refused sent afresh rather than replayed, a change whose answer was lost, or answered with a server error, read back and recorded, a replayed answer read back rather than trusted, a change onto a weekly price refused before Stripe hears of it, a first purchase refused before the other stored the customer told to retry, a price billing weekly or quarterly refused before Checkout while a subscription created at Stripe on a quarterly price, and its cancellation, are still recorded, two first purchases with different emails sharing one customer, a cancelled subscription never revived by a later read, and reconciliation following pages and long item lists, deleting drifted items, and repairing the rest past one it cannot record. The races run for real: a second change refused while the first is held open at Stripe, a change refusing while the test holds its row, five retries on a held subscription each answered at once rather than queued, a change whose caller is aborted mid-flight still finishing and freeing the row, a webhook arriving while a change holds the row, two purchases of one record converging at once, a reconciliation whose older read lands after a newer event or a cancellation created in the same second, and a cancellation in the same second as a write.

- `anubis/tests/billing_wallet_flow.rs` covers the wallet, against a stateful mock that holds payments and refunds, charges saved cards off-session with Stripe's idempotency cache, and can decline, lose an answer, or save a server error under a key: a top-up opened at Checkout with its card saved, credited once from Stripe's received amount however its four events arrive and repeat; spends that replay by key, refuse a reused key, drain to exactly zero, and move the organization from the free plan to the paid plan and back; a refund processed before its own payment's credit, a failed refund credited back once, and a refund past what was left taking the balance below zero; reconciliation crediting a top-up whose events never arrived; the billing roles alone reading and spending, another organization answering not found; and Stripe unset leaving reads answering, writes `503`, and held credit still spendable. The races run for real: forty spends and five credits on one wallet leaving the balance equal to the ledger's sum and every entry chaining from the one before; ten spends landing below the threshold arming one recharge, and two workers running it charging the card once; a declined card turning auto-recharge off and never being charged again; and a recharge whose answer was lost found by its attempt rather than charged twice, one answered with a saved server error retried under a fresh key.

- `anubis/tests/billing_limits_flow.rs` covers the limits: an invitation past the free plan's seats refused with the plan named, a re-invitation of somebody who already holds a seat costing nothing, a soft limit letting a record through where a hard one refuses (both against a real subscription row, so plan resolution is exercised rather than assumed), a membership change queueing `SyncSeats` only when a price is per-seat, the job telling a stateful mock Stripe the new quantity exactly once, a seat update Stripe refused sent under a fresh key on the job's next attempt rather than replayed, a retry after a lost answer sending nothing once the subscription is over, and a seat update answered by a stale replay retried under a fresh key.

The lifecycle suite proves the retrieve-rather-than-trust decision directly: the event bodies it delivers carry a status the mock no longer agrees with, and the row that lands is the mock's.

All five use `TestDatabase`, so each narrative owns a database and can count rows; see [testing.md](testing.md). On the frontend, `BillingPage.test.tsx` renders the screen against a stubbed `fetch` in three states: billing disabled, the free plan with its grid, and a live subscription set to cancel.

## The plan catalog in the frontend

```
anubis billing generate-ts --file config/billing.yml --out frontend/src/plans.generated.ts
```

The fourth generator, beside `roles generate-ts` and the two clients, and it follows the same discipline: deterministic output in house style, committed by the application, regenerated in CI and compared with `git diff --exit-code`. A `billing.yml` edit that was never regenerated fails there instead of in review.

It emits the ordered plans with their prices, limits, `highlighted`, and `free` flags, plus `PlanKey`, `FREE_PLAN_KEY`, and `findPlan(key)`. The pricing grid reads that file rather than an endpoint, because the catalog is configuration compiled into the same build: asking the server for it would be a request for a file that shipped with the page. The API's `plan` is a different thing and stays an endpoint, because it is the one plan an organization is actually on.

## The billing screen

`/organizations/:organizationId/billing` in the starter, which is where Stripe returns the browser, so a cold load has to work (and is why the path avoids `/billing`, a reserved API prefix). The organization's settings page links to it, and so does the tenancy menu.

It renders in one shape with a few states:

| State | What the screen shows |
|---|---|
| Billing disabled | The current plan card, and a card naming `STRIPE_SECRET_KEY` instead of a pricing grid. Nothing that cannot work is offered. |
| Free plan | The plan, its seat usage, no subscription chip, and the full grid with the free plan marked current |
| Active subscription | A status chip, the renewal date, seat usage against the plan's limit, the customer portal button, and the grid with the bought plan marked and unbuyable |
| `past_due` | An amber chip and a line saying a payment failed and the card can be updated in the portal. Access is not cut off, because Stripe is still retrying |
| Cancelling | "Access ends" rather than "Renews", plus a line saying the subscription stops at the end of the period |
| Over the seat limit | A warning line under the seat count, which is where a soft limit shows itself |
| Back from Stripe | `?checkout=success` or `?checkout=canceled` opens the page with the matching sentence, and `?top_up=success` or `?top_up=canceled` after a top-up. None is trusted: the subscription or the credit appears when Stripe's event arrives |
| Selling credit | A credit card with the balance, a low-balance or below-zero warning, the auto-recharge settings, a warning when a declined recharge turned it off, the "Add credit" and "Auto-recharge" dialogs for `admin` or `billing` while billing is configured, and a link to the ledger at `/organizations/:organizationId/billing/activity`, paginated, newest first |
| Paid status | The plan the paid plan resolution put in force, with a line saying it is in force because the organization pays for something |

The screen renders the plan subscription. The subscriptions an application sells per record arrive in the same response, for the application to render where each record lives rather than on a page that cannot name them.

Reading is open to every member of the organization, because the plan explains what the whole organization can do. The checkout buttons, the portal link, and the "refresh from Stripe" action are drawn only for `admin` or `billing`, read from the compiled `roles.generated.ts` through `permissions.ts`, which is the same authority the endpoints enforce.

The refresh action is `POST .../reconcile`. It exists on the screen because the one thing a customer does after paying is come back and look, and an event that has not landed yet is the one moment the page can be wrong.

## What this module deliberately leaves out

- **Everything about money.** No card form, no plan-change screen, no invoice list, no tax handling, no dunning email. Stripe Checkout takes the purchase and the customer portal takes every change after it, including plan changes, cancellations, and payment methods. What this framework keeps is the projection it has to authorize from.
- **Proration policy.** Seat changes ride Stripe's default (`create_prorations`). Anything else is a setting on the account.
- **Notifying a customer whose card failed.** `past_due` reaches the row and the screen says so, but nobody is emailed. The event to hang that on is `invoice.payment_failed`, and the surface it needs is notification rather than billing.
- **Time-based usage limits.** Bullet Train's usage trackers meter verbs over a window ("one publish per three days"). Anubis meters counts. A window needs a tracker table and a cycling schedule, which is a larger feature than a limit and is not obviously worth it.
- **A periodic reconciliation sweep.** `Reconciler::reconcile` is written to be called from a schedule; there are no recurring schedules yet. See [jobs.md](jobs.md#roadmap).
- **Changing a plan subscription's items from the application.** The portal changes a plan subscription, and `Subscriptions` changes the subscriptions an application sells per record; add-ons on the plan subscription itself go through the portal.
- **Telling the application a record's subscription changed.** A subscription cancelled in the portal, or one Stripe gave up collecting, reaches the projection and the screen; an application that acts on it (stopping the record it paid for) reads `Subscription::live_for_subject` where it decides. A hook the lifecycle calls on each change is future work.
- **Usage reporting for metered prices.** A metered item is projected, with no quantity; reporting usage to Stripe is the application's.

- **Credit beyond top-ups, recharges, and refunds.** No promotional grants, no expiry, no transfers between organizations, no overdraft. A grant is a ledger kind and an audited endpoint, worth building the day an application gives credit away.
- **Disputes.** A chargeback on a top-up reaches Stripe's dashboard, not the ledger: what to debit, and when, is a decision a person makes.
- **Tax on credit.** Top-ups carry no tax calculation. Whether prepaid credit is taxed at purchase or at use is a jurisdiction's question, not the framework's.
- **Managing saved cards.** A top-up saves the card auto-recharge charges, and the customer portal removes it; there is no card list here.
