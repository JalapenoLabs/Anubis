//! Application configuration loaded from the process environment.
//!
//! Anubis applications are configured twelve-factor style: plain environment
//! variables with sensible development defaults, so a freshly stamped app boots
//! with zero configuration. [`AppConfig::from_env`] reads the process
//! environment; [`AppConfig::from_lookup`] accepts any lookup function, which
//! keeps configuration fully testable without mutating global state.
//!
//! Recognized variables:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `ANUBIS_ENV` | `development` | One of `development`, `test`, `production` |
//! | `HOST` | `127.0.0.1` | IP address the server binds to |
//! | `PORT` | `3000` | TCP port the server binds to |
//! | `DATABASE_URL` | unset | Postgres connection URL, e.g. `postgres://user:pass@host/db` |
//! | `REDIS_URL` | unset | Redis connection URL, e.g. `redis://localhost:6379`; fans realtime channels out across instances |
//! | `APP_URL` | `http://<host>:<port>` | Public base URL used in email links |
//! | `ANUBIS_SECRET_KEY` | development key | Base64 for exactly 32 bytes; encrypts recoverable secrets at rest. Required in production |
//! | `SMTP_URL` | unset | SMTP relay, e.g. `smtps://user:password@smtp.example.com:465`; setting it delivers real email |
//! | `MAIL_FROM` | unset | Sender of outgoing email, e.g. `Acme <no-reply@acme.com>`. Required when `SMTP_URL` is set |
//! | `DKIM_PRIVATE_KEY` | unset | Private key that signs outgoing email; setting it signs every message |
//! | `DKIM_SELECTOR` | unset | Name the key is published under in DNS, e.g. `mail`. Required with `DKIM_PRIVATE_KEY` |
//! | `DKIM_DOMAIN` | the `MAIL_FROM` domain | Domain the signature claims responsibility for |
//! | `RATE_LIMIT_DISABLED` | `false` | `true` switches off the abuse limits on the auth endpoints |
//! | `PASSWORD_HASH_CONCURRENCY` | `64` | How many argon2 computations may run at once, each holding 19 MiB |
//! | `TRUSTED_PROXY_HEADER` | unset | Forwarding header a trusted proxy appends the client address to, e.g. `x-forwarded-for` |
//! | `SPA_DIR` | unset | Directory of built frontend assets to serve, e.g. `frontend/dist`; unset serves no frontend |
//! | `CORS_ALLOWED_ORIGINS` | unset | Comma-separated exact origins allowed to call the API from a browser, e.g. `https://app.example.com`; unset means same-origin only |
//! | `CSP_ALLOWED_SOURCES` | unset | Sources added to the content security policy, per directive, e.g. `script-src https://plausible.io; connect-src https://plausible.io` |
//! | `STRIPE_SECRET_KEY` | unset | Stripe secret key, e.g. `sk_live_...`; setting it enables billing |
//! | `STRIPE_WEBHOOK_SECRET` | unset | Secret Stripe signs billing events with, e.g. `whsec_...`; setting it enables the billing receiver |
//! | `STRIPE_API_BASE` | `https://api.stripe.com` | Where Stripe's API lives; overridden only by tests and mocks |
//!
//! Each OpenID Connect provider in [`crate::auth::oauth::known_providers`]
//! adds three more, named after the provider:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `GOOGLE_OAUTH_CLIENT_ID` | unset | OAuth client id; setting it and the secret enables the provider |
//! | `GOOGLE_OAUTH_CLIENT_SECRET` | unset | OAuth client secret |
//! | `GOOGLE_OAUTH_ISSUER` | `https://accounts.google.com` | Issuer whose discovery document configures the flow |
//!
//! The set grows milestone by milestone.
//!
//! # The secret key
//!
//! `ANUBIS_SECRET_KEY` keys [`crate::auth::secret_box`], which seals secrets
//! the application has to read back (today, TOTP seeds). Mint one with
//! `anubis secret generate`, which prints exactly the value this variable
//! takes; `openssl rand -base64 32` or any other source of 32 random bytes
//! rendered as base64 works too, and padding is optional. Production fails to
//! start without it. Development and test fall back to a fixed, public
//! development key so a freshly stamped application boots with no
//! configuration, and [`crate::telemetry::init`] warns whenever that fallback
//! is in use. `anubis doctor` reports the same two states: a warning when the
//! variable is unset, and a failure when it is set to something that cannot
//! be a key.
//!
//! ## Rotation
//!
//! Rotation is a one-way door: every value sealed under the old key stops
//! opening. That is the design rather than a limitation, because it makes
//! stolen ciphertext worthless the moment the key is replaced. Concretely, a
//! TOTP enrollment whose seed no longer opens is discarded and the user
//! enrolls a second factor again (see [`crate::auth::mfa`]), and any other
//! sealed secret has to be reissued the same way. Plan a rotation as user
//! communication, not just a deploy.
//!
//! Rotating without that cost needs a dual-key read path: a second variable
//! holding the previous key, tried when the current one fails, so values
//! re-seal under the new key as they are read. That is future work; today
//! there is one key.
//!
//! # Email delivery
//!
//! `SMTP_URL` is the switch: set it and [`crate::mail::Mailer::from_config`]
//! delivers over SMTP, in every environment. Both forms the relay world uses
//! are understood: `smtps://host:465` opens TLS immediately, while
//! `smtp://host:587?tls=required` connects in the clear and upgrades with
//! STARTTLS. Credentials ride in the URL and must be percent-encoded if they
//! contain URL syntax. The URL is a secret, so it is never echoed in errors
//! or logs.
//!
//! A relay needs a sender, so `MAIL_FROM` becomes required the moment
//! `SMTP_URL` is set. Leaving both unset keeps the log mailer, which is the
//! zero-configuration development default. Production without a relay boots
//! anyway, with a startup warning from [`crate::telemetry::init`]: a first
//! deploy that sends no email should not be blocked by mail configuration.
//! See `docs/email.md`.
//!
//! ## DKIM signing
//!
//! Relay providers sign on your behalf once the sender domain is verified with
//! them, so the three `DKIM_*` variables stay unset in most deployments. They
//! exist for the relay that does not sign: a company MTA, a sidecar, an
//! appliance. Setting them makes the framework sign every message itself,
//! which is additive, so a message signed twice is fine.
//!
//! The three are a group. `DKIM_PRIVATE_KEY` and `DKIM_SELECTOR` are required
//! together, and `DKIM_DOMAIN` defaults to the domain of the `MAIL_FROM`
//! address, so most deployments set two variables. Setting a `DKIM_DOMAIN`
//! that is neither the sender's domain nor one of its parents stops the boot:
//! the signature would verify and DMARC would still fail it, which is the
//! worst of both. Signing without `SMTP_URL` stops the boot too, because the
//! log mailer delivers nothing to sign.
//!
//! `DKIM_PRIVATE_KEY` holds a PKCS#1 RSA key in PEM form, or the base64 seed
//! of an ed25519 key; the leading `-----BEGIN` tells them apart. A PEM is
//! multi-line and plenty of places a deployment stores secrets hold one line,
//! so `\n` escapes are accepted and mean the same key. The key is a secret: it
//! is parsed once when the mailer is built, and never echoed in errors, logs,
//! or `Debug` output. `docs/email.md` carries the key generation, the DNS TXT
//! record, and the rotation steps.
//!
//! # Serving the frontend
//!
//! `SPA_DIR` points at the frontend's build output, which is what makes a
//! production deployment one binary: the same server that answers `/api` hands
//! the browser the compiled React app. Leaving it unset serves no frontend,
//! which is the development default, since the Vite dev server owns the
//! browser there. [`crate::telemetry::init`] warns when a production
//! deployment leaves it unset. The directory is validated when
//! [`crate::spa::Assets::new`] opens it, so a deploy that shipped without a
//! build fails at startup rather than at the first page load.
//!
//! # Realtime fanout
//!
//! `REDIS_URL` is the switch between the two realtime backends. Unset, the
//! default, realtime channels are served inside the process: a publish reaches
//! the browsers connected to this instance, which is everything a
//! single-instance deployment needs. Set, publishes travel through Redis
//! pub/sub, so every instance's subscribers hear them. Both `redis://` and
//! `rediss://` (TLS) are understood, credentials ride in the URL, and the URL
//! is a secret, so it is never echoed in errors or logs.
//!
//! Redis is a fanout here and nothing else. Nothing durable is stored in it,
//! and losing it costs live delivery until it returns, never data. See
//! [`crate::realtime`] and `docs/realtime.md`.
//!
//! # Rate limiting
//!
//! The auth endpoints carry per-client budgets by default, so nothing has to
//! be configured to have them. Two variables tune that:
//!
//! `RATE_LIMIT_DISABLED=true` switches the limits off. It exists for
//! development and for test suites that drive the auth endpoints hard from
//! one address; a deployment should never set it. The variable is read the
//! same way in every environment, because a limit that silently differs
//! between development and production is a limit nobody has tested.
//!
//! `TRUSTED_PROXY_HEADER` names the forwarding header that identifies the
//! client when the application sits behind a proxy, conventionally
//! `x-forwarded-for`. Leave it unset when the application terminates
//! connections itself: the socket peer address is then the client, and a
//! forwarding header would be attacker-supplied. Set it only when a proxy you
//! control appends to that header, because the limiter reads the last entry,
//! the one that proxy wrote. See [`crate::rate_limit`] and `docs/api.md`.
//!
//! `PASSWORD_HASH_CONCURRENCY` is the limit behind those budgets, and it is a
//! memory budget written as a count: each argon2 computation holds 19 MiB
//! while it runs, so the default of 64 caps password hashing near 1.2 GiB and
//! a request that finds every permit taken is shed as `503` rather than
//! queued. Lower it on a small instance, raise it only alongside the memory
//! limit it is derived from, and see [`crate::auth::password`] for why a bound
//! exists at all.
//!
//! # Cross-origin access
//!
//! `CORS_ALLOWED_ORIGINS` is the whole CORS surface: a comma-separated list of
//! exact origins, such as `https://app.example.com,https://admin.example.com`.
//! Unset, the default, sends no CORS headers at all, which is what a
//! same-origin deployment (the SPA served by this binary) wants. Each entry
//! must be a bare `scheme://host[:port]` with no path, query, credentials, or
//! wildcard, and is normalized at startup, so a typo fails the boot rather than
//! the first cross-origin call. See [`crate::server`] and `docs/server.md`.
//!
//! # The content security policy
//!
//! The framework sends a policy built for the frontend it ships, and
//! `CSP_ALLOWED_SOURCES` is how an application adds what its own third parties
//! need. It is written the way CSP is, one group per directive:
//!
//! ```sh
//! CSP_ALLOWED_SOURCES="script-src https://plausible.io; connect-src https://plausible.io"
//! ```
//!
//! Only the fetch directives take sources, and only sources naming a host, so a
//! misconfiguration cannot widen `default-src` or smuggle `'unsafe-eval'` into
//! `script-src`. Everything is validated at startup: a directive the framework
//! keeps, or a source that is not an origin, stops the boot rather than
//! disappearing quietly into a policy nobody reads until a widget is blank. See
//! [`crate::server`] and `docs/server.md`.
//!
//! # Billing
//!
//! `STRIPE_SECRET_KEY` is the switch. Set it and an organization can buy a
//! plan through Stripe Checkout and manage it in the customer portal; leave it
//! unset, the development default, and every organization is on the free plan
//! defined in `config/billing.yml`, the two write endpoints answer `503`
//! naming this variable, and reading the current plan keeps working. A
//! production deployment without it boots anyway, with a startup warning from
//! [`crate::telemetry::init`]: an application that does not charge yet should
//! not be blocked on billing configuration. The key is a bearer credential for
//! an account that moves money, so it is never echoed in errors or logs.
//!
//! `STRIPE_WEBHOOK_SECRET` is the secret Stripe signs its events with, copied
//! from the endpoint's page in the Stripe dashboard. The billing receiver
//! verifies every request against it and refuses the ones that do not check
//! out, so an unset secret means the receiver answers `503` and subscriptions
//! stop being kept current. It is only read when `STRIPE_SECRET_KEY` is set,
//! and setting it alone stops the boot: the receiver reads each subscription
//! back from Stripe, which needs the key.
//!
//! `STRIPE_API_BASE` points the client somewhere other than Stripe. It exists
//! for tests and Stripe-compatible mocks; a deployment leaves it unset. See
//! [`crate::billing`] and `docs/billing.md`.
//!
//! # OAuth providers
//!
//! A provider is enabled by setting both its client id and its client secret;
//! setting one without the other is a misconfiguration and refuses to start,
//! because a half-configured provider is a sign-in button that always fails.
//! The issuer variable overrides the registry's issuer, which is what a
//! self-hosted or single-tenant deployment needs. Register the redirect URI
//! `<APP_URL>/auth/oauth/<provider>/callback` with the provider; the sign-in
//! page renders whichever providers are enabled here, so nothing else is
//! needed. `anubis scaffold oauth <provider>` prints both steps.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZero;
use std::path::PathBuf;

