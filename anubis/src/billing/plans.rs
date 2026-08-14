//! Plan definitions compiled from `config/billing.yml`.
//!
//! One YAML file defines what an application sells: the plans, the Stripe
//! price behind each billing interval, and the limits a plan grants.
//! [`PlanSet::from_yaml`] parses and validates it, so an application that
//! embeds the file with `include_str!` fails at boot (and its tests fail in
//! CI) on any bad edit, exactly the way `config/roles.yml` does.
//!
//! ```yaml
//! plans:
//!   - key: free
//!     name: Free
//!     limits:
//!       seats: 3
//!   - key: pro
//!     name: Pro
//!     highlighted: true
//!     prices:
//!       monthly:
//!         stripe_price_id: price_1QpbQSKKAAAAAAAAAAAAAAAA
//!         amount: 2900
//!         currency: usd
//!     limits:
//!       seats: 25
//! ```
//!
//! # The free plan
//!
//! Exactly one plan carries no prices, and that is the free plan. It is what
//! an organization with no subscription resolves to, which makes plan
//! resolution total: every organization is always on a plan, and no code path
//! has to answer "what does an unsubscribed organization get".
//!
//! # Plans do not inherit
//!
//! Roles include other roles, and resolving them needs cycle detection. Plans
//! deliberately do not: each one states its limits in full, because a pricing
//! table is read across, and a limit inherited from a plan three rows up is a
//! limit nobody can see. There is no cycle to detect here, only duplicate keys
//! and prices, which is what validation rejects.
//!
//! # Amounts
//!
//! `amount` and `currency` are display metadata for the pricing page, in the
//! currency's smallest unit (2900 is $29.00). Stripe's price is the source of
//! truth for what a customer is actually charged; nothing here is sent to
//! Stripe, only `stripe_price_id` is. Keeping the amount in the file is what
//! lets a pricing page render without an API call.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

/// How often a price recurs.
///
/// The two intervals a SaaS actually sells. Stripe spells them `month` and
/// `year` on its own objects; [`Interval::stripe_interval`] is that
/// translation, needed when reading a subscription back rather than when
/// buying one, since a checkout names a price id and the price knows its own
/// recurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    /// Billed every month.
    Monthly,
    /// Billed every year.
    Yearly,
}

impl Interval {
    /// Both intervals, in the order a pricing page offers them.
    pub const ALL: [Self; 2] = [Self::Monthly, Self::Yearly];

    /// The interval's spelling in `billing.yml`, in the API, and in the column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    /// Stripe's spelling of the same recurrence.
    #[must_use]
    pub fn stripe_interval(self) -> &'static str {
        match self {
            Self::Monthly => "month",
            Self::Yearly => "year",
        }
    }

    /// Reads an interval written either way, ours or Stripe's.
    ///
    /// Both spellings are accepted because both arrive: `monthly` from a
    /// request body and from the stored column, `month` from a Stripe object.
    ///
    /// # Examples
    /// ```
    /// use anubis::billing::Interval;
    ///
    /// assert_eq!(Interval::parse("yearly"), Some(Interval::Yearly));
    /// assert_eq!(Interval::parse("year"), Some(Interval::Yearly));
    /// assert_eq!(Interval::parse("weekly"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "monthly" | "month" => Some(Self::Monthly),
            "yearly" | "year" => Some(Self::Yearly),
            _unknown => None,
        }
    }
}

impl Display for Interval {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One price of one plan: what Stripe charges, and what the page shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    stripe_price_id: String,
    amount: i64,
    currency: String,
}

impl Price {
    /// The Stripe price this interval buys, e.g. `price_1Qpb...`.
    ///
    /// Not a secret: it identifies a public product and appears in Stripe's own
    /// browser-side code.
    #[must_use]
    pub fn stripe_price_id(&self) -> &str {
        &self.stripe_price_id
    }

    /// The displayed amount, in the currency's smallest unit.
    #[must_use]
    pub fn amount(&self) -> i64 {
        self.amount
    }

    /// The ISO 4217 currency code, lowercase, as Stripe writes it.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }
}

/// One plan an application sells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    key: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    highlighted: bool,
    #[serde(default)]
    prices: BTreeMap<Interval, Price>,
    #[serde(default)]
    limits: BTreeMap<String, i64>,
}

impl Plan {
    /// The plan's stable key, stored on subscriptions, e.g. `pro`.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The plan's display name, e.g. `Pro`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The one-line pitch under the name, when the file gives one.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Whether the pricing page should make this plan stand out.
    #[must_use]
    pub fn is_highlighted(&self) -> bool {
        self.highlighted
    }

