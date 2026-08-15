// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

// Core
import { Navigate, Route, Routes, useLocation, useSearchParams } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { ClaimInvitationPage } from './pages/ClaimInvitationPage'
import { CreativeConceptPage } from './pages/CreativeConceptPage'
import { CreativeConceptsPage } from './pages/CreativeConceptsPage'
import { DashboardPage } from './pages/DashboardPage'
import { BillingPage } from './pages/billing/BillingPage'
import { DevelopersPage } from './pages/developers/DevelopersPage'
import { OrganizationSettingsPage } from './pages/tenancy/OrganizationSettingsPage'
import { TeamSettingsPage } from './pages/tenancy/TeamSettingsPage'
import { ProfileSettingsPage } from './pages/settings/ProfileSettingsPage'
import { SecuritySettingsPage } from './pages/settings/SecuritySettingsPage'
import { ConfirmEmailChangePage } from './pages/auth/ConfirmEmailChangePage'
import { ForcePasswordChangePage } from './pages/auth/ForcePasswordChangePage'
import { ForgotPasswordPage } from './pages/auth/ForgotPasswordPage'
import { ResetPasswordPage } from './pages/auth/ResetPasswordPage'
import { SignInPage } from './pages/auth/SignInPage'
import { SignUpPage } from './pages/auth/SignUpPage'
import { VerifyEmailPage } from './pages/auth/VerifyEmailPage'
// 🐺 anubis:page-imports

// Misc
import { TeamProvider } from './context/TeamProvider'

// Misc
import {
  DESTINATION_PARAM,
  POST_SIGN_IN_REDIRECT_TO,
  UNKNOWN_ROUTE_REDIRECT_TO,
  UrlTree,
  getUrlWithDestination,
  sanitizeDestination,
} from './urls'

type GateProps = {
  children: ReactNode
}

function CenteredSpinner() {
  return <div className='flex min-h-screen items-center justify-center'>
    <Spinner size='lg' />
  </div>
}

/**
 * Renders children only when signed in; otherwise redirects to sign-in,
 * carrying the page the user asked for so they land there afterwards.
 *
 * An account owing a password change is signed in and can reach nothing else,
 * so it goes to the forced-change screen instead of to sign-in.
 */
function RequireAuth(props: GateProps) {
  const { user, passwordChangeRequired, isLoading } = useCurrentUser()
  const location = useLocation()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (passwordChangeRequired) {
    return <Navigate to={UrlTree.forcedPasswordChange} replace />
  }

  if (!user) {
    const attempted = `${location.pathname}${location.search}${location.hash}`
    return <Navigate to={getUrlWithDestination(UrlTree.signIn, attempted)} replace />
  }

  return props.children
}

/**
 * Renders children only when signed out; the signed-in land on the destination
 * the guard preserved, or on the dashboard when there is none.
 *
 * Signing in again would land an account owing a password change right back
 * here, so it is sent to the screen that ends the loop.
 */
function RequireGuest(props: GateProps) {
  const { user, passwordChangeRequired, isLoading } = useCurrentUser()
  const [ searchParams ] = useSearchParams()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (passwordChangeRequired) {
    return <Navigate to={UrlTree.forcedPasswordChange} replace />
  }

  if (user) {
    const destination = sanitizeDestination(searchParams.get(DESTINATION_PARAM))
    return <Navigate to={destination ?? POST_SIGN_IN_REDIRECT_TO} replace />
  }

  return props.children
}

/**
 * Renders the forced password change only while the backend demands one.
 *
 * The screen is not a page a person navigates to: reaching it with nothing
 * owed means the change already landed, or the visitor is signed out.
 */
function RequirePasswordChange(props: GateProps) {
  const { user, passwordChangeRequired, isLoading } = useCurrentUser()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (passwordChangeRequired) {
    return props.children
  }

  if (user) {
    return <Navigate to={POST_SIGN_IN_REDIRECT_TO} replace />
  }

  return <Navigate to={UrlTree.signIn} replace />
}

/** Signed-in pages get the team context on top of the auth gate. */
function Workspace(props: GateProps) {
  return <RequireAuth>
    <TeamProvider>{
        props.children
      }</TeamProvider>
  </RequireAuth>
}

export function App() {
  return <Routes>
    <Route
      path={UrlTree.root}
      element={
        <Workspace>
          <DashboardPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.teamSettings}
      element={
        <Workspace>
          <TeamSettingsPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.teamDevelopers}
      element={
        <Workspace>
          <DevelopersPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.organizationSettings}
      element={
        <Workspace>
          <OrganizationSettingsPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.organizationBilling}
      element={
        <Workspace>
          <BillingPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.creativeConcepts}
      element={
        <Workspace>
          <CreativeConceptsPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.creativeConcept}
      element={
        <Workspace>
          <CreativeConceptPage />
        </Workspace>
      }
    />
    {/* 🐺 anubis:routes */}
    <Route
      path={UrlTree.settings}
      element={<Navigate to={UrlTree.settingsProfile} replace />}
    />
    <Route
      path={UrlTree.settingsProfile}
      element={
        <Workspace>
          <ProfileSettingsPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.settingsSecurity}
      element={
        <Workspace>
          <SecuritySettingsPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.claimInvitation}
      element={
        <RequireAuth>
          <ClaimInvitationPage />
        </RequireAuth>
      }
    />
    <Route
      path={UrlTree.signIn}
      element={
        <RequireGuest>
          <SignInPage />
        </RequireGuest>
      }
    />
    <Route
      path={UrlTree.signUp}
      element={
        <RequireGuest>
          <SignUpPage />
        </RequireGuest>
      }
    />
    <Route
      path={UrlTree.forgotPassword}
      element={
        <RequireGuest>
          <ForgotPasswordPage />
        </RequireGuest>
      }
    />
    <Route
      path={UrlTree.resetPassword}
      element={<ResetPasswordPage />}
    />
    <Route
      path={UrlTree.forcedPasswordChange}
      element={
        <RequirePasswordChange>
          <ForcePasswordChangePage />
        </RequirePasswordChange>
      }
    />
    <Route
      path={UrlTree.verifyEmail}
      element={<VerifyEmailPage />}
    />
    <Route
      path={UrlTree.confirmEmailChange}
      element={<ConfirmEmailChangePage />}
    />
    <Route
      path='*'
      element={<Navigate to={UNKNOWN_ROUTE_REDIRECT_TO} replace />}
    />
  </Routes>
}
