// Copyright © 2026 Jalapeno Labs

// Misc
import { arrayBufferToBase64Url, base64UrlToArrayBuffer } from './encoding'

/**
 * A credential descriptor as JSON: the id arrives base64url encoded.
 *
 * The wire types are the DOM's own with their binary members retyped, so a
 * field the specification adds reaches the browser without a change here.
 */
export type WireCredentialDescriptor = Omit<PublicKeyCredentialDescriptor, 'id'> & {
  id: string
}

export type WireCreationOptions = {
  publicKey: Omit<PublicKeyCredentialCreationOptions, 'challenge' | 'user' | 'excludeCredentials'> & {
    challenge: string
    user: Omit<PublicKeyCredentialUserEntity, 'id'> & { id: string }
    excludeCredentials?: WireCredentialDescriptor[]
  }
}

export type WireRequestOptions = {
  publicKey: Omit<PublicKeyCredentialRequestOptions, 'challenge' | 'allowCredentials'> & {
    challenge: string
    allowCredentials?: WireCredentialDescriptor[]
  }
}

/** What `navigator.credentials.create()` produced, ready to post back. */
export type RegistrationCredentialPayload = {
  id: string
  rawId: string
  type: string
  response: {
    attestationObject: string
    clientDataJSON: string
  }
}

/** What `navigator.credentials.get()` produced, ready to post back. */
export type AuthenticationCredentialPayload = {
  id: string
  rawId: string
  type: string
  response: {
    authenticatorData: string
    clientDataJSON: string
    signature: string
    userHandle: string | null
  }
}

/** True when this browser can run a passkey ceremony at all. */
export function isPasskeySupported(): boolean {
  return typeof window !== 'undefined'
    && typeof window.PublicKeyCredential !== 'undefined'
    && typeof navigator.credentials?.create === 'function'
}

/** Turns the registration challenge into `navigator.credentials.create()` options. */
export function toCredentialCreationOptions(wire: WireCreationOptions): CredentialCreationOptions {
  const { challenge, user, excludeCredentials, ...rest } = wire.publicKey

  return {
    publicKey: {
      ...rest,
      challenge: base64UrlToArrayBuffer(challenge),
      user: {
        ...user,
        id: base64UrlToArrayBuffer(user.id),
      },
      excludeCredentials: excludeCredentials?.map((descriptor) => ({
        ...descriptor,
        id: base64UrlToArrayBuffer(descriptor.id),
      })),
    },
  }
}

/** Turns the login challenge into `navigator.credentials.get()` options. */
export function toCredentialRequestOptions(wire: WireRequestOptions): CredentialRequestOptions {
  const { challenge, allowCredentials, ...rest } = wire.publicKey

  return {
    publicKey: {
      ...rest,
      challenge: base64UrlToArrayBuffer(challenge),
      allowCredentials: allowCredentials?.map((descriptor) => ({
        ...descriptor,
        id: base64UrlToArrayBuffer(descriptor.id),
      })),
    },
  }
}

/**
 * Encodes a freshly created passkey for `passkeys/register/finish`.
 *
 * The argument is `unknown` because it comes from the authenticator, which is
 * as much a runtime boundary as an HTTP response: the DOM types promise a
 * `Credential`, and what arrives is whatever the platform actually handed
 * back. The type guards below are the validation, so callers pass the result
 * of `navigator.credentials.create()` straight through.
 */
export function serializeRegistrationCredential(credential: unknown): RegistrationCredentialPayload {
  if (!isAttestationCredential(credential)) {
    console.debug('serializeRegistrationCredential received no usable credential', credential)
    throw new Error('This browser returned no usable passkey. Try registering again.')
  }

  return {
    id: credential.id,
    rawId: arrayBufferToBase64Url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: arrayBufferToBase64Url(credential.response.attestationObject),
      clientDataJSON: arrayBufferToBase64Url(credential.response.clientDataJSON),
    },
  }
}

/** Encodes a signed assertion for `passkeys/login/finish`. */
export function serializeAuthenticationCredential(credential: unknown): AuthenticationCredentialPayload {
  if (!isAssertionCredential(credential)) {
    console.debug('serializeAuthenticationCredential received no usable credential', credential)
    throw new Error('This browser returned no usable passkey. Try signing in again.')
  }

  const userHandle = credential.response.userHandle

  return {
    id: credential.id,
    rawId: arrayBufferToBase64Url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: arrayBufferToBase64Url(credential.response.authenticatorData),
      clientDataJSON: arrayBufferToBase64Url(credential.response.clientDataJSON),
      signature: arrayBufferToBase64Url(credential.response.signature),
      userHandle: userHandle && arrayBufferToBase64Url(userHandle),
    },
  }
}

/** The members every ceremony result carries, whichever ceremony ran. */
type CredentialEnvelope = {
  id: string
  type: string
  rawId: ArrayBuffer
  response: object
}

type AttestationCredential = CredentialEnvelope & {
  response: {
    attestationObject: ArrayBuffer
    clientDataJSON: ArrayBuffer
  }
}

type AssertionCredential = CredentialEnvelope & {
  response: {
    authenticatorData: ArrayBuffer
    clientDataJSON: ArrayBuffer
    signature: ArrayBuffer
    userHandle: ArrayBuffer | null
  }
}

function isCredentialEnvelope(value: unknown): value is CredentialEnvelope {
  return typeof value === 'object'
    && value !== null
    && 'id' in value && typeof value.id === 'string'
    && 'type' in value && typeof value.type === 'string'
    && 'rawId' in value && value.rawId instanceof ArrayBuffer
    && 'response' in value && typeof value.response === 'object' && value.response !== null
}

function isAttestationCredential(value: unknown): value is AttestationCredential {
  return isCredentialEnvelope(value)
    && 'attestationObject' in value.response && value.response.attestationObject instanceof ArrayBuffer
    && 'clientDataJSON' in value.response && value.response.clientDataJSON instanceof ArrayBuffer
}

function isAssertionCredential(value: unknown): value is AssertionCredential {
  return isCredentialEnvelope(value)
    && 'authenticatorData' in value.response && value.response.authenticatorData instanceof ArrayBuffer
    && 'clientDataJSON' in value.response && value.response.clientDataJSON instanceof ArrayBuffer
    && 'signature' in value.response && value.response.signature instanceof ArrayBuffer
    && 'userHandle' in value.response
    && (value.response.userHandle === null || value.response.userHandle instanceof ArrayBuffer)
}
