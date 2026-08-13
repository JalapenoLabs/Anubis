// Copyright © 2026 Jalapeno Labs

import type { WireCreationOptions, WireRequestOptions } from './ceremony'

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { arrayBufferToBase64Url } from './encoding'
import {
  serializeAuthenticationCredential,
  serializeRegistrationCredential,
  toCredentialCreationOptions,
  toCredentialRequestOptions,
} from './ceremony'

function bufferOf(...bytes: number[]): ArrayBuffer {
  return new Uint8Array(bytes).buffer
}

/** Reads a decoded ceremony member back out, whichever buffer flavor it is. */
function bytesOf(source: BufferSource | undefined): number[] {
  if (!source) {
    console.debug('bytesOf received no buffer, returning no bytes')
    return []
  }
  if (ArrayBuffer.isView(source)) {
    return [ ...new Uint8Array(source.buffer, source.byteOffset, source.byteLength) ]
  }
  return [ ...new Uint8Array(source) ]
}

const creationChallenge: WireCreationOptions = {
  publicKey: {
    rp: { id: 'localhost', name: 'localhost' },
    user: { id: 'AAEC', name: 'alex@example.com', displayName: 'Alex' },
    challenge: 'AAECAw',
    pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
    timeout: 60_000,
    excludeCredentials: [{ type: 'public-key', id: '-_8' }],
    attestation: 'none',
  },
}

const requestChallenge: WireRequestOptions = {
  publicKey: {
    challenge: 'AAECAw',
    rpId: 'localhost',
    allowCredentials: [],
    userVerification: 'preferred',
    timeout: 60_000,
  },
}

describe('toCredentialCreationOptions', () => {
  it('should decode the challenge, the user handle, and the excluded ids', () => {
    const options = toCredentialCreationOptions(creationChallenge)
    const publicKey = options.publicKey

    expect(publicKey).toBeDefined()
    expect(bytesOf(publicKey?.challenge)).toEqual([ 0, 1, 2, 3 ])
    expect(bytesOf(publicKey?.user.id)).toEqual([ 0, 1, 2 ])
    expect(bytesOf(publicKey?.excludeCredentials?.[0].id)).toEqual([ 251, 255 ])
  })

  it('should pass every other member through untouched', () => {
    const publicKey = toCredentialCreationOptions(creationChallenge).publicKey

    expect(publicKey?.rp).toEqual({ id: 'localhost', name: 'localhost' })
    expect(publicKey?.user.name).toBe('alex@example.com')
    expect(publicKey?.pubKeyCredParams).toEqual([{ type: 'public-key', alg: -7 }])
    expect(publicKey?.timeout).toBe(60_000)
    expect(publicKey?.attestation).toBe('none')
  })
})

describe('toCredentialRequestOptions', () => {
  it('should decode the challenge and keep the relying party id', () => {
    const publicKey = toCredentialRequestOptions(requestChallenge).publicKey

    expect(bytesOf(publicKey?.challenge)).toEqual([ 0, 1, 2, 3 ])
    expect(publicKey?.rpId).toBe('localhost')
    expect(publicKey?.userVerification).toBe('preferred')
  })

  it('should leave a discoverable ceremony with no allowed credentials', () => {
    const publicKey = toCredentialRequestOptions(requestChallenge).publicKey

    expect(publicKey?.allowCredentials).toEqual([])
  })
})

describe('serializeRegistrationCredential', () => {
  it('should encode every binary member as base64url', () => {
    const payload = serializeRegistrationCredential({
      id: 'credential-id',
      type: 'public-key',
      rawId: bufferOf(0, 1, 2, 3),
      response: {
        attestationObject: bufferOf(251, 255),
        clientDataJSON: bufferOf(4, 5),
      },
    })

    expect(payload).toEqual({
      id: 'credential-id',
      type: 'public-key',
      rawId: 'AAECAw',
      response: {
        attestationObject: '-_8',
        clientDataJSON: 'BAU',
      },
    })
  })

  it('should reject a result that is not a credential', () => {
    expect(() => serializeRegistrationCredential(null)).toThrow()
    expect(() => serializeRegistrationCredential({ id: 'only-an-id' })).toThrow()
  })

  it('should reject an assertion handed to the registration serializer', () => {
    expect(() => serializeRegistrationCredential({
      id: 'credential-id',
      type: 'public-key',
      rawId: bufferOf(0),
      response: {
        authenticatorData: bufferOf(1),
        clientDataJSON: bufferOf(2),
        signature: bufferOf(3),
        userHandle: null,
      },
    })).toThrow()
  })
})

describe('serializeAuthenticationCredential', () => {
  it('should encode every binary member as base64url', () => {
    const payload = serializeAuthenticationCredential({
      id: 'credential-id',
      type: 'public-key',
      rawId: bufferOf(0, 1, 2, 3),
      response: {
        authenticatorData: bufferOf(1, 2),
        clientDataJSON: bufferOf(4, 5),
        signature: bufferOf(6, 7),
        userHandle: bufferOf(8, 9),
      },
    })

    expect(payload.rawId).toBe('AAECAw')
    expect(payload.response.authenticatorData).toBe(arrayBufferToBase64Url(bufferOf(1, 2)))
    expect(payload.response.signature).toBe(arrayBufferToBase64Url(bufferOf(6, 7)))
    expect(payload.response.userHandle).toBe(arrayBufferToBase64Url(bufferOf(8, 9)))
  })

  it('should keep an absent user handle null rather than encoding it', () => {
    const payload = serializeAuthenticationCredential({
      id: 'credential-id',
      type: 'public-key',
      rawId: bufferOf(0),
      response: {
        authenticatorData: bufferOf(1),
        clientDataJSON: bufferOf(2),
        signature: bufferOf(3),
        userHandle: null,
      },
    })

    expect(payload.response.userHandle).toBeNull()
  })

  it('should reject a result missing its signature', () => {
    expect(() => serializeAuthenticationCredential({
      id: 'credential-id',
      type: 'public-key',
      rawId: bufferOf(0),
      response: {
        authenticatorData: bufferOf(1),
        clientDataJSON: bufferOf(2),
        userHandle: null,
      },
    })).toThrow()
  })
})
