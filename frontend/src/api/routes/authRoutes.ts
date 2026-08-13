// Copyright © 2026 Jalapeno Labs

import type {
  Credentials,
  MessageEnvelope,
  MfaStatus,
  Passkey,
  PasskeyLoginChallenge,
  PasskeyRegistrationChallenge,
  SignInResult,
  TotpEnrollment,
  User,
  WireMfaChallenge,
  WireUserEnvelope,
} from '../types'
import type {
  AuthenticationCredentialPayload,
  RegistrationCredentialPayload,
} from '../../webauthn/ceremony'
import type { KyInstance } from 'ky'

// Misc
import { toSignInResult, toUser } from '../types'

type WirePasskey = {
  id: string
  name: string
  created_at: string
  last_used_at: string | null
}

/**
 * The sign-in surface: credentials, one-time codes, second factors, passkeys.
 *
 * Every route here is reachable signed out, except the passkey registration
 * ceremony and the TOTP enrollment, which are the signed-in half of the same
 * two features and stay beside their login halves.
 */
export function createAuthRoutes(client: KyInstance) {
  async function register(credentials: Credentials): Promise<User> {
    const response = await client
      .post('auth/register', { json: credentials })
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  async function login(credentials: Credentials): Promise<SignInResult> {
    const response = await client
      .post('auth/login', { json: credentials })
      .json<WireUserEnvelope | WireMfaChallenge>()
    return toSignInResult(response)
  }

  async function logout(): Promise<void> {
    await client.post('auth/logout')
  }

  async function me(): Promise<User> {
    const response = await client
      .get('auth/me')
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  function requestEmailVerification() {
    return client
      .post('auth/verify-email/request')
      .json<MessageEnvelope>()
  }

  async function confirmEmailVerification(token: string): Promise<User> {
    const response = await client
      .post('auth/verify-email/confirm', { json: { token }})
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  function requestPasswordReset(email: string) {
    return client
      .post('auth/password-reset/request', { json: { email }})
      .json<MessageEnvelope>()
  }

  function confirmPasswordReset(token: string, password: string) {
    return client
      .post('auth/password-reset/confirm', { json: { token, password }})
      .json<MessageEnvelope>()
  }

  /**
   * Mails a sign-in code, answering the same way for unregistered addresses.
   *
   * The uniform answer is the contract: a caller cannot use this route to
   * learn which addresses have accounts.
   */
  function requestEmailSignInCode(email: string) {
    return client
      .post('auth/email-code/request', { json: { email }})
      .json<MessageEnvelope>()
  }

  async function verifyEmailSignInCode(email: string, code: string): Promise<SignInResult> {
    const response = await client
      .post('auth/email-code/verify', { json: { email, code }})
      .json<WireUserEnvelope | WireMfaChallenge>()
    return toSignInResult(response)
  }

  async function getMfaStatus(): Promise<MfaStatus> {
    const response = await client
      .get('auth/mfa')
      .json<{ totp_enabled: boolean }>()
    return { totpEnabled: response.totp_enabled }
  }

  async function startTotpEnrollment(): Promise<TotpEnrollment> {
    const response = await client
      .post('auth/mfa/totp/setup')
      .json<{ secret: string, otpauth_uri: string }>()
    return {
      secret: response.secret,
      otpauthUri: response.otpauth_uri,
    }
  }

  /** Activates the pending enrollment, answering with the recovery codes. */
  async function confirmTotpEnrollment(code: string): Promise<string[]> {
    const response = await client
      .post('auth/mfa/totp/confirm', { json: { code }})
      .json<{ recovery_codes: string[] }>()
    return response.recovery_codes
  }

  async function disableTotp(password: string): Promise<void> {
    await client.post('auth/mfa/totp/disable', { json: { password }})
  }

  /** Exchanges a login challenge plus a TOTP or recovery code for a session. */
  async function verifyMfaChallenge(mfaToken: string, code: string): Promise<User> {
    const response = await client
      .post('auth/mfa/verify', { json: { mfa_token: mfaToken, code }})
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  async function listPasskeys(): Promise<Passkey[]> {
    const response = await client
      .get('auth/passkeys')
      .json<{ passkeys: WirePasskey[] }>()
    return response.passkeys.map((passkey) => ({
      id: passkey.id,
      name: passkey.name,
      createdAt: passkey.created_at,
      lastUsedAt: passkey.last_used_at,
    }))
  }

  async function deletePasskey(passkeyId: string): Promise<void> {
    await client.delete(`auth/passkeys/${passkeyId}`)
  }

  async function startPasskeyRegistration(): Promise<PasskeyRegistrationChallenge> {
    const response = await client
      .post('auth/passkeys/register/start')
      .json<{ state_token: string, creation_options: PasskeyRegistrationChallenge['creationOptions'] }>()
    return {
      stateToken: response.state_token,
      creationOptions: response.creation_options,
    }
  }

  async function finishPasskeyRegistration(
    stateToken: string,
    credential: RegistrationCredentialPayload,
    name: string,
  ): Promise<void> {
    await client.post('auth/passkeys/register/finish', {
      json: {
        state_token: stateToken,
        credential,
        name,
      },
    })
  }

  async function startPasskeyLogin(): Promise<PasskeyLoginChallenge> {
    const response = await client
      .post('auth/passkeys/login/start')
      .json<{ state_token: string, request_options: PasskeyLoginChallenge['requestOptions'] }>()
    return {
      stateToken: response.state_token,
      requestOptions: response.request_options,
    }
  }

  /** A passkey is multi-factor by construction, so this always yields a session. */
  async function finishPasskeyLogin(
    stateToken: string,
    credential: AuthenticationCredentialPayload,
  ): Promise<User> {
    const response = await client
      .post('auth/passkeys/login/finish', {
        json: {
          state_token: stateToken,
          credential,
        },
      })
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  return {
    register,
    login,
    logout,
    me,
    requestEmailVerification,
    confirmEmailVerification,
    requestPasswordReset,
    confirmPasswordReset,
    requestEmailSignInCode,
    verifyEmailSignInCode,
    getMfaStatus,
    startTotpEnrollment,
    confirmTotpEnrollment,
    disableTotp,
    verifyMfaChallenge,
    listPasskeys,
    deletePasskey,
    startPasskeyRegistration,
    finishPasskeyRegistration,
    startPasskeyLogin,
    finishPasskeyLogin,
  } as const
}
