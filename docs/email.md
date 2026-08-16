# Email

All outgoing email goes through `anubis::mail::Mailer`, a cheap-to-clone service with pluggable backends. Framework flows (verification, password reset) and application code use the same service.

## Backends

- **`Mailer::log`**: writes each email to structured logs, action links included. The development default; nothing leaves the machine, and the logged link is how you complete verification flows locally.
- **`Mailer::test`**: captures emails in an in-memory outbox whose handle tests read, in the spirit of Rails' `ActionMailer::Base.deliveries`. Framework integration tests and application tests both use it.
- **`Mailer::smtp`**: delivers through an SMTP relay, optionally signing each message with DKIM. The production backend, built on `lettre` with rustls and the RustCrypto signing stack; no OpenSSL is linked anywhere in the tree.

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

### DKIM signing

Relay providers sign for you: verify the sender domain with Postmark, SES, or whoever carries the mail, publish the records they hand you, and every message leaves signed. That is the default, and it needs nothing here.

Anubis signs messages itself when the relay does not: a company MTA, a sidecar, an appliance, anything that forwards a message untouched. Signing is additive, so a message signed twice is fine, and a deployment that switches providers can leave its own signature in place.

Three variables turn it on:

| Variable | Meaning |
|---|---|
| `DKIM_PRIVATE_KEY` | The signing key: a PKCS#1 RSA key in PEM form, or the base64 seed of an ed25519 key |
| `DKIM_SELECTOR` | The name the key is published under in DNS, e.g. `mail` |
| `DKIM_DOMAIN` | The domain the signature claims. Defaults to the `MAIL_FROM` domain |

They are a group. `DKIM_PRIVATE_KEY` and `DKIM_SELECTOR` are required together, `DKIM_DOMAIN` defaults, and setting part of the group stops the boot naming the rest. Signing without `SMTP_URL` stops the boot too: the log mailer delivers nothing to sign.

`DKIM_DOMAIN` must be the sender's domain or one of its parents, which is what DMARC's relaxed alignment accepts. A key at `acme.com` may sign for `no-reply@mail.acme.com`; a key at `example.com` may not, and the boot fails rather than shipping mail that verifies and fails DMARC anyway.

Each message is signed over `From`, `Subject`, `To`, and `Date`, with simple header and relaxed body canonicalization. The key is read once when the mailer is built, so an unreadable key fails startup rather than every send, and it never appears in an error, a log line, or `Debug` output.

#### Generating a key

RSA is the safe choice: every verifier checks `rsa-sha256`, while ed25519 (RFC 8463) is still uneven in the wild. Sign with RSA unless you know your recipients.

```bash
# RSA. `-traditional` is required: it writes PKCS#1, the form the signer reads.
# Plain `openssl genrsa` on OpenSSL 3 writes PKCS#8, which is refused.
openssl genrsa -traditional -out dkim.private.pem 2048

# ed25519, whose private key is the 32-byte seed rather than a PEM.
openssl genpkey -algorithm ED25519 -out dkim.ed25519.pem
openssl pkey -in dkim.ed25519.pem -outform DER | tail -c 32 | openssl base64 -A
```

A PEM is multi-line and plenty of places a deployment stores secrets hold one line, so `\n` escapes are accepted and mean the same key:

```bash
DKIM_PRIVATE_KEY='-----BEGIN RSA PRIVATE KEY-----\nMIIEow...\n-----END RSA PRIVATE KEY-----'
DKIM_SELECTOR=mail
```

Nothing in the environment names the algorithm: the `-----BEGIN` a PEM opens with is what tells the two apart.

#### Publishing the public key

The public half goes in DNS as a TXT record at `<selector>._domainkey.<domain>`. For `DKIM_SELECTOR=mail` and `DKIM_DOMAIN=acme.com`, that is `mail._domainkey.acme.com`:

```
v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A...
```

Derive `p=` from the key you generated:

```bash
# RSA: the base64 of the DER SubjectPublicKeyInfo.
openssl rsa -in dkim.private.pem -pubout -outform DER | openssl base64 -A

# ed25519: the raw 32-byte public key, and the record reads `k=ed25519`.
openssl pkey -in dkim.ed25519.pem -pubout -outform DER | tail -c 32 | openssl base64 -A
```

Providers differ on whether the value must be quoted or split into 255-character chunks; both are the same record, and `dig +short TXT mail._domainkey.acme.com` shows what the world sees. Publish before you set `DKIM_PRIVATE_KEY`: a signature nobody can look up is worse than no signature, because a verifier treats the lookup failure as a failed check.

#### Rotation

Selectors exist so a key can be replaced without a gap. Rotate in three steps, each its own deploy:

1. **Publish.** Generate a new key and publish it under a new selector, say `mail2`, leaving the old record in place. Mail keeps flowing, signed by the old key.
2. **Switch.** Set `DKIM_PRIVATE_KEY` and `DKIM_SELECTOR` to the new pair. New mail is signed by the new key; mail already in flight still verifies against the old record.
3. **Retire.** Once nothing in the world could still be checking the old signature, delete the old TXT record and the old key. A week is generous.

Rotate on a schedule you can live with, and immediately if the key leaks. Rotating in the other order, deleting first, breaks every message in flight.

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