use axum::http::HeaderName;
use url::{Origin, Url};

use crate::auth::oauth::{OauthProviderConfig, known_providers};
use crate::auth::password;
use crate::auth::secret_box::SecretKey;
use crate::rate_limit::RateLimitConfig;
use crate::server::{self, CorsConfig, CspConfig};

/// Selects the runtime environment.
const ENV_VAR: &str = "ANUBIS_ENV";

/// IP address the server binds to.
const HOST_VAR: &str = "HOST";

/// TCP port the server binds to.
const PORT_VAR: &str = "PORT";

/// Postgres connection URL.
const DATABASE_URL_VAR: &str = "DATABASE_URL";

/// Redis connection URL, which fans realtime channels out across instances.
const REDIS_URL_VAR: &str = "REDIS_URL";

/// What a valid `REDIS_URL` looks like, quoted back in errors.
const REDIS_URL_FORM: &str =
    "a redis:// or rediss:// connection URL, e.g. `redis://localhost:6379`";

/// Public base URL of the application, used when building email links.
const APP_URL_VAR: &str = "APP_URL";

/// Base64 key that encrypts recoverable secrets at rest.
const SECRET_KEY_VAR: &str = "ANUBIS_SECRET_KEY";

/// What a valid `ANUBIS_SECRET_KEY` looks like, quoted back in errors.
const SECRET_KEY_FORM: &str = "base64 for exactly 32 random bytes, e.g. `openssl rand -base64 32`";

/// SMTP relay URL, credentials included.
const SMTP_URL_VAR: &str = "SMTP_URL";

/// What a valid `SMTP_URL` looks like, quoted back in errors.
const SMTP_URL_FORM: &str =
    "an smtp:// or smtps:// relay URL, e.g. `smtps://user:password@smtp.example.com:465`";

/// Sender address of outgoing email.
const MAIL_FROM_VAR: &str = "MAIL_FROM";

/// What a valid `MAIL_FROM` looks like, quoted back in errors.
const MAIL_FROM_FORM: &str =
    "an email address with an optional display name, e.g. `Acme <no-reply@acme.com>`";

/// Private key that signs outgoing email.
const DKIM_PRIVATE_KEY_VAR: &str = "DKIM_PRIVATE_KEY";

/// What a valid `DKIM_PRIVATE_KEY` looks like, quoted back in errors.
const DKIM_PRIVATE_KEY_FORM: &str = "a PKCS#1 RSA private key in PEM form, from `openssl genrsa -traditional -out dkim.pem \
     2048`, with `\\n` escapes where the value must be one line, or the base64 seed of an \
     ed25519 key";

/// Name the signing key is published under in DNS.
const DKIM_SELECTOR_VAR: &str = "DKIM_SELECTOR";

/// What a valid `DKIM_SELECTOR` looks like, quoted back in errors.
const DKIM_SELECTOR_FORM: &str = "the name the key is published under in DNS, e.g. `mail`";

/// Domain the signature claims responsibility for.
const DKIM_DOMAIN_VAR: &str = "DKIM_DOMAIN";

/// What a valid `DKIM_DOMAIN` looks like, quoted back in errors.
const DKIM_DOMAIN_FORM: &str =
    "the MAIL_FROM domain or one of its parents, e.g. `acme.com` for `no-reply@mail.acme.com`";

/// The variables that ask for DKIM signing, which is a group or nothing.
const DKIM_VARS: [&str; 3] = [DKIM_PRIVATE_KEY_VAR, DKIM_SELECTOR_VAR, DKIM_DOMAIN_VAR];

/// Directory of built frontend assets the server hands the browser.
const SPA_DIR_VAR: &str = "SPA_DIR";

/// Switches off the per-client budgets on the abuse-prone endpoints.
const RATE_LIMIT_DISABLED_VAR: &str = "RATE_LIMIT_DISABLED";

/// Names the forwarding header that identifies the client behind a proxy.
const TRUSTED_PROXY_HEADER_VAR: &str = "TRUSTED_PROXY_HEADER";

/// Caps how many argon2 computations may hold memory at once.
const PASSWORD_HASH_CONCURRENCY_VAR: &str = "PASSWORD_HASH_CONCURRENCY";

/// What a valid `PASSWORD_HASH_CONCURRENCY` looks like, quoted back in errors.
const PASSWORD_HASH_CONCURRENCY_FORM: &str =
    "a positive number of concurrent hashes, each holding 19 MiB, e.g. `64`";

/// Lists the origins allowed to call the application from a browser.
const CORS_ALLOWED_ORIGINS_VAR: &str = "CORS_ALLOWED_ORIGINS";

/// What a valid `CORS_ALLOWED_ORIGINS` entry looks like, quoted back in errors.
const ORIGIN_FORM: &str =
    "comma-separated exact origins with no path and no wildcard, e.g. `https://app.example.com`";

/// Adds sources to the content security policy the framework sends.
const CSP_ALLOWED_SOURCES_VAR: &str = "CSP_ALLOWED_SOURCES";

/// What a valid `CSP_ALLOWED_SOURCES` group looks like, quoted back in errors.
const CSP_GROUP_FORM: &str = "semicolon-separated groups of a directive and its sources, e.g. \
     `script-src https://plausible.io; connect-src https://plausible.io`";

/// Which directives `CSP_ALLOWED_SOURCES` may name, quoted back in errors.
const CSP_DIRECTIVE_FORM: &str = "one of script-src, style-src, img-src, font-src, connect-src, frame-src, media-src; \
     default-src, base-uri, form-action, and frame-ancestors are the framework's";

/// What a valid `CSP_ALLOWED_SOURCES` source looks like, quoted back in errors.
const CSP_SOURCE_FORM: &str = "an http, https, ws, or wss origin, optionally wildcarded one label \
     deep, e.g. `https://plausible.io` or `https://*.example.com`";

/// The Stripe secret key that enables billing.
const STRIPE_SECRET_KEY_VAR: &str = "STRIPE_SECRET_KEY";

/// What a valid `STRIPE_SECRET_KEY` looks like, quoted back in errors.
const STRIPE_SECRET_KEY_FORM: &str =
    "a Stripe secret key, e.g. `sk_live_...`, or a restricted key, `rk_live_...`";

