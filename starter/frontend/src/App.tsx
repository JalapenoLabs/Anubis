// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

// Core
import { Navigate, Route, Routes } from 'react-router'
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { Spinner } from '@heroui/react'
import { DashboardPage } from './pages/DashboardPage'
import { ForgotPasswordPage } from './pages/auth/ForgotPasswordPage'
import { ResetPasswordPage } from './pages/auth/ResetPasswordPage'
import { SignInPage } from './pages/auth/SignInPage'
import { SignUpPage } from './pages/auth/SignUpPage'
import { VerifyEmailPage } from './pages/auth/VerifyEmailPage'

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

export function App() {
  return <Routes>
    <Route
      path={UrlTree.root}
      element={
        <RequireAuth>
          <DashboardPage />
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
