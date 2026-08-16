// Copyright © 2026 Jalapeno Labs

import type {
  AuthSession,
  ChangePasswordRequest,
  EmailChangeRequest,
  MessageEnvelope,
  ProfileUpdate,
  User,
  WireUserEnvelope,
} from '../types'
import type { KyInstance } from 'ky'

// Misc
import { toUser } from '../types'

type WireSession = {
  id: string
  created_at: string
  expires_at: string
  current: boolean
}

/** Signed-in account management: profile, avatar, credentials, sessions. */
export function createAccountRoutes(client: KyInstance) {
  async function updateProfile(update: ProfileUpdate): Promise<User> {
    const response = await client
      .patch('auth/profile', {
        json: {
          first_name: update.firstName,
          last_name: update.lastName,
          time_zone: update.timeZone,
          locale: update.locale,
        },
      })
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  /**
   * Stores an avatar, answering with the versioned URL that serves it.
   *
   * The image is the request body rather than a multipart part: there is one
   * file and no other field, and the server crops, resizes, and re-encodes it
   * anyway, so the browser has nothing useful to say about it.
   *
   * Refresh the current user after this: the profile payload carries the new
   * version, which is what updates every other view of the account.
   */
  async function uploadAvatar(image: Blob): Promise<string> {
    const response = await client
      .post('auth/profile/avatar', {
        body: image,
        headers: { 'content-type': image.type || 'application/octet-stream' },
      })
      .json<{ avatar_url: string }>()
    return response.avatar_url
  }

  async function deleteAvatar(): Promise<void> {
    await client.delete('auth/profile/avatar')
  }

  /** Rotates the password; every other session signs out. */
  function changePassword(request: ChangePasswordRequest) {
    return client
      .post('auth/change-password', {
        json: {
          current_password: request.currentPassword,
          new_password: request.newPassword,
        },
      })
      .json<MessageEnvelope>()
  }

  /** Mails a confirmation link to the new address; nothing changes until it is clicked. */
  function requestEmailChange(request: EmailChangeRequest) {
    return client
      .post('auth/change-email/request', {
        json: {
          new_email: request.newEmail,
          password: request.password,
        },
      })
      .json<MessageEnvelope>()
  }

  async function confirmEmailChange(token: string): Promise<User> {
    const response = await client
      .post('auth/change-email/confirm', { json: { token }})
      .json<WireUserEnvelope>()
    return toUser(response.user)
  }

  async function listSessions(): Promise<AuthSession[]> {
    const response = await client
      .get('auth/sessions')
      .json<{ sessions: WireSession[] }>()
    return response.sessions.map((session) => ({
      id: session.id,
      createdAt: session.created_at,
      expiresAt: session.expires_at,
      isCurrent: session.current,
    }))
  }

  async function revokeSession(sessionId: string): Promise<void> {
    await client.delete(`auth/sessions/${sessionId}`)
  }

  async function deleteAccount(password: string): Promise<void> {
    await client.delete('auth/account', { json: { password }})
  }

  return {
    updateProfile,
    uploadAvatar,
    deleteAvatar,
    changePassword,
    requestEmailChange,
    confirmEmailChange,
    listSessions,
    revokeSession,
    deleteAccount,
  } as const
}