/// Where Stripe's API lives, for tests and Stripe-compatible mocks.
const STRIPE_API_BASE_VAR: &str = "STRIPE_API_BASE";

/// What a valid `STRIPE_API_BASE` looks like, quoted back in errors.
const STRIPE_API_BASE_FORM: &str = "an http(s) base URL, e.g. `https://api.stripe.com`";

/// The secret Stripe signs webhook requests to the billing receiver with.
const STRIPE_WEBHOOK_SECRET_VAR: &str = "STRIPE_WEBHOOK_SECRET";

/// What a valid `STRIPE_WEBHOOK_SECRET` looks like, quoted back in errors.
const STRIPE_WEBHOOK_SECRET_FORM: &str = "a Stripe webhook signing secret, e.g. `whsec_...`, from the endpoint's page in the \
     Stripe dashboard";

/// What a valid `TRUSTED_PROXY_HEADER` looks like, quoted back in errors.
const HEADER_NAME_FORM: &str = "an HTTP header name, e.g. `x-forwarded-for`";

/// What a valid boolean variable looks like, quoted back in errors.
const BOOLEAN_FORM: &str = "one of true, false, 1, 0, yes, no";

/// Loopback keeps development servers off the network unless opted in.
const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Matches the port the starter frontend proxies to during development.
const DEFAULT_PORT: u16 = 3000;

/// The runtime environment an application is running in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Environment {
    /// Local development, the default when `ANUBIS_ENV` is unset.
    #[default]
    Development,
    /// Automated test runs.
    Test,
    /// Live deployments.
    Production,
}

impl Environment {
    /// Returns `true` in local development.
    #[must_use]
    pub fn is_development(self) -> bool {
        self == Self::Development
    }

    /// Returns `true` in automated test runs.
    #[must_use]
    pub fn is_test(self) -> bool {
        self == Self::Test
    }

    /// Returns `true` in live deployments.
    #[must_use]
    pub fn is_production(self) -> bool {
        self == Self::Production
    }
}

impl Display for Environment {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        };
        f.write_str(name)
    }
}

/// Where the application server binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerConfig {
    /// IP address the server binds to.
    pub host: IpAddr,
    /// TCP port the server binds to.
    pub port: u16,
}

impl ServerConfig {
    /// Returns the address the server should bind to.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

/// Postgres connection settings.
///
/// The URL embeds credentials, so this type never exposes it through `Debug`
/// or `Display`; read it deliberately with [`DatabaseConfig::url`].
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    url: String,
}

impl DatabaseConfig {
    /// Returns the Postgres connection URL, credentials included.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseConfig(...)")
    }
}

/// Redis connection settings for the realtime fanout.
///
/// The URL may embed credentials, so this type never exposes it through
/// `Debug` or `Display`; read it deliberately with [`RedisConfig::url`].
#[derive(Clone, PartialEq, Eq)]
pub struct RedisConfig {
    url: String,
}

impl RedisConfig {
    /// Returns the Redis connection URL, credentials included.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("RedisConfig(...)")
    }
}

/// SMTP relay settings for outgoing email.
///
/// The URL embeds credentials, so this type never exposes it through `Debug`;
/// read it deliberately with [`SmtpConfig::url`].
#[derive(Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    url: String,
    from: String,
    dkim: Option<DkimConfig>,
}

impl SmtpConfig {
    /// Returns the relay URL, credentials included.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the sender address every outgoing email carries.
    #[must_use]
    pub fn from(&self) -> &str {
        &self.from
    }

    /// Returns the signing settings, when the deployment signs its own mail.
    ///
    /// Absent, the relay's own signature is the only one, which is what a
    /// verified sender domain at a relay provider gives you; see the module
    /// docs and [`crate::mail::Mailer::smtp_signed`].
    #[must_use]
    pub fn dkim(&self) -> Option<&DkimConfig> {
        self.dkim.as_ref()
    }
}

impl fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("from", &self.from)
            .field("dkim", &self.dkim)
            .finish_non_exhaustive()
    }
}

/// DKIM signing settings for outgoing email.
///
/// The key is a secret, so this type never exposes it through `Debug`; read it
/// deliberately with [`DkimConfig::private_key`].
#[derive(Clone, PartialEq, Eq)]
pub struct DkimConfig {
    private_key: String,
    selector: String,
    domain: String,
}

impl DkimConfig {
    /// Returns the signing key: a PKCS#1 PEM, or a base64 ed25519 seed.
    ///
    /// Newline escapes are already resolved, so the value is exactly what the
    /// key parser expects.
    #[must_use]
    pub fn private_key(&self) -> &str {
        &self.private_key
    }

    /// Returns the selector the key is published under in DNS.
    #[must_use]
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Returns the domain the signature claims responsibility for.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

impl fmt::Debug for DkimConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("DkimConfig")
            .field("selector", &self.selector)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

/// Stripe credentials for the billing module.
///
/// Both values are bearer credentials for an account that can move money, so
/// this type never exposes either through `Debug`; read them deliberately with
/// [`StripeConfig::secret_key`] and [`StripeConfig::webhook_secret`].
#[derive(Clone, PartialEq, Eq)]
pub struct StripeConfig {
    secret_key: String,
    api_base: String,
    webhook_secret: Option<String>,
}

impl StripeConfig {
    /// Returns the Stripe secret key.
    #[must_use]
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    /// Returns the API base URL, without a trailing slash.
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Returns the secret Stripe signs webhook requests with, when set.
    ///
    /// Absent, the billing receiver refuses every request with `503` rather
    /// than believing an unsigned one, and subscriptions stop being kept
    /// current. See [`crate::billing`].
    #[must_use]
    pub fn webhook_secret(&self) -> Option<&str> {
        self.webhook_secret.as_deref()
    }
}

impl fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("StripeConfig")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// Top-level application configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    /// The runtime environment.
    pub environment: Environment,
    /// Where the application server binds.
    pub server: ServerConfig,
    /// Postgres connection settings, when `DATABASE_URL` is set.
    pub database: Option<DatabaseConfig>,
    /// Redis connection settings, when `REDIS_URL` is set.
    ///
    /// Present fans realtime channels out across instances, absent serves them
    /// inside the process; see the module docs and [`crate::realtime`].
    pub redis: Option<RedisConfig>,
    /// Public base URL of the application, without a trailing slash.
    ///
    /// Used when building links in outgoing email. Defaults to the bind
    /// address, which is right for development and must be set explicitly in
    /// production.
    pub app_url: String,
    /// Key that encrypts recoverable secrets at rest, from `ANUBIS_SECRET_KEY`.
    ///
    /// Production requires it. Development and test fall back to the built-in
    /// development key; see the module docs.
    pub secret_key: SecretKey,
    /// SMTP relay settings, when `SMTP_URL` is set.
    ///
    /// Present means real delivery, absent means the log mailer; see the
    /// module docs.
    pub smtp: Option<SmtpConfig>,
    /// The OpenID Connect providers the environment enabled, in registry order.
    ///
    /// Empty unless a provider's client id and secret are both set.
    pub oauth: Vec<OauthProviderConfig>,
    /// Directory of built frontend assets, when `SPA_DIR` is set.
    ///
    /// Present means the binary also serves the SPA, absent means it serves
    /// only the API; see the module docs and [`crate::spa`].
    pub spa_dir: Option<PathBuf>,
    /// Whether the abuse limits are on, and how clients are addressed.
    ///
    /// On by default; see the module docs and [`crate::rate_limit`].
    pub rate_limit: RateLimitConfig,
    /// Which origins may call the application from a browser.
    ///
    /// Empty by default, which sends no CORS headers at all; see the module
    /// docs and [`crate::server`].
    pub cors: CorsConfig,
    /// What the application adds to the content security policy.
    ///
    /// Empty by default, which sends the framework's policy unchanged; see the
    /// module docs and [`crate::server`].
    pub csp: CspConfig,
    /// Stripe credentials, when `STRIPE_SECRET_KEY` is set.
    ///
    /// Present enables checkout and the customer portal, absent leaves every
    /// organization on the free plan; see the module docs and
    /// [`crate::billing`].
    pub stripe: Option<StripeConfig>,
    /// How many argon2 computations may hold memory at once.
    ///
    /// Defaults to [`password::DEFAULT_CONCURRENCY`]; see the module docs and
    /// [`crate::auth::password`].
    pub password_hash_concurrency: NonZero<usize>,
}