    /// Every price this plan sells, keyed by interval.
    #[must_use]
    pub fn prices(&self) -> &BTreeMap<Interval, Price> {
        &self.prices
    }

    /// The price for one interval, when the plan sells that interval.
    #[must_use]
    pub fn price(&self, interval: Interval) -> Option<&Price> {
        self.prices.get(&interval)
    }

    /// Every limit this plan grants, keyed by the application's own vocabulary.
    #[must_use]
    pub fn limits(&self) -> &BTreeMap<String, i64> {
        &self.limits
    }

    /// The limit this plan puts on `name`, or `None` for no limit at all.
    ///
    /// A limit the plan does not name is unlimited. That is the convention
    /// rather than a magic number, because "unlimited" written as `-1` is a
    /// value every caller has to remember to special-case.
    #[must_use]
    pub fn limit(&self, name: &str) -> Option<i64> {
        self.limits.get(name).copied()
    }

    /// Whether this is the free plan, which is the plan that sells no price.
    #[must_use]
    pub fn is_free(&self) -> bool {
        self.prices.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BillingFile {
    plans: Vec<Plan>,
}

/// The validated plans of an application, in the order the file lists them.
///
/// File order is presentation order: a pricing page renders
/// [`PlanSet::plans`] as it stands, so the cheapest-first reading of the YAML
/// is the reading a customer gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSet {
    plans: Vec<Plan>,
    /// Index of the one plan with no prices; see the module docs.
    free: usize,
}

impl PlanSet {
    /// Parses and validates a `billing.yml` document.
    ///
    /// # Errors
    /// Returns an [`Error`] on malformed YAML, an empty plan list, a duplicate
    /// plan key, a malformed key, limit name, currency, or amount, a Stripe
    /// price id used by two plans, or a plan list without exactly one free
    /// plan.
    ///
    /// # Examples
    /// ```
    /// use anubis::billing::PlanSet;
    ///
    /// let plans = PlanSet::from_yaml("
    /// plans:
    ///   - key: free
    ///     name: Free
    ///     limits:
    ///       seats: 3
    /// ")?;
    ///
    /// assert_eq!(plans.free().key(), "free");
    /// # Ok::<(), anubis::billing::plans::Error>(())
    /// ```
    pub fn from_yaml(yaml: &str) -> Result<Self, Error> {
        let file: BillingFile = serde_norway::from_str(yaml)
            .map_err(|source| Error::new(format!("billing.yml does not parse: {source}")))?;

        if file.plans.is_empty() {
            return Err(Error::new("billing.yml defines no plans".to_owned()));
        }

        let mut keys = BTreeSet::new();
        let mut price_ids = BTreeSet::new();
        let mut free = Vec::new();

        for (index, plan) in file.plans.iter().enumerate() {
            validate_plan(plan)?;

            if !keys.insert(plan.key.as_str()) {
                return Err(Error::new(format!(
                    "billing.yml defines plan {:?} twice",
                    plan.key
                )));
            }
            for price in plan.prices.values() {
                if !price_ids.insert(price.stripe_price_id.as_str()) {
                    return Err(Error::new(format!(
                        "plan {:?} reuses Stripe price {:?}, which another plan already sells; \
                         a price identifies the plan a subscription is on, so each one may \
                         belong to a single plan",
                        plan.key, price.stripe_price_id,
                    )));
                }
            }
            if plan.is_free() {
                free.push(index);
            }
        }

        match free.as_slice() {
            [only] => Ok(Self {
                plans: file.plans,
                free: *only,
            }),
            [] => Err(Error::new(
                "billing.yml defines no free plan: exactly one plan must sell no prices, \
                 because that is what an organization without a subscription is on"
                    .to_owned(),
            )),
            [first, rest @ ..] => Err(Error::new(format!(
                "billing.yml defines {} free plans ({:?} and {:?}): exactly one plan may sell \
                 no prices",
                rest.len() + 1,
                file.plans[*first].key,
                file.plans[rest[0]].key,
            ))),
        }
    }

    /// Every plan, in the order the file lists them.
    #[must_use]
    pub fn plans(&self) -> &[Plan] {
        &self.plans
    }

    /// The plan with this key, if the file defines one.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&Plan> {
        self.plans.iter().find(|plan| plan.key == key)
    }

    /// The free plan, which every plan set has exactly one of.
    #[must_use]
    pub fn free(&self) -> &Plan {
        // Validated at construction: `free` indexes the one plan with no prices.
        &self.plans[self.free]
    }

