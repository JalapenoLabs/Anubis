// Copyright © 2026 Jalapeno Labs

/**
 * Decodes a base64url string into the buffer a WebAuthn ceremony expects.
 *
 * JSON carries no binary, so every challenge, credential id, and user handle
 * crosses the wire base64url encoded without padding (RFC 4648 section 5) and
 * has to become a buffer before the browser will look at it.
 */
export function base64UrlToArrayBuffer(value: string): ArrayBuffer {
  const base64 = value.replaceAll('-', '+').replaceAll('_', '/')
  const paddingLength = (4 - (base64.length % 4)) % 4
  const binary = atob(base64.padEnd(base64.length + paddingLength, '='))

  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index)
  }

  return bytes.buffer
}

/** Encodes a ceremony result's binary member the way the server reads it. */
export function arrayBufferToBase64Url(buffer: ArrayBuffer): string {
  let binary = ''
  for (const byte of new Uint8Array(buffer)) {
    binary += String.fromCharCode(byte)
  }

  return btoa(binary)
    .replaceAll('+', '-')
    .replaceAll('/', '_')
    .replaceAll('=', '')
}