impl AppConfig {
    /// Loads configuration from the process environment.
    ///
    /// # Errors
    /// Returns an [`Error`] when a recognized variable is set to a value that
    /// does not parse.
    pub fn from_env() -> Result<Self, Error> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Loads configuration from an arbitrary variable lookup.
    ///
    /// The lookup receives a variable name and returns its value, if set.
    /// Unset variables fall back to their documented defaults. This is the
    /// seam that keeps configuration testable: pass a closure over a map
    /// instead of touching the process environment.
    ///
    /// # Errors
    /// Returns an [`Error`] when a recognized variable is set to a value that
    /// does not parse.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let environment = match lookup(ENV_VAR) {
            None => Environment::default(),
            Some(value) => parse_environment(&value).ok_or_else(|| {
                Error::invalid(ENV_VAR, &value, "one of development, test, production")
            })?,
        };

        let host = match lookup(HOST_VAR) {
            None => DEFAULT_HOST,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(HOST_VAR, &value, "an IP address"))?,
        };

        let port = match lookup(PORT_VAR) {
            None => DEFAULT_PORT,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(PORT_VAR, &value, "a TCP port number"))?,
        };

        let database = match lookup(DATABASE_URL_VAR) {
            None => None,
            Some(url) if url.trim().is_empty() => {
                return Err(Error::invalid(
                    DATABASE_URL_VAR,
                    &url,
                    "a non-empty Postgres connection URL",
                ));
            }
            Some(url) => Some(DatabaseConfig { url }),
        };

        let redis = match non_empty(lookup(REDIS_URL_VAR)) {
            None => None,
            Some(url) => {
                let url = url.trim().to_owned();
                if !url.starts_with("redis://") && !url.starts_with("rediss://") {
                    // The URL may carry a password, so it is never quoted back.
                    return Err(Error::invalid_secret(REDIS_URL_VAR, REDIS_URL_FORM));
                }
                Some(RedisConfig { url })
            }
        };

        let app_url = match lookup(APP_URL_VAR) {
            None => format!("http://{host}:{port}"),
            Some(url) => {
                let trimmed = url.trim().trim_end_matches('/');
                if trimmed.is_empty() || !trimmed.starts_with("http") {
                    return Err(Error::invalid(APP_URL_VAR, &url, "an http(s) base URL"));
                }
                trimmed.to_owned()
            }
        };

        let secret_key = match lookup(SECRET_KEY_VAR) {
            Some(encoded) => SecretKey::from_base64(&encoded)
                .map_err(|_error| Error::invalid_secret(SECRET_KEY_VAR, SECRET_KEY_FORM))?,
            // Live deployments store real secrets, so the key is mandatory
            // there; anywhere else the public development key keeps a fresh
            // checkout running with no configuration.
            None if environment.is_production() => {
                return Err(Error::missing(SECRET_KEY_VAR, SECRET_KEY_FORM));
            }
            None => SecretKey::development(),
        };

        let smtp = smtp_config(&lookup)?;
        let oauth = oauth_providers(&lookup)?;
        // Only the path is resolved here; whether it holds a built frontend is
        // the assets service's question, asked once at startup.
        let spa_dir = non_empty(lookup(SPA_DIR_VAR)).map(|dir| PathBuf::from(dir.trim()));
        let rate_limit = rate_limit_config(&lookup)?;
        let cors = cors_config(&lookup)?;
        let csp = csp_config(&lookup)?;
        let stripe = stripe_config(&lookup)?;
        let password_hash_concurrency = match non_empty(lookup(PASSWORD_HASH_CONCURRENCY_VAR)) {
            None => password::DEFAULT_CONCURRENCY,
            // Zero would refuse every sign-in, so it is a typo rather than a
            // setting; `NonZero` rejects it along with anything unparseable.
            Some(value) => value.trim().parse().map_err(|_error| {
                Error::invalid(
                    PASSWORD_HASH_CONCURRENCY_VAR,
                    value.trim(),
                    PASSWORD_HASH_CONCURRENCY_FORM,
                )
            })?,
        };

        Ok(Self {
            environment,
            server: ServerConfig { host, port },
            database,
            redis,
            app_url,
            secret_key,
            smtp,
            oauth,
            spa_dir,
            rate_limit,
            cors,
            csp,
            stripe,
            password_hash_concurrency,
        })
    }
}

/// Resolves the abuse limits: whether they run, and how clients are addressed.
///
/// A header name that cannot be a header name stops startup rather than
/// silently reverting to the peer address, because the difference decides who
/// every request is charged to.
fn rate_limit_config(lookup: &impl Fn(&str) -> Option<String>) -> Result<RateLimitConfig, Error> {
    let disabled = match non_empty(lookup(RATE_LIMIT_DISABLED_VAR)) {
        None => false,
        Some(value) => parse_bool(&value)
            .ok_or_else(|| Error::invalid(RATE_LIMIT_DISABLED_VAR, value.trim(), BOOLEAN_FORM))?,
    };

    let trusted_proxy_header = match non_empty(lookup(TRUSTED_PROXY_HEADER_VAR)) {
        None => None,
        Some(value) => {
            let name = value.trim().to_ascii_lowercase();
            let parsed = HeaderName::try_from(name).map_err(|_error| {
                Error::invalid(TRUSTED_PROXY_HEADER_VAR, value.trim(), HEADER_NAME_FORM)
            })?;
            Some(parsed)
        }
    };

    Ok(RateLimitConfig {
        disabled,
        trusted_proxy_header,
    })
}

/// Resolves which origins may call the application from a browser.
///
/// An entry that is not an exact origin stops startup rather than being
/// dropped: a typo that silently narrows a CORS policy surfaces as a browser
/// error in someone else's console, days later.
fn cors_config(lookup: &impl Fn(&str) -> Option<String>) -> Result<CorsConfig, Error> {
    let Some(raw) = non_empty(lookup(CORS_ALLOWED_ORIGINS_VAR)) else {
        return Ok(CorsConfig::default());
    };

    let mut allowed_origins = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let origin = parse_origin(entry)
            .ok_or_else(|| Error::invalid(CORS_ALLOWED_ORIGINS_VAR, entry, ORIGIN_FORM))?;
        if !allowed_origins.contains(&origin) {
            allowed_origins.push(origin);
        }
    }

    Ok(CorsConfig { allowed_origins })
}

/// Renders one entry as the exact origin a browser will send, or `None`.
///
/// A browser's `Origin` header is `scheme://host[:port]` and nothing else, so
/// anything carrying a path, a query, credentials, or a wildcard could never
/// match one and is a misunderstanding worth failing on. `Origin` serialization
/// also normalizes the case and drops a default port, which is what makes the
/// stored value comparable to what arrives.
fn parse_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;

    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    if !matches!(url.path(), "" | "/") {
        return None;
    }
    // A wildcard host parses as a perfectly good domain but can never equal an
    // `Origin` header. People reach for `https://*.example.com` expecting
    // subdomain matching, which CORS has no notion of, so this is a mistake
    // worth naming rather than a rule that silently never matches.
    if url.host_str().is_some_and(|host| host.contains('*')) {
        return None;
    }

    match url.origin() {
        Origin::Tuple(..) => Some(url.origin().ascii_serialization()),
        Origin::Opaque(_) => None,
    }
}

/// Resolves what the application adds to the content security policy.
///
/// The value is written the way CSP itself is, one group per directive:
/// `script-src https://plausible.io; connect-src https://plausible.io`. A
/// directive the framework will not widen, or a source that is not a host,
/// stops startup rather than being dropped, because a policy quietly missing
/// the source it was given is a blank widget nobody connects to this variable.
fn csp_config(lookup: &impl Fn(&str) -> Option<String>) -> Result<CspConfig, Error> {
    let Some(raw) = non_empty(lookup(CSP_ALLOWED_SOURCES_VAR)) else {
        return Ok(CspConfig::default());
    };

    let mut additional_sources: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for group in raw.split(';') {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }

        let (name, listed) = group
            .split_once(char::is_whitespace)
            .ok_or_else(|| Error::invalid(CSP_ALLOWED_SOURCES_VAR, group, CSP_GROUP_FORM))?;
        let directive = server::extendable_directive(name)
            .ok_or_else(|| Error::invalid(CSP_ALLOWED_SOURCES_VAR, name, CSP_DIRECTIVE_FORM))?;

        let sources = additional_sources.entry(directive).or_default();
        for entry in listed.split([',', ' ', '\t']) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let source = parse_csp_source(entry)
                .ok_or_else(|| Error::invalid(CSP_ALLOWED_SOURCES_VAR, entry, CSP_SOURCE_FORM))?;
            if !sources.contains(&source) {
                sources.push(source);
            }
        }

        if sources.is_empty() {
            return Err(Error::invalid(
                CSP_ALLOWED_SOURCES_VAR,
                group,
                CSP_GROUP_FORM,
            ));
        }
    }

    Ok(CspConfig { additional_sources })
}

/// Renders one entry as the source expression CSP will read, or `None`.
///
/// Only host sources are accepted. Every real reason to extend the policy names
/// a host: an analytics endpoint, an error ingest, a CDN, a payment widget. A
/// form that names none either widens a directive wholesale (`https:`) or
/// reopens what the framework closed on purpose (`'unsafe-eval'`, `data:` under
/// `script-src`), and that belongs in a code review rather than in an
/// environment variable.
///
/// A single leading `*.` label is kept, because CSP does match subdomains and
/// `https://*.example.com` means exactly what people expect it to.
fn parse_csp_source(value: &str) -> Option<String> {
    // The wildcard is stripped before parsing and put back after: it is CSP
    // syntax rather than URL syntax, so no URL parser owes us an opinion on it.
    let (scheme, rest) = value.split_once("://")?;
    let (wildcard, host_and_port) = match rest.strip_prefix("*.") {
        Some(remainder) => (true, remainder),
        None => (false, rest),
    };

    let url = Url::parse(&format!("{scheme}://{host_and_port}")).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    if !matches!(url.path(), "" | "/") {
        return None;
    }
    let host = url.host_str()?;
    if host.contains('*') {
        return None;
    }

    let mut rendered = format!("{}://", url.scheme());
    if wildcard {
        rendered.push_str("*.");
    }
    rendered.push_str(host);
    if let Some(port) = url.port() {
        rendered.push(':');
        rendered.push_str(&port.to_string());
    }
    Some(rendered)
}