    /// The plan and interval a Stripe price belongs to.
    ///
    /// This is how an incoming subscription event resolves the plan it bought:
    /// Stripe names a price, and validation guarantees at most one plan sells
    /// it.
    #[must_use]
    pub fn find_by_price_id(&self, stripe_price_id: &str) -> Option<(&Plan, Interval)> {
        self.plans.iter().find_map(|plan| {
            plan.prices
                .iter()
                .find(|(_interval, price)| price.stripe_price_id == stripe_price_id)
                .map(|(interval, _price)| (plan, *interval))
        })
    }
}

/// Checks one plan's own fields, before the set-wide rules run.
fn validate_plan(plan: &Plan) -> Result<(), Error> {
    if !is_config_key(&plan.key) {
        return Err(Error::new(format!(
            "plan key {:?} is not usable: keys are lowercase letters, digits, and underscores, \
             e.g. `pro` or `team_plus`, because they are stored on subscriptions and read by \
             the frontend",
            plan.key,
        )));
    }
    if plan.name.trim().is_empty() {
        return Err(Error::new(format!("plan {:?} has no name", plan.key)));
    }

    for (interval, price) in &plan.prices {
        if price.stripe_price_id.trim().is_empty() {
            return Err(Error::new(format!(
                "plan {:?} has no stripe_price_id for its {interval} price",
                plan.key,
            )));
        }
        if price.amount < 0 {
            return Err(Error::new(format!(
                "plan {:?} has a negative {interval} amount: amounts are in the currency's \
                 smallest unit and cannot be below zero",
                plan.key,
            )));
        }
        if price.currency.len() != 3 || !price.currency.chars().all(|c| c.is_ascii_lowercase()) {
            return Err(Error::new(format!(
                "plan {:?} has currency {:?} for its {interval} price: expected a lowercase \
                 ISO 4217 code, e.g. `usd`",
                plan.key, price.currency,
            )));
        }
    }

    for (name, count) in &plan.limits {
        if !is_config_key(name) {
            return Err(Error::new(format!(
                "plan {:?} names limit {name:?}: limit names are lowercase letters, digits, and \
                 underscores, e.g. `seats` or `projects`",
                plan.key,
            )));
        }
        if *count < 0 {
            return Err(Error::new(format!(
                "plan {:?} limits {name:?} to {count}: a limit cannot be negative, and a limit \
                 the plan does not name is unlimited",
                plan.key,
            )));
        }
    }

    Ok(())
}

