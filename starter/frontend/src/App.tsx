// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

// Core
import { Navigate, Route, Routes } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { ClaimInvitationPage } from './pages/ClaimInvitationPage'
import { DashboardPage } from './pages/DashboardPage'
import { MembersPage } from './pages/MembersPage'
import { ForgotPasswordPage } from './pages/auth/ForgotPasswordPage'
import { ResetPasswordPage } from './pages/auth/ResetPasswordPage'
import { SignInPage } from './pages/auth/SignInPage'
import { SignUpPage } from './pages/auth/SignUpPage'
import { VerifyEmailPage } from './pages/auth/VerifyEmailPage'

// Misc
import { TeamProvider } from './context/TeamProvider'

// Misc
import { UNKNOWN_ROUTE_REDIRECT_TO, UrlTree } from './urls'

type GateProps = {
  children: ReactNode
}

function CenteredSpinner() {
  return <div className='flex min-h-screen items-center justify-center'>
    <Spinner size='lg' />
  </div>
}

/** Renders children only when signed in; otherwise redirects to sign-in. */
function RequireAuth(props: GateProps) {
  const { user, isLoading } = useCurrentUser()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (!user) {
    return <Navigate to={UrlTree.signIn} replace />
  }

  return props.children
}

/** Renders children only when signed out; the signed-in land on the dashboard. */
function RequireGuest(props: GateProps) {
  const { user, isLoading } = useCurrentUser()

  if (isLoading) {
    return <CenteredSpinner />
  }

  if (user) {
    return <Navigate to={UrlTree.root} replace />
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
      path={UrlTree.members}
      element={
        <Workspace>
          <MembersPage />
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
      path='*'
      element={<Navigate to={UNKNOWN_ROUTE_REDIRECT_TO} replace />}
    />
  </Routes>
}
