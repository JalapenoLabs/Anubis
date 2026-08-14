//! Planning one `anubis scaffold webhook <Provider>` run.
//!
//! This is the receiving half of webhooks, Bullet Train's
//! `super_scaffold:incoming_webhook` rebuilt on the Postgres job queue. What it
//! generates is one provider's endpoint: a table the requests are stored in,
//! the unauthenticated route that stores them, the signature check the
//! developer finishes, and the background job that processes them.
//!
//! The command takes the **provider**, not the model, because the provider is
//! the thing a person has an account with. The model's name follows from it:
//! `Stripe` becomes `StripeWebhook`, stored in `stripe_webhooks`, received at
//! `/webhooks/stripe`. Both names are rewritten at once, which is what lets one
//! template carry both the model's identifiers and the provider's own spelling
//! in its URL and its prose.
//!
//! [`WebhookScaffold`] performs no I/O, like the rest of the engine, so the
//! whole plan is testable as plain strings.
//!
//! ```
//! use anubis::scaffold::WebhookScaffold;
//!
//! let scaffold = WebhookScaffold::parse("Stripe")?;
//! assert_eq!(scaffold.model().pascal(), "StripeWebhook");
//! assert_eq!(scaffold.table(), "stripe_webhooks");
//! assert_eq!(scaffold.path(), "/webhooks/stripe");
//! # Ok::<(), anubis::scaffold::ScaffoldError>(())
//! ```

use super::error::ScaffoldError;
use super::inflect::Names;
use super::model::{MAX_WIDTH, ROUTER_INDENT, module_pairs, name_pairs};
use super::stamp::Replacements;

/// The suffix every incoming-webhook model's name carries.
///
/// A provider is not a model, and `Stripe` on its own would be a table of
/// Stripes. The suffix is what makes the generated names read as what they
/// hold, and naming it once here is what keeps the parser's refusal and the
/// generated name in step.
const MODEL_SUFFIX: &str = "Webhook";

/// The living template every `anubis scaffold webhook` run transforms.
///
/// It lives in `backend/src/scaffolding/hypothetically_remote/` and is compiled
/// and integration-tested with the rest of the starter, so it cannot rot. The
/// name follows the family's absurdly-abstract convention (`CreativeConcept`,
/// `TangibleThing`, `IncidentalLinkage`, `PeripheralNotion`): a two-word
/// `PascalCase` provider inside a two-word `snake_case` namespace carries
/// enough shape to transform into any real provider's name without ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebhookTemplate {
    provider: &'static str,
    model: &'static str,
    module: &'static str,
}

/// The one webhook template.
const TEMPLATE: WebhookTemplate = WebhookTemplate {
    provider: "HypotheticalSender",
    model: "HypotheticalSenderWebhook",
    module: "hypothetically_remote",
};

impl WebhookTemplate {
    /// The template provider's name, e.g. `HypotheticalSender`.
    #[must_use]
    pub fn provider(self) -> &'static str {
        self.provider
    }

    /// The template model's name, e.g. `HypotheticalSenderWebhook`.
    #[must_use]
    pub fn model(self) -> &'static str {
        self.model
    }

    /// The template module under `backend/src/scaffolding/`.
    #[must_use]
    pub fn module(self) -> &'static str {
        self.module
    }

    /// The template's table, e.g. `hypothetical_sender_webhooks`.
    ///
    /// # Panics
    /// Panics if a template name in this module is not a parseable model name,
    /// which is a bug in the framework rather than in an application.
    #[must_use]
    pub fn table(self) -> String {
        Names::parse(self.model)
            .expect("template names are valid")
            .snake_plural()
    }
}

/// One planned `anubis scaffold webhook <Provider>`.
#[derive(Debug, Clone)]
pub struct WebhookScaffold {
    provider: Names,
    model: Names,
}

impl WebhookScaffold {
    /// Plans a receiver for one provider.
    ///
    /// The provider is written the way it should read in code and in the URL,
    /// exactly as `anubis scaffold oauth` takes its provider: `stripe` gives
    /// `/webhooks/stripe`, and `SendGrid` gives `/webhooks/send-grid`.
    ///
    /// # Errors
    /// Returns an error when the name does not parse, or when it already ends
    /// in `Webhook`, which would generate a `StripeWebhookWebhook`.
    pub fn parse(provider: &str) -> Result<Self, ScaffoldError> {
        let provider = Names::parse(provider)?;
        let snake = provider.snake();
        if matches!(
            snake.rsplit('_').next().unwrap_or_default(),
            "webhook" | "webhooks"
        ) {
            return Err(ScaffoldError::new(format!(
                "name the provider, not the model: `{}` already ends in `{MODEL_SUFFIX}`, and the \
                 command appends it. `anubis scaffold webhook Stripe` generates `StripeWebhook`, \
                 stored in `stripe_webhooks`.",
                provider.pascal(),
            )));
        }

        let model = Names::parse(&format!("{}{MODEL_SUFFIX}", provider.pascal()))?;
        Ok(Self { provider, model })
    }

    /// The provider's names, e.g. `Stripe`.
    #[must_use]
    pub fn provider(&self) -> &Names {
        &self.provider
    }

    /// The model's names, e.g. `StripeWebhook`.
    #[must_use]
    pub fn model(&self) -> &Names {
        &self.model
    }

    /// The living template this scaffold transforms.
    #[must_use]
    pub fn template(&self) -> WebhookTemplate {
        TEMPLATE
    }