/// Whether a word may be a plan key or a limit name.
///
/// The same shape both times, because both cross into JSON and into generated
/// TypeScript, where a hyphen or a space would need quoting at every use.
fn is_config_key(candidate: &str) -> bool {
    !candidate.is_empty()
        && !candidate.starts_with('_')
        && !candidate.ends_with('_')
        && candidate.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

/// A `billing.yml` parsing or validation failure.
#[derive(Debug)]
pub struct Error {
    message: String,
    backtrace: Backtrace,
}

impl Error {
    fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
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
    use super::{Interval, PlanSet};

    const BASELINE: &str = "
plans:
  - key: free
    name: Free
    description: Everything one person needs.
    limits:
      seats: 1
      projects: 3
  - key: pro
    name: Pro
    highlighted: true
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
      yearly:
        stripe_price_id: price_pro_yearly
        amount: 29000
        currency: usd
    limits:
      seats: 25
";

    #[test]
    fn a_plan_set_keeps_file_order_and_reads_every_field() {
        let plans = PlanSet::from_yaml(BASELINE).expect("the baseline must parse");

        let keys: Vec<&str> = plans.plans().iter().map(super::Plan::key).collect();
        assert_eq!(keys, ["free", "pro"], "file order is presentation order");

        let free = plans.free();
        assert_eq!(free.key(), "free");
        assert!(free.is_free());
        assert_eq!(free.description(), Some("Everything one person needs."));
        assert_eq!(free.limit("projects"), Some(3));
        assert_eq!(
            free.limit("webhooks"),
            None,
            "an unnamed limit is unlimited"
        );

        let pro = plans.find("pro").expect("pro must be defined");
        assert!(pro.is_highlighted());
        assert!(!pro.is_free());
        let monthly = pro.price(Interval::Monthly).expect("pro sells monthly");
        assert_eq!(monthly.stripe_price_id(), "price_pro_monthly");
        assert_eq!(monthly.amount(), 2900);
        assert_eq!(monthly.currency(), "usd");
        assert_eq!(pro.limit("seats"), Some(25));

        assert!(plans.find("enterprise").is_none());
    }

    #[test]
    fn a_stripe_price_resolves_back_to_its_plan_and_interval() {
        let plans = PlanSet::from_yaml(BASELINE).expect("the baseline must parse");

        let (plan, interval) = plans
            .find_by_price_id("price_pro_yearly")
            .expect("a configured price must resolve");
        assert_eq!(plan.key(), "pro");
        assert_eq!(interval, Interval::Yearly);

        assert!(plans.find_by_price_id("price_unknown").is_none());
    }

    #[test]
    fn intervals_read_both_spellings_and_render_ours() {
        assert_eq!(Interval::parse("MONTHLY"), Some(Interval::Monthly));
        assert_eq!(Interval::parse(" month "), Some(Interval::Monthly));
        assert_eq!(Interval::parse("yearly"), Some(Interval::Yearly));
        assert_eq!(Interval::parse("annual"), None);
        assert_eq!(Interval::Yearly.as_str(), "yearly");
        assert_eq!(Interval::Yearly.stripe_interval(), "year");
        assert_eq!(Interval::Monthly.to_string(), "monthly");
    }

    #[test]
    fn a_document_without_exactly_one_free_plan_is_rejected() {
        let none_free = "
plans:
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_pro_monthly
        amount: 2900
        currency: usd
";
        let error = PlanSet::from_yaml(none_free).expect_err("a free plan is required");
        assert!(error.to_string().contains("no free plan"), "got: {error}");

        let two_free = "
plans:
  - key: free
    name: Free
  - key: starter
    name: Starter
";
        let error = PlanSet::from_yaml(two_free).expect_err("one free plan at most");
        assert!(error.to_string().contains("2 free plans"), "got: {error}");
    }

    #[test]
    fn duplicate_keys_and_duplicate_prices_are_rejected() {
        let duplicate_key = "
plans:
  - key: free
    name: Free
  - key: free
    name: Free again
    prices:
      monthly:
        stripe_price_id: price_a
        amount: 100
        currency: usd
";
        let error = PlanSet::from_yaml(duplicate_key).expect_err("keys are unique");
        assert!(error.to_string().contains("twice"), "got: {error}");

        let duplicate_price = "
plans:
  - key: free
    name: Free
  - key: basic
    name: Basic
    prices:
      monthly:
        stripe_price_id: price_shared
        amount: 100
        currency: usd
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_shared
        amount: 200
        currency: usd
";
        let error = PlanSet::from_yaml(duplicate_price).expect_err("prices are unique");
        assert!(error.to_string().contains("price_shared"), "got: {error}");
    }

    #[test]
    fn malformed_keys_limits_and_prices_are_rejected() {
        const SPACED_KEY: &str = "
plans:
  - key: Pro Plan
    name: Pro
";
        const BLANK_NAME: &str = "
plans:
  - key: free
    name: '  '
";
        const SPACED_LIMIT: &str = "
plans:
  - key: free
    name: Free
    limits:
      Active Seats: 3
";
        const NEGATIVE_LIMIT: &str = "
plans:
  - key: free
    name: Free
    limits:
      seats: -1
";
        const NEGATIVE_AMOUNT: &str = "
plans:
  - key: free
    name: Free
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_a
        amount: -5
        currency: usd
";
        const SHOUTED_CURRENCY: &str = "
plans:
  - key: free
    name: Free
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: price_a
        amount: 100
        currency: DOLLARS
";
        const NO_PRICE_ID: &str = "
plans:
  - key: free
    name: Free
  - key: pro
    name: Pro
    prices:
      monthly:
        stripe_price_id: ''
        amount: 100
        currency: usd
";

        for (yaml, expected) in [
            (SPACED_KEY, "not usable"),
            (BLANK_NAME, "no name"),
            (SPACED_LIMIT, "limit names"),
            (NEGATIVE_LIMIT, "cannot be negative"),
            (NEGATIVE_AMOUNT, "cannot be below zero"),
            (SHOUTED_CURRENCY, "ISO 4217"),
            (NO_PRICE_ID, "no stripe_price_id"),
        ] {
            let error = PlanSet::from_yaml(yaml).expect_err("must be rejected");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in: {error}",
            );
        }
    }

    #[test]
    fn empty_and_malformed_documents_are_rejected() {
        PlanSet::from_yaml("plans: []").expect_err("an empty plan list is rejected");
        PlanSet::from_yaml("not yaml: [").expect_err("malformed yaml is rejected");
        PlanSet::from_yaml("plans:\n  - key: free\n    name: Free\n    typo: 1\n")
            .expect_err("unknown fields are rejected");
    }
}