/// Resolves the Stripe credentials, when the environment carries them.
///
/// A publishable key is refused by name rather than by its shape: it is the
/// key a person reaches for first, it is not secret, and every call made with
/// it would fail at Stripe with a message about the wrong key type.
fn stripe_config(lookup: &impl Fn(&str) -> Option<String>) -> Result<Option<StripeConfig>, Error> {
    let webhook_secret = non_empty(lookup(STRIPE_WEBHOOK_SECRET_VAR)).map(|secret| {
        // Trimmed rather than validated by prefix: `whsec_` is the shape Stripe
        // hands out today, and refusing anything else would refuse a
        // Stripe-compatible mock for no gain. The one mistake worth naming is
        // below.
        secret.trim().to_owned()
    });

    let Some(secret_key) = non_empty(lookup(STRIPE_SECRET_KEY_VAR)) else {
        if webhook_secret.is_some() {
            // A receiver with no API key could store events and read nothing
            // back, so half-configured billing stops the boot the same way a
            // half-configured OAuth provider does.
            return Err(Error::invalid_secret(
                STRIPE_SECRET_KEY_VAR,
                "set alongside STRIPE_WEBHOOK_SECRET: the billing receiver reads each \
                 subscription back from Stripe, which needs the secret key",
            ));
        }
        return Ok(None);
    };
    let secret_key = secret_key.trim().to_owned();

    if webhook_secret.as_deref().is_some_and(|secret| {
        ["sk_", "rk_", "pk_"]
            .iter()
            .any(|key| secret.starts_with(key))
    }) {
        // An API key pasted into the signing secret: it would verify nothing and
        // every real event would be refused. The value is never quoted back.
        return Err(Error::invalid_secret(
            STRIPE_WEBHOOK_SECRET_VAR,
            STRIPE_WEBHOOK_SECRET_FORM,
        ));
    }

    if !secret_key.starts_with("sk_") && !secret_key.starts_with("rk_") {
        // The key spends money, so the bad value is never quoted back.
        return Err(Error::invalid_secret(
            STRIPE_SECRET_KEY_VAR,
            STRIPE_SECRET_KEY_FORM,
        ));
    }

    let api_base = match non_empty(lookup(STRIPE_API_BASE_VAR)) {
        None => crate::billing::stripe::API_BASE.to_owned(),
        Some(url) => {
            let trimmed = url.trim().trim_end_matches('/');
            if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                return Err(Error::invalid(
                    STRIPE_API_BASE_VAR,
                    url.trim(),
                    STRIPE_API_BASE_FORM,
                ));
            }
            trimmed.to_owned()
        }
    };

    Ok(Some(StripeConfig {
        secret_key,
        api_base,
        webhook_secret,
    }))
}

/// Reads the spellings of yes and no that environment variables use.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Resolves the SMTP relay, when the environment configures one.
///
/// The URL is checked for a relay scheme only; the transport parses the rest
/// when the mailer is built. A relay cannot deliver without a sender, so a URL
/// without `MAIL_FROM` stops startup rather than failing every send later.
fn smtp_config(lookup: &impl Fn(&str) -> Option<String>) -> Result<Option<SmtpConfig>, Error> {
    let Some(url) = non_empty(lookup(SMTP_URL_VAR)) else {
        if DKIM_VARS
            .iter()
            .any(|name| non_empty(lookup(name)).is_some())
        {
            // The log mailer delivers nothing, so a key configured beside it
            // would sign nothing. Naming the relay is the fix either way:
            // configure one, or drop the signing variables.
            return Err(Error::missing(
                SMTP_URL_VAR,
                "a relay to sign for: DKIM signs messages on their way out, and the log mailer \
                 sends none",
            ));
        }
        return Ok(None);
    };
    let url = url.trim().to_owned();

    if !url.starts_with("smtp://") && !url.starts_with("smtps://") {
        // The URL carries credentials, so the bad value is never quoted back.
        return Err(Error::invalid_secret(SMTP_URL_VAR, SMTP_URL_FORM));
    }

    let Some(from) = non_empty(lookup(MAIL_FROM_VAR)) else {
        return Err(Error::missing(MAIL_FROM_VAR, MAIL_FROM_FORM));
    };
    let from = from.trim().to_owned();

    // The sender's own domain is both the check that the address parses and the
    // default the signature claims responsibility for.
    let Some(from_domain) = crate::mail::address_domain(&from) else {
        return Err(Error::invalid(MAIL_FROM_VAR, &from, MAIL_FROM_FORM));
    };

    let dkim = dkim_config(lookup, &from_domain)?;

    Ok(Some(SmtpConfig { url, from, dkim }))
}

/// Resolves DKIM signing, when the environment asks for it.
///
/// The three variables are one group: a key with no selector cannot be found in
/// DNS, and a selector with no key signs nothing, so either mistake stops
/// startup naming what is missing. `DKIM_DOMAIN` is the exception, defaulting to
/// `from_domain`, which is what a deployment signing for its own sender wants.
fn dkim_config(
    lookup: &impl Fn(&str) -> Option<String>,
    from_domain: &str,
) -> Result<Option<DkimConfig>, Error> {
    let private_key = non_empty(lookup(DKIM_PRIVATE_KEY_VAR))
        .map(|key| unescape_newlines(&key).trim().to_owned());
    let selector = non_empty(lookup(DKIM_SELECTOR_VAR));
    let domain = non_empty(lookup(DKIM_DOMAIN_VAR));

    let (private_key, selector) = match (private_key, selector) {
        (None, None) if domain.is_none() => return Ok(None),
        (None, None) => {
            return Err(Error::missing(
                DKIM_PRIVATE_KEY_VAR,
                "a signing key and a DKIM_SELECTOR to publish it under; DKIM_DOMAIN alone signs \
                 nothing",
            ));
        }
        (None, Some(_selector)) => {
            return Err(Error::missing(DKIM_PRIVATE_KEY_VAR, DKIM_PRIVATE_KEY_FORM));
        }
        (Some(_key), None) => return Err(Error::missing(DKIM_SELECTOR_VAR, DKIM_SELECTOR_FORM)),
        (Some(private_key), Some(selector)) => (private_key, selector),
    };

    if !crate::mail::is_valid_signing_key(&private_key) {
        // The key signs mail as this domain, so the bad value is never quoted
        // back.
        return Err(Error::invalid_secret(
            DKIM_PRIVATE_KEY_VAR,
            DKIM_PRIVATE_KEY_FORM,
        ));
    }

    // Both ride in the DKIM-Signature header, where anything but a DNS name is
    // either unresolvable or a header of somebody else's choosing.
    let selector = selector.trim().to_owned();
    if !is_dns_name(&selector) {
        return Err(Error::invalid(
            DKIM_SELECTOR_VAR,
            &selector,
            DKIM_SELECTOR_FORM,
        ));
    }

    let domain = match domain {
        None => from_domain.to_ascii_lowercase(),
        Some(domain) => {
            let domain = domain.trim().to_ascii_lowercase();
            if !is_dns_name(&domain) || !domain_covers(&domain, from_domain) {
                return Err(Error::invalid(DKIM_DOMAIN_VAR, &domain, DKIM_DOMAIN_FORM));
            }
            domain
        }
    };

    Ok(Some(DkimConfig {
        private_key,
        selector,
        domain,
    }))
}

/// Restores the newlines a one-line PEM had to escape.
///
/// A PEM key is multi-line, and plenty of places a deployment keeps secrets (a
/// `.env` file, a platform's config-var field) hold one line only. Writing the
/// key with `\n` escapes is the convention those places grew, so both forms are
/// accepted and mean the same key.
fn unescape_newlines(value: &str) -> String {
    value.replace("\\n", "\n")
}

/// Returns `true` when `value` is a dotted sequence of DNS labels.
fn is_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
        })
}

/// Returns `true` when a signature for `domain` speaks for `from_domain`.
///
/// DMARC's relaxed alignment, the default, accepts a signing domain that is the
/// sender's domain or one of its parents, which is what an organization signing
/// `no-reply@mail.acme.com` with an `acme.com` key relies on. An unrelated
/// domain is worth failing the boot over: the signature would verify and DMARC
/// would still reject the message, so the mail looks signed and lands in spam.
fn domain_covers(domain: &str, from_domain: &str) -> bool {
    let from_domain = from_domain.to_ascii_lowercase();
    from_domain == domain || from_domain.ends_with(&format!(".{domain}"))
}

/// Resolves every provider whose credentials the environment carries.
///
/// A provider with neither credential is simply not enabled. A provider with
/// one of the two is a misconfiguration: the button would be rendered and
/// every click would fail, so startup stops and names the missing variable.
fn oauth_providers(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<OauthProviderConfig>, Error> {
    let mut configured = Vec::new();

    for provider in known_providers() {
        let client_id = non_empty(lookup(provider.client_id_var));
        let client_secret = non_empty(lookup(provider.client_secret_var));

        let (client_id, client_secret) = match (client_id, client_secret) {
            (None, None) => continue,
            (Some(client_id), Some(client_secret)) => (client_id, client_secret),
            (Some(_client_id), None) => {
                return Err(Error::missing(
                    provider.client_secret_var,
                    "the client secret that goes with the client id",
                ));
            }
            (None, Some(_client_secret)) => {
                return Err(Error::missing(
                    provider.client_id_var,
                    "the client id that goes with the client secret",
                ));
            }
        };

        let issuer = non_empty(lookup(provider.issuer_var));
        let config = OauthProviderConfig::new(provider, client_id, client_secret, issuer.clone())
            .map_err(|_error| {
            Error::invalid(
                provider.issuer_var,
                issuer.as_deref().unwrap_or(provider.issuer),
                "an OpenID Connect issuer: an http(s) URL with no query string or fragment",
            )
        })?;
        configured.push(config);
    }

    Ok(configured)
}

/// Treats a variable set to whitespace as unset, the way a `.env` line reads.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_environment(value: &str) -> Option<Environment> {
    match value.to_ascii_lowercase().as_str() {
        "development" | "dev" => Some(Environment::Development),
        "test" => Some(Environment::Test),
        "production" | "prod" => Some(Environment::Production),
        _ => None,
    }
}

