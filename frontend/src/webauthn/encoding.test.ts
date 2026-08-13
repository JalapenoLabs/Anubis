// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Misc
import { arrayBufferToBase64Url, base64UrlToArrayBuffer } from './encoding'

function bufferOf(...bytes: number[]): ArrayBuffer {
  return new Uint8Array(bytes).buffer
}

describe('base64UrlToArrayBuffer', () => {
  it('should decode a padded-length input', () => {
    expect([ ...new Uint8Array(base64UrlToArrayBuffer('AAEC')) ]).toEqual([ 0, 1, 2 ])
  })

  it('should decode inputs whose length needs one or two pad characters', () => {
    expect([ ...new Uint8Array(base64UrlToArrayBuffer('AAECAw')) ]).toEqual([ 0, 1, 2, 3 ])
    expect([ ...new Uint8Array(base64UrlToArrayBuffer('AAECAwQ')) ]).toEqual([ 0, 1, 2, 3, 4 ])
  })

  it('should decode the url-safe alphabet, which plain base64 cannot', () => {
    // 0xFB 0xFF decodes to `+/` in base64 and to `-_` in base64url.
    expect([ ...new Uint8Array(base64UrlToArrayBuffer('-_8')) ]).toEqual([ 251, 255 ])
  })

  it('should decode an empty string to an empty buffer', () => {
    expect(base64UrlToArrayBuffer('').byteLength).toBe(0)
  })
})

describe('arrayBufferToBase64Url', () => {
  it('should encode without padding', () => {
    expect(arrayBufferToBase64Url(bufferOf(0, 1, 2, 3))).toBe('AAECAw')
  })

  it('should use the url-safe alphabet', () => {
    expect(arrayBufferToBase64Url(bufferOf(251, 255))).toBe('-_8')
  })

  it('should encode high bytes without mangling them', () => {
    expect(arrayBufferToBase64Url(bufferOf(255, 254, 253))).toBe('__79')
  })
})

describe('the encoding round trip', () => {
  it('should survive every byte value', () => {
    const everyByte = new Uint8Array(256)
    for (let index = 0; index < everyByte.length; index += 1) {
      everyByte[index] = index
    }

    const encoded = arrayBufferToBase64Url(everyByte.buffer)
    expect([ ...new Uint8Array(base64UrlToArrayBuffer(encoded)) ]).toEqual([ ...everyByte ])
  })
})
