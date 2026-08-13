# Email

All outgoing email goes through `anubis::mail::Mailer`, a cheap-to-clone service with pluggable backends. Framework flows (verification, password reset) and application code use the same service.

## Backends

- **`Mailer::log`**: writes each email to structured logs, action links included. The development default; nothing leaves the machine, and the logged link is how you complete verification flows locally.
- **`Mailer::test`**: captures emails in an in-memory outbox whose handle tests read, in the spirit of Rails' `ActionMailer::Base.deliveries`. Framework integration tests and application tests both use it.
- **`Mailer::smtp`**: delivers through an SMTP relay. The production backend, built on `lettre` with rustls; no OpenSSL is linked anywhere in the tree.

`Mailer::from_config` picks the backend, so which one runs is a deployment decision rather than a code change. The starter's composition root calls it.

## SMTP

Two variables configure delivery:

| Variable | Meaning |
|---|---|
| `SMTP_URL` | The relay, credentials included: `smtps://user:password@smtp.example.com:465` |
| `MAIL_FROM` | Sender of every outgoing email: `Acme <no-reply@acme.com>`. Required when `SMTP_URL` is set |

One URL carries the host, port, credentials, and TLS mode, which is what relay providers hand you anyway. Both TLS forms work:

- `smtps://smtp.example.com:465` opens TLS on connect (implicit TLS).
- `smtp://smtp.example.com:587?tls=required` connects in the clear and upgrades with STARTTLS, refusing to send if the upgrade fails.

Plain `smtp://host:port` with no `tls=` parameter sends unencrypted and is only appropriate for a relay on a trusted link, such as a sidecar on localhost. Credentials containing URL syntax (`@`, `:`, `/`) must be percent-encoded.

Provider examples, using each provider's generic SMTP credentials:

```bash
# Postmark
SMTP_URL='smtp://<server-token>:<server-token>@smtp.postmarkapp.com:587?tls=required'

# Amazon SES (the SMTP credentials from the SES console, not an AWS access key)
SMTP_URL='smtps://<ses-smtp-user>:<ses-smtp-password>@email-smtp.us-east-1.amazonaws.com:465'

MAIL_FROM='Acme <no-reply@acme.com>'
```

The sender domain still needs SPF, DKIM, and DMARC records at the provider, or mail lands in spam regardless of the transport.

### Per environment

`SMTP_URL` is the switch, and it works the same in every environment: set it and email is delivered over SMTP, so staging can send real mail and a developer can point at a local catcher such as Mailpit.

Leaving it unset keeps the log mailer in development and test, which is what makes `yarn dev` zero-configuration: the verification link is printed to the console.

Production without `SMTP_URL` still boots, and logs a prominent warning at startup that email delivery is disabled. A hard failure would block a first deploy that has nothing to email anyone about, and would turn a mail outage into an outage of the whole application. The warning names the flows that silently stop working: verification, password reset, sign-in codes, and invitations.

### Failure behavior

Connections are pooled and opened lazily, so building the mailer performs no I/O and startup never waits on a relay. Anubis deliberately runs no startup connectivity probe: relays throttle connections, and a probe would spend that budget on every deploy and every restart. The cost is that a wrong host, a refused credential, or a blocked port surfaces at the **first send**, not at boot, so smoke-test a deploy by triggering one real email.

Delivery failures return `mail::Error` from `Mailer::send`, the same type and the same call sites the log and test backends already use. The relay URL is a secret: it never appears in an error message, a log line, or `Debug` output.

## Email-driven auth flows

Verification and reset links use single-use tokens with the same discipline as sessions: random 256-bit token to the client, SHA-256 at rest, consumption deletes the row atomically. Lifetimes: 3 days for email verification, 30 minutes for password reset. Issuing a new token replaces any outstanding one for that user and purpose.

Links point at `APP_URL` (`/verify-email?token=...`, `/reset-password?token=...`), where the SPA pages complete the flow against the confirm endpoints. `APP_URL` defaults to the bind address in development and must be set explicitly in production.

A completed password reset revokes every session the user has.

Password-reset requests answer identically whether or not the email is registered, so responses cannot probe which addresses exist.

## Content

Bodies are plain text today; HTML templating is on the roadmap. User-facing email copy is English until backend i18n lands (also roadmap).
