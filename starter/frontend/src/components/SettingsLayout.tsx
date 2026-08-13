// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'

// UI
import { Tab, Tabs } from '@heroui/react'
import { AppShell } from './AppShell'

// Misc
import { UrlTree } from '../urls'

/** The settings routes, which double as the tab keys. */
type SettingsTab = typeof UrlTree.settingsProfile | typeof UrlTree.settingsSecurity

type Props = {
  user: User
  /** The tab this page occupies, which is its own route. */
  activeTab: SettingsTab
  children: ReactNode
}

/**
 * The frame every settings page shares: the shell, the heading, and the tabs.
 *
 * The tabs navigate rather than swap panels, so each area keeps a URL a user
 * can bookmark and the browser's back button behaves.
 */
export function SettingsLayout(props: Props) {
  const { t } = useTranslation()
  const navigate = useNavigate()

  return <AppShell
    user={props.user}
    breadcrumbs={[{ label: t('settings.title') }]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('settings.title')
        }</h2>
      <p className='opacity-70'>{
          t('settings.subtitle')
        }</p>
    </div>
    <div className='relaxed'>
      <Tabs
        aria-label={t('settings.title')}
        selectedKey={props.activeTab}
        onSelectionChange={(key) => navigate(String(key))}
      >
        <Tab key={UrlTree.settingsProfile} title={t('settings.profile.tab')} />
        <Tab key={UrlTree.settingsSecurity} title={t('settings.security.tab')} />
      </Tabs>
    </div>
    {props.children}
  </AppShell>
}
