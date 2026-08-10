# Email

All outgoing email goes through `anubis::mail::Mailer`, a cheap-to-clone service with pluggable backends. Framework flows (verification, password reset) and application code use the same service.

## Backends

- **`Mailer::log`**: writes each email to structured logs, action links included. The development default; nothing leaves the machine, and the logged link is how you complete verification flows locally.
- **`Mailer::test`**: captures emails in an in-memory outbox whose handle tests read, in the spirit of Rails' `ActionMailer::Base.deliveries`. Framework integration tests and application tests both use it.
- **SMTP**: on the roadmap (tracked in GitHub issues). `Mailer::send` is async and fallible so a transport backend slots in without call-site changes.

## Email-driven auth flows

Verification and reset links use single-use tokens with the same discipline as sessions: random 256-bit token to the client, SHA-256 at rest, consumption deletes the row atomically. Lifetimes: 3 days for email verification, 30 minutes for password reset. Issuing a new token replaces any outstanding one for that user and purpose.

Links point at `APP_URL` (`/verify-email?token=...`, `/reset-password?token=...`), where the SPA pages complete the flow against the confirm endpoints. `APP_URL` defaults to the bind address in development and must be set explicitly in production.

A completed password reset revokes every session the user has.

Password-reset requests answer identically whether or not the email is registered, so responses cannot probe which addresses exist.

## Content

Bodies are plain text today; HTML templating is on the roadmap. User-facing email copy is English until backend i18n lands (also roadmap).
