// Copyright © 2026 Jalapeno Labs

/**
 * The user shape returned by every auth endpoint.
 *
 * Mirrors the backend's UserResponse serializer. The M3 client generator will
 * replace these handwritten types with generated ones.
 */
export type User = {
  id: string
  email: string
  emailVerified: boolean
  createdAt: string
}

export type UserEnvelope = {
  user: User
}

export type MessageEnvelope = {
  message: string
}

export type Credentials = {
  email: string
  password: string
}

/** Raw wire shape; the backend serializes snake_case fields. */
export type WireUser = {
  id: string
  email: string
  email_verified: boolean
  created_at: string
}

export type WireUserEnvelope = {
  user: WireUser
}

export function toUser(wire: WireUser): User {
  return {
    id: wire.id,
    email: wire.email,
    emailVerified: wire.email_verified,
    createdAt: wire.created_at,
  }
}