    /// The full rewrite from template names to this receiver's names.
    ///
    /// Two names are rewritten, the model's and the provider's, plus the module
    /// path. The model's variants are the longer patterns, and
    /// [`Replacements`] applies longest-first, so
    /// `hypothetical_sender_webhooks` becomes `stripe_webhooks` rather than
    /// `stripe_webhooks` twice over.
    #[must_use]
    pub fn replacements(&self) -> Replacements {
        let mut pairs = name_pairs(TEMPLATE.model, &self.model);
        pairs.extend(name_pairs(TEMPLATE.provider, &self.provider));
        pairs.extend(module_pairs(TEMPLATE.module, &self.module()));
        Replacements::new(pairs)
    }

    /// The application module the receiver lives in, e.g. `stripe_webhooks`.
    #[must_use]
    pub fn module(&self) -> String {
        self.model.snake_plural()
    }

    /// The table received requests are stored in, e.g. `stripe_webhooks`.
    #[must_use]
    pub fn table(&self) -> String {
        self.model.snake_plural()
    }

    /// The path the provider posts to, e.g. `/webhooks/stripe`.
    ///
    /// Keyed by the provider rather than by the model, because the URL is
    /// configuration a person types into somebody else's console.
    #[must_use]
    pub fn path(&self) -> String {
        format!("/webhooks/{}", self.provider.kebab())
    }

    /// The environment variable the signing secret is read from.
    #[must_use]
    pub fn signing_secret_var(&self) -> String {
        format!("{}_SECRET", self.model.screaming())
    }

    /// The migration directory name for `version`.
    #[must_use]
    pub fn migration_directory(&self, version: &str) -> String {
        format!("{version}_create_{}", self.table())
    }

    /// The module declaration inserted into `backend/src/lib.rs`.
    #[must_use]
    pub fn module_declaration(&self) -> String {
        format!("pub mod {};", self.module())
    }

    /// The router mount inserted into `webhooks_router`.
    ///
    /// Pre-wrapped the way `rustfmt` would wrap it when a long provider name
    /// pushes it past the formatter's width, so generated code is format-clean
    /// untouched.
    #[must_use]
    pub fn route_mount(&self) -> String {
        let module = self.module();
        let single = format!("router = router.merge({module}::router(pool.clone()));");
        if single.len() + ROUTER_INDENT <= MAX_WIDTH {
            single
        } else {
            format!("router = router.merge({module}::router(\n    pool.clone(),\n));")
        }
    }

    /// The job registration inserted into `register_jobs`.
    #[must_use]
    pub fn job_registration(&self) -> String {
        format!("worker = {}::register_jobs(pool, worker);", self.module())
    }
}

#[cfg(test)]
mod tests {
    use super::WebhookScaffold;

    #[test]
    fn a_provider_names_its_model_module_table_and_path() {
        let scaffold = WebhookScaffold::parse("Stripe").expect("a valid provider");

        assert_eq!(scaffold.provider().pascal(), "Stripe");
        assert_eq!(scaffold.model().pascal(), "StripeWebhook");
        assert_eq!(scaffold.module(), "stripe_webhooks");
        assert_eq!(scaffold.table(), "stripe_webhooks");
        assert_eq!(scaffold.path(), "/webhooks/stripe");
        assert_eq!(scaffold.signing_secret_var(), "STRIPE_WEBHOOK_SECRET");
        assert_eq!(scaffold.module_declaration(), "pub mod stripe_webhooks;");
        assert_eq!(
            scaffold.route_mount(),
            "router = router.merge(stripe_webhooks::router(pool.clone()));",
        );
        assert_eq!(
            scaffold.job_registration(),
            "worker = stripe_webhooks::register_jobs(pool, worker);",
        );
        assert_eq!(
            scaffold.migration_directory("2026-08-15-101112"),
            "2026-08-15-101112_create_stripe_webhooks",
        );
    }

    #[test]
    fn the_replacements_rewrite_the_model_and_the_provider() {
        let replacements = WebhookScaffold::parse("Stripe")
            .expect("a valid provider")
            .replacements();

        // The model's own identifiers, which are the longer patterns.
        assert_eq!(
            replacements.apply("struct HypotheticalSenderWebhook; // hypothetical_sender_webhooks"),
            "struct StripeWebhook; // stripe_webhooks",
        );
        assert_eq!(
            replacements.apply("HYPOTHETICAL_SENDER_WEBHOOK_SECRET"),
            "STRIPE_WEBHOOK_SECRET",
        );
        assert_eq!(
            replacements.apply("\"hypothetical_sender_webhook.process\""),
            "\"stripe_webhook.process\"",
        );
        // The provider's own spelling, in the URL and in prose.
        assert_eq!(
            replacements.apply("POST /webhooks/hypothetical-sender"),
            "POST /webhooks/stripe",
        );
        assert_eq!(
            replacements.apply("really came from Hypothetical Sender"),
            "really came from Stripe",
        );
        // And the module the generated code lives in.
        assert_eq!(
            replacements.apply("use crate::scaffolding::hypothetically_remote::router;"),
            "use crate::stripe_webhooks::router;",
        );
    }

    #[test]
    fn a_lowercase_provider_keeps_its_own_spelling() {
        let scaffold = WebhookScaffold::parse("github").expect("a valid provider");

        assert_eq!(scaffold.model().pascal(), "GithubWebhook");
        assert_eq!(scaffold.path(), "/webhooks/github");
        assert_eq!(scaffold.module(), "github_webhooks");
    }

    #[test]
    fn naming_the_model_instead_of_the_provider_is_refused() {
        for named in ["StripeWebhook", "stripe_webhooks", "Webhook"] {
            let error =
                WebhookScaffold::parse(named).expect_err("the command appends the suffix itself");
            assert!(
                error.message().contains("name the provider"),
                "unexpected message: {}",
                error.message(),
            );
        }

        WebhookScaffold::parse("").unwrap_err();
        WebhookScaffold::parse("9lives").unwrap_err();
    }
}
