// Copyright © 2026 Jalapeno Labs

// Core
import { useCurrentUser } from '@jalapenolabs/anubis'

// UI
import { AvatarCard } from '../../components/settings/AvatarCard'
import { ProfileForm } from '../../components/settings/ProfileForm'
import { SettingsLayout } from '../../components/SettingsLayout'

// Misc
import { UrlTree } from '../../urls'

/** Who the account belongs to: picture, name, time zone, locale. */
export function ProfileSettingsPage() {
  const { user } = useCurrentUser()

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  return <SettingsLayout
    user={user}
    activeTab={UrlTree.settingsProfile}
  >
    <AvatarCard user={user} />
    <ProfileForm user={user} />
  </SettingsLayout>
}
