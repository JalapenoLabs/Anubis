// Copyright © 2026 Jalapeno Labs

export type {
  AuthSession,
  ChangePasswordRequest,
  ClaimedInvitation,
  CreatedOrganization,
  Credentials,
  EmailChangeRequest,
  InviteMemberRequest,
  MembershipOrganization,
  MembershipTeam,
  MembershipsOverview,
  MessageEnvelope,
  MfaStatus,
  OauthProvider,
  OrganizationRosterMember,
  Passkey,
  PasskeyLoginChallenge,
  PasskeyRegistrationChallenge,
  ProfileUpdate,
  SignInResult,
  TeamRosterMember,
  TenancyOrganization,
  TenancyTeam,
  TotpEnrollment,
  User,
  UserEnvelope,
} from './api/types'
export type {
  AuthenticationCredentialPayload,
  RegistrationCredentialPayload,
  WireCreationOptions,
  WireRequestOptions,
} from './webauthn/ceremony'
export type { AnubisApi } from './api/createAnubisApi'
export type { AnubisV1, ErrorV1, TeamEnvelopeV1, TeamV1 } from './api/v1.generated'
export type { CurrentUserResult } from './react/useCurrentUser'
export type { MembershipsResult } from './react/useMemberships'
export type { OauthProvidersResult } from './react/useOauthProviders'
export type { AnubisFieldProps, FieldOption } from './fields/types'
export type { RealtimeListener } from './realtime/listeners'
export type {
  ClientFrame,
  RealtimeErrorCode,
  RealtimeEvent,
  ServerFrame,
} from './realtime/protocol'
export type {
  ConnectionState,
  RealtimeClientOptions,
  WebSocketLike,
} from './realtime/RealtimeClient'

// Fields
export { BooleanField } from './fields/BooleanField'
export { ButtonsField } from './fields/ButtonsField'
export { ColorPickerField } from './fields/ColorPickerField'
export { DateAndTimeField } from './fields/DateAndTimeField'
export { DateField } from './fields/DateField'
export { EmailField } from './fields/EmailField'
export { FieldWrapper } from './fields/FieldWrapper'
export { NumberField } from './fields/NumberField'
export { OptionsField } from './fields/OptionsField'
export { PasswordField } from './fields/PasswordField'
export { PhoneField } from './fields/PhoneField'
export { SuperSelectField } from './fields/SuperSelectField'
export { TextAreaField } from './fields/TextAreaField'
export { TextField } from './fields/TextField'
export { useFieldState } from './fields/useFieldState'

// WebAuthn
export { arrayBufferToBase64Url, base64UrlToArrayBuffer } from './webauthn/encoding'
export {
  isPasskeySupported,
  serializeAuthenticationCredential,
  serializeRegistrationCredential,
  toCredentialCreationOptions,
  toCredentialRequestOptions,
} from './webauthn/ceremony'

// Realtime
export { REALTIME_PATH, RealtimeClient } from './realtime/RealtimeClient'
export { RealtimeProvider, useRealtime } from './react/RealtimeProvider'
export { useChannel } from './react/useChannel'

// Misc
export { createAnubisApi } from './api/createAnubisApi'
export { createAnubisV1 } from './api/v1.generated'
export { getApiErrorMessage } from './api/errors'
export { AnubisProvider, useAnubisApi } from './react/AnubisProvider'
export { useCurrentUser } from './react/useCurrentUser'
export { useMemberships } from './react/useMemberships'
export { useOauthProviders } from './react/useOauthProviders'

/**
 * The version of the Anubis frontend package.
 *
 * Kept in lockstep with package.json; the unit test guards the pairing.
 */
export const ANUBIS_VERSION = '0.1.0'
