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
import { TangibleThingPage } from './pages/TangibleThingPage'
import { GranularDetailPage } from './pages/GranularDetailPage'
import { DashboardPage } from './pages/DashboardPage'
import { BillingPage } from './pages/billing/BillingPage'
import { DevelopersPage } from './pages/developers/DevelopersPage'
import { AuditLogPage } from './pages/auditLog/AuditLogPage'
import { OrganizationSettingsPage } from './pages/tenancy/OrganizationSettingsPage'
import { TeamSettingsPage } from './pages/tenancy/TeamSettingsPage'
import { ProfileSettingsPage } from './pages/settings/ProfileSettingsPage'
import { SecuritySettingsPage } from './pages/settings/SecuritySettingsPage'
import { ConfirmEmailChangePage } from './pages/auth/ConfirmEmailChangePage'
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
 */
function RequireAuth(props: GateProps) {
  const { user, isLoading } = useCurrentUser()
  const location = useLocation()

  if (isLoading) {
    return <CenteredSpinner />
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
 */
function RequireGuest(props: GateProps) {
  const { user, isLoading } = useCurrentUser()
  const [ searchParams ] = useSearchParams()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (user) {
    const destination = sanitizeDestination(searchParams.get(DESTINATION_PARAM))
    return <Navigate to={destination ?? POST_SIGN_IN_REDIRECT_TO} replace />
  }

  return props.children
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
      path={UrlTree.teamAuditLog}
      element={
        <Workspace>
          <AuditLogPage />
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
    <Route
      path={UrlTree.tangibleThing}
      element={
        <Workspace>
          <TangibleThingPage />
        </Workspace>
      }
    />
    <Route
      path={UrlTree.granularDetail}
      element={
        <Workspace>
          <GranularDetailPage />
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
