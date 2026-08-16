//! A software passkey, so a test can finish a real WebAuthn ceremony.
//!
//! [`SoftAuthenticator`] stands in for the browser and the security key
//! together: it builds the `clientDataJSON` a browser would collect, and signs
//! it with a real ES256 key. Everything the relying party verifies is
//! genuine, so a test that gets a session out of it has proven the
//! cryptographic path, not a fixture.
//!
//! The signing is [`SoftPasskey`]'s, from `webauthn-authenticator-rs`. What
//! this type adds is the one thing that token does not keep: **resident
//! credentials**. A discoverable authenticator remembers, per credential, the
//! user handle it was created for, chooses among its own credentials when a
//! request names none, and reports the handle back so the relying party learns
//! whose credential signed. `SoftPasskey` keeps no such store and answers only
//! requests that name a credential, so the store lives here.
//!
//! Neither piece this adds is signed data. The credential choice is the
//! authenticator's alone, and the user handle rides beside the assertion
//! rather than inside it, which is why a relying party treats it as a lookup
//! hint and then verifies the signature against the key it finds. Anubis'
//! login does exactly that, so the test exercises it exactly as written.

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};

use serde_json::Value;
use webauthn_authenticator_rs::AuthenticatorBackendHashedClientData;
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_authenticator_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Url, WebauthnAuthenticator,
};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs_proto::{
    AllowCredentials, PublicKeyCredentialCreationOptions, PublicKeyCredentialRequestOptions,
};

/// The credential type every WebAuthn descriptor carries.
const PUBLIC_KEY: &str = "public-key";

/// A discoverable software authenticator plugged into a browser at one origin.
pub struct SoftAuthenticator {
    /// The origin the browser reports, which every ceremony signs over.
    origin: Url,
    /// The ES256 token that holds the keys and does the signing.
    token: SoftPasskey,
    /// The resident credentials: credential id to user handle.
    residents: HashMap<Vec<u8>, Vec<u8>>,
}

impl Debug for SoftAuthenticator {
    /// Names the origin and how many credentials are resident; `SoftPasskey`
    /// implements no `Debug`, and its key material should not render anyway.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoftAuthenticator")
            .field("origin", &self.origin.as_str())
            .field("residents", &self.residents.len())
            .finish_non_exhaustive()
    }
}

impl SoftAuthenticator {
    /// Plugs a fresh authenticator into a browser sitting at `origin`.
    ///
    /// The origin must be `https://` or a `localhost` URL, which is the rule
    /// browsers enforce and this library enforces with them.
    ///
    /// # Panics
    /// Panics when `origin` is not a URL.
    pub fn new(origin: &str) -> Self {
        Self {
            origin: Url::parse(origin).expect("the test origin must be a URL"),
            // User verification: the token reports the UV it never really
            // performs, because passkey registration asks for it and a
            // software token has no biometric to offer.
            token: SoftPasskey::new(true),
            residents: HashMap::new(),
        }
    }

    /// Answers a registration challenge, as `navigator.credentials.create`
    /// does, and keeps the credential resident.
    ///
    /// Takes the `creation_options` the API's register-start returned, still
    /// in the wire form the browser receives, and gives back the credential to
    /// post to register-finish. The credential is returned typed rather than
    /// as JSON so that a test can tamper with one field and prove the finish
    /// notices.
    ///
    /// # Panics
    /// Panics when the options are not a WebAuthn challenge, or when the
    /// authenticator refuses them.
    pub fn create(&mut self, creation_options: &Value) -> RegisterPublicKeyCredential {
        let options: CreationChallengeResponse = serde_json::from_value(creation_options.clone())
            .expect("register-start must return creation options");

        self.do_registration(self.origin.clone(), options)
            .expect("the authenticator must answer the creation challenge")
    }

    /// Answers an authentication challenge, as `navigator.credentials.get`
    /// does, with one of the credentials it holds.
    ///
    /// # Panics
    /// Panics when the options are not a WebAuthn challenge, or when the
    /// authenticator holds no credential that can answer it.
    pub fn get(&mut self, request_options: &Value) -> PublicKeyCredential {
        let options: RequestChallengeResponse = serde_json::from_value(request_options.clone())
            .expect("login-start must return request options");

        self.do_authentication(self.origin.clone(), options)
            .expect("the authenticator must answer the request challenge")
    }
}

/// The authenticator half of the ceremony: sign what the browser collected.
///
/// Implementing the hashed-client-data trait rather than
/// [`webauthn_authenticator_rs::AuthenticatorBackend`] is what earns the
/// browser half for free: the library builds the `clientDataJSON`, hashes it,
/// and puts it back into the response, so the only thing written here is what
/// a discoverable authenticator itself would do.
impl AuthenticatorBackendHashedClientData for SoftAuthenticator {
    fn perform_register(
        &mut self,
        client_data_hash: Vec<u8>,
        options: PublicKeyCredentialCreationOptions,
        timeout_ms: u32,
    ) -> Result<RegisterPublicKeyCredential, WebauthnCError> {
        let user_handle = options.user.id.clone();
        let credential = self
            .token
            .perform_register(client_data_hash, options, timeout_ms)?;

        self.residents
            .insert(credential.raw_id.clone(), user_handle);
        Ok(credential)
    }

    fn perform_auth(
        &mut self,
        client_data_hash: Vec<u8>,
        mut options: PublicKeyCredentialRequestOptions,
        timeout_ms: u32,
    ) -> Result<PublicKeyCredential, WebauthnCError> {
        // A discoverable request names no credential: choosing one is the
        // authenticator's job, and the token only answers requests that name
        // one, so the resident credentials are offered to it here.
        if options.allow_credentials.is_empty() {
            options.allow_credentials = self
                .residents
                .keys()
                .map(|credential_id| AllowCredentials {
                    type_: PUBLIC_KEY.to_owned(),
                    id: credential_id.clone(),
                    transports: None,
                })
                .collect();
        }

        let mut assertion = self
            .token
            .perform_auth(client_data_hash, options, timeout_ms)?;

        // The user handle is what makes a credential discoverable: it is how
        // the relying party learns whose credential signed, before it has
        // anything to verify the signature against.
        assertion.response.user_handle = self.residents.get(&assertion.raw_id).cloned();
        Ok(assertion)
    }
}