/// A configuration variable is missing, or set to a value that does not parse.
#[derive(Debug)]
pub struct Error {
    variable: &'static str,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// The variable holds a value that does not parse.
    ///
    /// The value is quoted back to make the fix obvious, so never use this for
    /// a variable holding a secret; use [`Error::invalid_secret`] instead.
    fn invalid(variable: &'static str, value: &str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("invalid value for {variable}: expected {expected}, got {value:?}"),
        )
    }

    /// The variable holds a value that does not parse and must not be logged.
    fn invalid_secret(variable: &'static str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("invalid value for {variable}: expected {expected}"),
        )
    }

    /// The variable is required in this environment but is not set.
    fn missing(variable: &'static str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("{variable} is not set: expected {expected}"),
        )
    }

    fn new(variable: &'static str, message: String) -> Self {
        Self {
            variable,
            message,
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns the name of the offending environment variable.
    #[must_use]
    pub fn variable(&self) -> &str {
        self.variable
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
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    use super::{AppConfig, Environment};
    use crate::auth::secret_box::SecretKey;

    /// A syntactically valid `ANUBIS_SECRET_KEY`, for tests that need one.
    const SAMPLE_SECRET_KEY: &str = "bkVLZLd1zHBqxWvKGKp5gRTZKcTf9UvHT5vXbHvWJ0M=";

    /// The throwaway DKIM key the mail suites sign with; see [`crate::mail`].
    const TEST_PRIVATE_KEY: &str = include_str!("../tests/support/dkim_test_key.pem");

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let config = AppConfig::from_lookup(|_name| None).expect("defaults must parse");

        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.server.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.server.port, 3000);
    }

    #[test]
    fn variables_override_defaults() {
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("HOST", "0.0.0.0"),
            ("PORT", "8080"),
            ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("valid variables must parse");

        assert_eq!(config.environment, Environment::Production);
        assert_eq!(config.server.host, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.socket_addr().to_string(), "0.0.0.0:8080");
    }

    #[test]
    fn environment_names_parse_case_insensitively_with_short_forms() {
        for (value, expected) in [
            ("DEVELOPMENT", Environment::Development),
            ("dev", Environment::Development),
            ("Test", Environment::Test),
            ("PROD", Environment::Production),
        ] {
            // Production insists on a key; every environment accepts one.
            let lookup = lookup_from(&[
                ("ANUBIS_ENV", value),
                ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
            ]);
            let config = AppConfig::from_lookup(lookup).expect("known names must parse");
            assert_eq!(config.environment, expected, "for input {value:?}");
        }
    }

    #[test]
    fn invalid_port_is_rejected_with_context() {
        let lookup = lookup_from(&[("PORT", "not-a-port")]);

        let error = AppConfig::from_lookup(lookup).expect_err("junk ports must be rejected");

        assert_eq!(error.variable(), "PORT");
        let rendered = error.to_string();
        assert!(rendered.contains("PORT"), "got: {rendered}");
        assert!(rendered.contains("not-a-port"), "got: {rendered}");
    }

    #[test]
    fn invalid_environment_is_rejected_with_context() {
        let lookup = lookup_from(&[("ANUBIS_ENV", "staging")]);

        let error = AppConfig::from_lookup(lookup).expect_err("unknown environments are rejected");

        assert_eq!(error.variable(), "ANUBIS_ENV");
    }

    #[test]
    fn app_url_defaults_to_the_bind_address_and_trims_trailing_slashes() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert_eq!(defaulted.app_url, "http://127.0.0.1:3000");

        let lookup = lookup_from(&[("APP_URL", "https://app.example.com/")]);
        let explicit = AppConfig::from_lookup(lookup).expect("a URL is fine");
        assert_eq!(explicit.app_url, "https://app.example.com");

        let lookup = lookup_from(&[("APP_URL", "not-a-url")]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk URLs are rejected");
        assert_eq!(error.variable(), "APP_URL");
    }

    #[test]
    fn database_url_is_optional_but_must_be_non_empty() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.database.is_none());

        let lookup = lookup_from(&[("DATABASE_URL", "postgres://app:hunter2@localhost/app")]);
        let set = AppConfig::from_lookup(lookup).expect("a URL is fine");
        let database = set.database.expect("database config must be present");
        assert_eq!(database.url(), "postgres://app:hunter2@localhost/app");

        let lookup = lookup_from(&[("DATABASE_URL", "   ")]);
        let error = AppConfig::from_lookup(lookup).expect_err("blank URLs are rejected");
        assert_eq!(error.variable(), "DATABASE_URL");
    }

    #[test]
    fn realtime_stays_in_process_until_a_redis_url_is_set() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.redis.is_none());

        for url in ["redis://localhost:6379", "rediss://cache.example.com:6380"] {
            let lookup = lookup_from(&[("REDIS_URL", url)]);
            let config = AppConfig::from_lookup(lookup).expect("connection URLs must parse");
            let redis = config.redis.expect("redis config must be present");
            assert_eq!(redis.url(), url);
        }
    }

    #[test]
    fn a_redis_url_of_the_wrong_scheme_is_rejected_without_echoing_it() {
        let lookup = lookup_from(&[("REDIS_URL", "http://user:hunter2@cache.example.com")]);

        let error = AppConfig::from_lookup(lookup).expect_err("only redis schemes are accepted");

        assert_eq!(error.variable(), "REDIS_URL");
        let rendered = error.to_string();
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(rendered.contains("rediss://"), "got: {rendered}");
    }

    #[test]
    fn redis_debug_output_never_leaks_credentials() {
        let lookup = lookup_from(&[("REDIS_URL", "redis://default:hunter2@localhost:6379")]);
        let config = AppConfig::from_lookup(lookup).expect("a URL is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("RedisConfig"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }

    #[test]
    fn database_debug_output_never_leaks_credentials() {
        let lookup = lookup_from(&[("DATABASE_URL", "postgres://app:hunter2@localhost/app")]);
        let config = AppConfig::from_lookup(lookup).expect("a URL is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("DatabaseConfig"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }

    #[test]
    fn the_secret_key_falls_back_to_the_development_key_outside_production() {
        for environment in ["development", "test"] {
            let lookup = lookup_from(&[("ANUBIS_ENV", environment)]);
            let config = AppConfig::from_lookup(lookup).expect("no key is fine here");
            assert!(
                config.secret_key.is_development(),
                "for environment {environment}"
            );
        }
    }

    #[test]
    fn production_refuses_to_start_without_a_secret_key() {
        let lookup = lookup_from(&[("ANUBIS_ENV", "production")]);

        let error = AppConfig::from_lookup(lookup).expect_err("production requires a key");

        assert_eq!(error.variable(), "ANUBIS_SECRET_KEY");
        let rendered = error.to_string();
        assert!(rendered.contains("is not set"), "got: {rendered}");
        assert!(rendered.contains("32 random bytes"), "got: {rendered}");
    }

    #[test]
    fn a_configured_secret_key_is_used_verbatim() {
        let encoded = SecretKey::generate().to_base64();
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", &encoded),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("a valid key must parse");

        assert!(!config.secret_key.is_development());
        assert_eq!(config.secret_key.to_base64(), encoded);
    }

    #[test]
    fn a_malformed_secret_key_is_rejected_without_echoing_it() {
        let lookup = lookup_from(&[("ANUBIS_SECRET_KEY", "c2hvcnQ=")]);

        let error = AppConfig::from_lookup(lookup).expect_err("short keys are rejected");

        assert_eq!(error.variable(), "ANUBIS_SECRET_KEY");
        let rendered = error.to_string();
        assert!(!rendered.contains("c2hvcnQ"), "got: {rendered}");
    }

    #[test]
    fn smtp_is_unconfigured_until_a_relay_url_is_set() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.smtp.is_none());

        // A sender without a relay is harmless: the log mailer ignores it.
        let lookup = lookup_from(&[("MAIL_FROM", "Acme <no-reply@acme.com>")]);
        let sender_only = AppConfig::from_lookup(lookup).expect("a lone sender is fine");
        assert!(sender_only.smtp.is_none());
    }

    #[test]
    fn both_smtp_url_forms_are_accepted() {
        for url in [
            "smtps://user:hunter2@smtp.example.com:465",
            "smtp://user:hunter2@smtp.example.com:587?tls=required",
        ] {
            let lookup =
                lookup_from(&[("SMTP_URL", url), ("MAIL_FROM", "Acme <no-reply@acme.com>")]);
            let config = AppConfig::from_lookup(lookup).expect("relay URLs must parse");

            let smtp = config.smtp.expect("smtp config must be present");
            assert_eq!(smtp.url(), url);
            assert_eq!(smtp.from(), "Acme <no-reply@acme.com>");
        }
    }

    #[test]
    fn a_relay_url_of_the_wrong_scheme_is_rejected_without_echoing_it() {
        let lookup = lookup_from(&[
            ("SMTP_URL", "https://user:hunter2@smtp.example.com"),
            ("MAIL_FROM", "no-reply@acme.com"),
        ]);

        let error = AppConfig::from_lookup(lookup).expect_err("only relay schemes are accepted");

        assert_eq!(error.variable(), "SMTP_URL");
        let rendered = error.to_string();
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(rendered.contains("smtps://"), "got: {rendered}");
    }

    #[test]
    fn a_configured_relay_requires_a_sender_address() {
        let lookup = lookup_from(&[("SMTP_URL", "smtps://smtp.example.com")]);
        let error = AppConfig::from_lookup(lookup).expect_err("a relay needs a sender");
        assert_eq!(error.variable(), "MAIL_FROM");
        assert!(error.to_string().contains("is not set"), "got: {error}");

        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "no-reply"),
        ]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk senders are rejected");
        assert_eq!(error.variable(), "MAIL_FROM");
    }

    #[test]
    fn smtp_debug_output_never_leaks_credentials() {
        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://user:hunter2@smtp.example.com:465"),
            ("MAIL_FROM", "Acme <no-reply@acme.com>"),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("a relay URL is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("SmtpConfig"), "got: {rendered}");
        assert!(rendered.contains("no-reply@acme.com"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }

    #[test]
    fn mail_is_unsigned_until_a_key_and_a_selector_are_set() {
        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "Acme <no-reply@acme.com>"),
        ]);
        let unsigned = AppConfig::from_lookup(lookup).expect("a relay without a key is fine");
        assert!(
            unsigned
                .smtp
                .expect("smtp config must be present")
                .dkim()
                .is_none()
        );

        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "Acme <no-reply@acme.com>"),
            ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
            ("DKIM_SELECTOR", " mail "),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("a key and a selector must parse");
        let smtp = config.smtp.expect("smtp config must be present");
        let dkim = smtp.dkim().expect("dkim config must be present");
        assert_eq!(dkim.selector(), "mail");
        assert_eq!(
            dkim.domain(),
            "acme.com",
            "the signature defaults to the sender's own domain",
        );
        assert_eq!(dkim.private_key(), TEST_PRIVATE_KEY.trim());
    }

    #[test]
    fn a_one_line_key_written_with_escapes_is_the_same_key() {
        let escaped = TEST_PRIVATE_KEY.trim().replace('\n', "\\n");
        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "no-reply@acme.com"),
            ("DKIM_PRIVATE_KEY", &escaped),
            ("DKIM_SELECTOR", "mail"),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("an escaped key must parse");

        let smtp = config.smtp.expect("smtp config must be present");
        let dkim = smtp.dkim().expect("dkim config must be present");
        assert_eq!(dkim.private_key(), TEST_PRIVATE_KEY.trim());
    }

    #[test]
    fn half_the_signing_group_stops_the_boot_naming_what_is_missing() {
        let relay = [
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "no-reply@acme.com"),
        ];

        for (extra, missing) in [
            (("DKIM_SELECTOR", "mail"), "DKIM_PRIVATE_KEY"),
            (("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY), "DKIM_SELECTOR"),
            (("DKIM_DOMAIN", "acme.com"), "DKIM_PRIVATE_KEY"),
        ] {
            let lookup = lookup_from(&[relay[0], relay[1], extra]);
            let error = AppConfig::from_lookup(lookup).expect_err("half a group is a mistake");
            assert_eq!(error.variable(), missing, "for {extra:?}");
            assert!(error.to_string().contains("is not set"), "got: {error}");
        }

        // A lone domain names the selector it also needs, not just the key.
        let lookup = lookup_from(&[relay[0], relay[1], ("DKIM_DOMAIN", "acme.com")]);
        let error = AppConfig::from_lookup(lookup).expect_err("a lone domain is a mistake");
        assert!(error.to_string().contains("DKIM_SELECTOR"), "got: {error}");
    }

    #[test]
    fn signing_without_a_relay_stops_the_boot() {
        for (name, value) in [
            ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
            ("DKIM_SELECTOR", "mail"),
            ("DKIM_DOMAIN", "acme.com"),
        ] {
            let lookup = lookup_from(&[("MAIL_FROM", "no-reply@acme.com"), (name, value)]);
            let error =
                AppConfig::from_lookup(lookup).expect_err("the log mailer sends nothing to sign");
            assert_eq!(error.variable(), "SMTP_URL", "for {name}");
        }
    }

    #[test]
    fn the_signing_domain_may_be_the_senders_or_one_of_its_parents() {
        for domain in ["acme.com", "ACME.COM", "mail.acme.com"] {
            let lookup = lookup_from(&[
                ("SMTP_URL", "smtps://smtp.example.com"),
                ("MAIL_FROM", "Acme <no-reply@mail.acme.com>"),
                ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
                ("DKIM_SELECTOR", "mail"),
                ("DKIM_DOMAIN", domain),
            ]);
            let config = AppConfig::from_lookup(lookup).expect("an aligned domain must parse");
            let smtp = config.smtp.expect("smtp config must be present");
            assert_eq!(
                smtp.dkim().map(super::DkimConfig::domain),
                Some(domain.to_ascii_lowercase().as_str()),
                "for {domain}",
            );
        }

        // A signature DMARC cannot align is worse than no signature: it
        // verifies, and the message is rejected anyway.
        for domain in [
            "example.com",
            "notacme.com",
            "sub.mail.acme.com",
            "not a host",
        ] {
            let lookup = lookup_from(&[
                ("SMTP_URL", "smtps://smtp.example.com"),
                ("MAIL_FROM", "Acme <no-reply@mail.acme.com>"),
                ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
                ("DKIM_SELECTOR", "mail"),
                ("DKIM_DOMAIN", domain),
            ]);
            let error = AppConfig::from_lookup(lookup).expect_err("an unaligned domain is a bug");
            assert_eq!(error.variable(), "DKIM_DOMAIN", "for {domain}");
        }
    }

    #[test]
    fn a_selector_that_is_not_a_dns_name_is_rejected() {
        for selector in ["mail selector", "mail\r\nBcc: attacker@example.com", "."] {
            let lookup = lookup_from(&[
                ("SMTP_URL", "smtps://smtp.example.com"),
                ("MAIL_FROM", "no-reply@acme.com"),
                ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
                ("DKIM_SELECTOR", selector),
            ]);
            let error = AppConfig::from_lookup(lookup).expect_err("junk selectors are rejected");
            assert_eq!(error.variable(), "DKIM_SELECTOR", "for {selector:?}");
        }
    }

    #[test]
    fn a_key_that_cannot_sign_is_rejected_without_echoing_it() {
        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "no-reply@acme.com"),
            ("DKIM_PRIVATE_KEY", "-----BEGIN RSA PRIVATE KEY-----hunter2"),
            ("DKIM_SELECTOR", "mail"),
        ]);

        let error = AppConfig::from_lookup(lookup).expect_err("only real keys are accepted");

        assert_eq!(error.variable(), "DKIM_PRIVATE_KEY");
        let rendered = error.to_string();
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(rendered.contains("-traditional"), "got: {rendered}");
    }

    #[test]
    fn dkim_debug_output_never_leaks_the_key() {
        let lookup = lookup_from(&[
            ("SMTP_URL", "smtps://smtp.example.com"),
            ("MAIL_FROM", "no-reply@acme.com"),
            ("DKIM_PRIVATE_KEY", TEST_PRIVATE_KEY),
            ("DKIM_SELECTOR", "mail"),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("a key is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("DkimConfig"), "got: {rendered}");
        assert!(rendered.contains("selector: \"mail\""), "got: {rendered}");
        assert!(
            !rendered.contains("MII"),
            "no part of the key may be printed: {rendered}",
        );
    }

    #[test]
    fn production_boots_without_a_relay() {
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("a missing relay must not block boot");

        assert!(config.smtp.is_none());
    }

    #[test]
    fn the_spa_directory_is_unset_until_asked_for() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.spa_dir.is_none());

        // A variable set to whitespace reads as unset, the way a `.env` line does.
        let lookup = lookup_from(&[("SPA_DIR", "   ")]);
        let blank = AppConfig::from_lookup(lookup).expect("blank is fine");
        assert!(blank.spa_dir.is_none());

        let lookup = lookup_from(&[("SPA_DIR", " frontend/dist ")]);
        let set = AppConfig::from_lookup(lookup).expect("a path is fine");
        assert_eq!(set.spa_dir, Some(PathBuf::from("frontend/dist")));
    }

    #[test]
    fn rate_limiting_is_on_until_a_deployment_turns_it_off() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert!(!defaulted.rate_limit.disabled);
        assert!(defaulted.rate_limit.trusted_proxy_header.is_none());

        for value in ["true", "TRUE", "1", "yes"] {
            let lookup = lookup_from(&[("RATE_LIMIT_DISABLED", value)]);
            let config = AppConfig::from_lookup(lookup).expect("a boolean must parse");
            assert!(config.rate_limit.disabled, "for input {value:?}");
        }

        for value in ["false", "0", "no"] {
            let lookup = lookup_from(&[("RATE_LIMIT_DISABLED", value)]);
            let config = AppConfig::from_lookup(lookup).expect("a boolean must parse");
            assert!(!config.rate_limit.disabled, "for input {value:?}");
        }

        let lookup = lookup_from(&[("RATE_LIMIT_DISABLED", "maybe")]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk booleans are rejected");
        assert_eq!(error.variable(), "RATE_LIMIT_DISABLED");
    }

    #[test]
    fn password_hashing_is_bounded_by_default_and_tunable_per_deployment() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert_eq!(
            defaulted.password_hash_concurrency,
            crate::auth::password::DEFAULT_CONCURRENCY,
        );

        let lookup = lookup_from(&[("PASSWORD_HASH_CONCURRENCY", " 8 ")]);
        let config = AppConfig::from_lookup(lookup).expect("a count must parse");
        assert_eq!(config.password_hash_concurrency.get(), 8);

        // Zero admits nobody, and a negative or fractional count is a typo.
        for value in ["0", "-1", "many", "1.5"] {
            let lookup = lookup_from(&[("PASSWORD_HASH_CONCURRENCY", value)]);
            let error = AppConfig::from_lookup(lookup).expect_err("junk counts are rejected");
            assert_eq!(
                error.variable(),
                "PASSWORD_HASH_CONCURRENCY",
                "for input {value:?}"
            );
        }
    }

    #[test]
    fn the_trusted_proxy_header_is_normalized_and_validated() {
        let lookup = lookup_from(&[("TRUSTED_PROXY_HEADER", " X-Forwarded-For ")]);
        let config = AppConfig::from_lookup(lookup).expect("a header name must parse");
        assert_eq!(
            config
                .rate_limit
                .trusted_proxy_header
                .as_ref()
                .map(axum::http::HeaderName::as_str),
            Some("x-forwarded-for"),
        );

        let lookup = lookup_from(&[("TRUSTED_PROXY_HEADER", "not a header")]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk header names are rejected");
        assert_eq!(error.variable(), "TRUSTED_PROXY_HEADER");
    }

    #[test]
    fn cross_origin_access_is_off_until_origins_are_named() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert!(defaulted.cors.allowed_origins.is_empty());

        let lookup = lookup_from(&[(
            "CORS_ALLOWED_ORIGINS",
            " https://app.example.com , https://admin.example.com:8443 , \
             https://app.example.com ",
        )]);
        let config = AppConfig::from_lookup(lookup).expect("origins must parse");
        assert_eq!(
            config.cors.allowed_origins,
            [
                "https://app.example.com".to_owned(),
                "https://admin.example.com:8443".to_owned(),
            ],
            "entries are normalized, in order, and deduplicated",
        );
    }

    #[test]
    fn an_origin_a_browser_could_never_send_is_rejected() {
        for value in [
            "*",
            "https://*.example.com",
            "https://app.example.com/dashboard",
            "https://app.example.com?tenant=acme",
            "https://user:hunter2@app.example.com",
            "app.example.com",
            "ftp://files.example.com",
        ] {
            let lookup = lookup_from(&[("CORS_ALLOWED_ORIGINS", value)]);
            match AppConfig::from_lookup(lookup) {
                Ok(config) => panic!("{value:?} must be rejected, got {:?}", config.cors),
                Err(error) => assert_eq!(error.variable(), "CORS_ALLOWED_ORIGINS", "for {value:?}"),
            }
        }
    }

    #[test]
    fn a_default_port_and_a_trailing_slash_normalize_away() {
        let lookup = lookup_from(&[(
            "CORS_ALLOWED_ORIGINS",
            "https://app.example.com:443/,HTTP://Local.Example.com:80",
        )]);

        let config = AppConfig::from_lookup(lookup).expect("origins must parse");

        assert_eq!(
            config.cors.allowed_origins,
            [
                "https://app.example.com".to_owned(),
                "http://local.example.com".to_owned(),
            ],
        );
    }

    #[test]
    fn the_content_policy_takes_extra_sources_per_directive() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert!(defaulted.csp.additional_sources.is_empty());

        let lookup = lookup_from(&[(
            "CSP_ALLOWED_SOURCES",
            " script-src https://plausible.io ; connect-src https://plausible.io, \
             https://*.ingest.sentry.io https://plausible.io ",
        )]);
        let config = AppConfig::from_lookup(lookup).expect("sources must parse");

        assert_eq!(
            config.csp.additional_sources.get("script-src"),
            Some(&vec!["https://plausible.io".to_owned()]),
        );
        assert_eq!(
            config.csp.additional_sources.get("connect-src"),
            Some(&vec![
                "https://plausible.io".to_owned(),
                // A wildcard label is CSP's own syntax, and is kept.
                "https://*.ingest.sentry.io".to_owned(),
            ]),
            "sources are deduplicated, in order",
        );
    }

    #[test]
    fn a_directive_the_framework_keeps_is_refused() {
        for value in [
            "default-src https://anything.example.com",
            "base-uri https://anything.example.com",
            "form-action https://anything.example.com",
            "frame-ancestors https://anything.example.com",
            "sandbox allow-scripts",
        ] {
            let lookup = lookup_from(&[("CSP_ALLOWED_SOURCES", value)]);
            match AppConfig::from_lookup(lookup) {
                Ok(config) => panic!("{value:?} must be rejected, got {:?}", config.csp),
                Err(error) => assert_eq!(error.variable(), "CSP_ALLOWED_SOURCES", "for {value:?}"),
            }
        }
    }

    #[test]
    fn a_source_that_is_not_a_host_is_refused() {
        for value in [
            // The keywords are the whole point of the policy, so none of them
            // arrives from an environment variable.
            "script-src 'unsafe-eval'",
            "script-src 'unsafe-inline'",
            "script-src *",
            // A bare scheme widens a directive wholesale.
            "script-src https:",
            "img-src data:",
            // And the shapes that are simply not origins.
            "script-src https://cdn.example.com/bundle.js",
            "script-src https://user:hunter2@cdn.example.com",
            "script-src cdn.example.com",
            "script-src ftp://files.example.com",
            "script-src https://*.*.example.com",
            // A directive naming nothing asks for something it cannot get.
            "script-src",
        ] {
            let lookup = lookup_from(&[("CSP_ALLOWED_SOURCES", value)]);
            match AppConfig::from_lookup(lookup) {
                Ok(config) => panic!("{value:?} must be rejected, got {:?}", config.csp),
                Err(error) => assert_eq!(error.variable(), "CSP_ALLOWED_SOURCES", "for {value:?}"),
            }
        }
    }

    #[test]
    fn billing_stays_off_until_a_stripe_key_is_set() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.stripe.is_none());

        for key in ["sk_test_abc123", "rk_live_abc123"] {
            let lookup = lookup_from(&[("STRIPE_SECRET_KEY", key)]);
            let config = AppConfig::from_lookup(lookup).expect("a secret key must parse");
            let stripe = config.stripe.expect("stripe config must be present");
            assert_eq!(stripe.secret_key(), key);
            assert_eq!(stripe.api_base(), "https://api.stripe.com");
        }
    }

    #[test]
    fn a_key_that_cannot_be_a_secret_key_is_rejected_without_echoing_it() {
        // The publishable key is the one people reach for by mistake.
        let lookup = lookup_from(&[("STRIPE_SECRET_KEY", "pk_live_hunter2")]);

        let error = AppConfig::from_lookup(lookup).expect_err("only secret keys are accepted");

        assert_eq!(error.variable(), "STRIPE_SECRET_KEY");
        let rendered = error.to_string();
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(rendered.contains("sk_live_"), "got: {rendered}");
    }

    #[test]
    fn stripe_debug_output_never_leaks_the_key() {
        let lookup = lookup_from(&[
            ("STRIPE_SECRET_KEY", "sk_live_hunter2"),
            ("STRIPE_WEBHOOK_SECRET", "whsec_hunter3"),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("a secret key is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("StripeConfig"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(!rendered.contains("hunter3"), "got: {rendered}");
    }

    #[test]
    fn the_webhook_secret_is_read_beside_the_key_and_never_alone() {
        let lookup = lookup_from(&[
            ("STRIPE_SECRET_KEY", "sk_test_abc123"),
            ("STRIPE_WEBHOOK_SECRET", " whsec_abc123 "),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("both secrets must parse");
        let stripe = config.stripe.expect("stripe config must be present");
        assert_eq!(stripe.webhook_secret(), Some("whsec_abc123"));

        // The receiver reads each subscription back from Stripe, so a signing
        // secret without an API key is a half-configured deployment.
        let lookup = lookup_from(&[("STRIPE_WEBHOOK_SECRET", "whsec_abc123")]);
        let error = AppConfig::from_lookup(lookup).expect_err("the key is required");
        assert_eq!(error.variable(), "STRIPE_SECRET_KEY");

        // An API key pasted into the signing secret verifies nothing.
        let lookup = lookup_from(&[
            ("STRIPE_SECRET_KEY", "sk_test_abc123"),
            ("STRIPE_WEBHOOK_SECRET", "sk_test_hunter2"),
        ]);
        let error = AppConfig::from_lookup(lookup).expect_err("an API key is not a signing secret");
        assert_eq!(error.variable(), "STRIPE_WEBHOOK_SECRET");
        assert!(!error.to_string().contains("hunter2"), "got: {error}");
    }

    #[test]
    fn the_stripe_api_base_is_overridable_for_mocks_and_validated() {
        let lookup = lookup_from(&[
            ("STRIPE_SECRET_KEY", "sk_test_abc123"),
            ("STRIPE_API_BASE", " http://127.0.0.1:9999/ "),
        ]);
        let config = AppConfig::from_lookup(lookup).expect("a base URL must parse");
        assert_eq!(
            config
                .stripe
                .as_ref()
                .map(super::StripeConfig::api_base)
                .expect("stripe config must be present"),
            "http://127.0.0.1:9999",
            "the trailing slash is trimmed so paths append cleanly",
        );

        let lookup = lookup_from(&[
            ("STRIPE_SECRET_KEY", "sk_test_abc123"),
            ("STRIPE_API_BASE", "api.stripe.com"),
        ]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk base URLs are rejected");
        assert_eq!(error.variable(), "STRIPE_API_BASE");
    }

    #[test]
    fn production_boots_without_stripe() {
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("missing billing must not block boot");

        assert!(config.stripe.is_none());
    }

    #[test]
    fn environment_predicates_match_variants() {
        assert!(Environment::Development.is_development());
        assert!(Environment::Test.is_test());
        assert!(Environment::Production.is_production());
        assert!(!Environment::Production.is_development());
    }
}
