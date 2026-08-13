// Copyright © 2026 Jalapeno Labs

// Core
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { ChangeEmailCard } from '../../components/settings/ChangeEmailCard'
import { ChangePasswordCard } from '../../components/settings/ChangePasswordCard'
import { DangerZoneCard } from '../../components/settings/DangerZoneCard'
import { PasskeysCard } from '../../components/settings/PasskeysCard'
import { SessionsCard } from '../../components/settings/SessionsCard'
import { SettingsLayout } from '../../components/SettingsLayout'
import { TwoFactorCard } from '../../components/settings/TwoFactorCard'

// Misc
import { UrlTree } from '../../urls'

/** How the account is proven: password, address, second factor, passkeys, sessions. */
export function SecuritySettingsPage() {
  const { user } = useCurrentUser()

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  return <SettingsLayout
    user={user}
    activeTab={UrlTree.settingsSecurity}
  >
    <ChangePasswordCard />
    <ChangeEmailCard user={user} />
    <TwoFactorCard />
    <PasskeysCard />
    <SessionsCard />
    <DangerZoneCard user={user} />
  </SettingsLayout>
}
