// Copyright © 2026 Jalapeno Labs

import type { Breadcrumb } from './Breadcrumbs'
import type { User } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../context/TeamProvider'

// UI
import {
  Avatar,
  Button,
  Dropdown,
  DropdownItem,
  DropdownMenu,
  DropdownSection,
  DropdownTrigger,
  Navbar,
  NavbarBrand,
  NavbarContent,
  NavbarItem,
} from '@heroui/react'
import { Breadcrumbs } from './Breadcrumbs'
import { CreateOrganizationModal } from './tenancy/CreateOrganizationModal'

// Misc
import {
  UrlTree,
  getOrganizationBillingUrl,
  getOrganizationSettingsUrl,
  getTeamDevelopersUrl,
  getTeamSettingsUrl,
} from '../urls'

type Props = {
  user: User
  /** The page's trail, rendered above the content. */
  breadcrumbs?: Breadcrumb[]
  children: ReactNode
}

/**
 * The switcher's non-team entries.
 *
 * Every other key in the menu is a team id, so these are namespaced to keep the
 * two apart: a key that is not one of these is a team to switch to.
 */
const TENANCY_ACTIONS = {
  teamSettings: 'tenancy:team-settings',
  organizationSettings: 'tenancy:organization-settings',
  organizationBilling: 'tenancy:organization-billing',
  newOrganization: 'tenancy:new-organization',
} as const

/** Signed-in application frame: navbar, breadcrumbs, and the page content. */
export function AppShell(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const { refresh } = useCurrentUser()
  const { memberships, current, selectTeam } = useTeamContext()
  const [ isCreatingOrganization, setIsCreatingOrganization ] = useState(false)

  function onSwitcherAction(key: string | number) {
    const action = String(key)

    if (action === TENANCY_ACTIONS.newOrganization) {
      setIsCreatingOrganization(true)
      return
    }

    if (action === TENANCY_ACTIONS.teamSettings) {
      if (!current) {
        console.debug('team settings chosen with no team selected')
        return
      }
      navigate(getTeamSettingsUrl(current.team.id))
      return
    }

    if (action === TENANCY_ACTIONS.organizationSettings) {
      if (!current) {
        console.debug('organization settings chosen with no team selected')
        return
      }
      navigate(getOrganizationSettingsUrl(current.organization.id))
      return
    }

    if (action === TENANCY_ACTIONS.organizationBilling) {
      if (!current) {
        console.debug('billing chosen with no team selected')
        return
      }
      navigate(getOrganizationBillingUrl(current.organization.id))
      return
    }

    selectTeam(action)
  }

  async function onSignOut() {
    try {
      await api.logout()
    }
    catch (error) {
      console.debug('logout request failed; refreshing session state anyway', error)
    }
    await refresh()
  }

  return <div className='min-h-screen'>
    <Navbar isBordered maxWidth='xl'>
      <NavbarBrand className='gap-4'>
        <Link to={UrlTree.root} className='text-lg font-bold'>{
            t('app.title')
          }</Link>
        <Dropdown placement='bottom-start'>
          <DropdownTrigger>
            <Button size='sm' variant='flat'>
              <span>{
                  current
                    ? current.team.name
                    : t('team.switcher.noTeams')
                }</span>
            </Button>
          </DropdownTrigger>
          <DropdownMenu
            aria-label={t('team.switcher.ariaLabel')}
            selectionMode='single'
            selectedKeys={current ? [ current.team.id ] : []}
            disabledKeys={
              current
                ? []
                : [
                    TENANCY_ACTIONS.teamSettings,
                    TENANCY_ACTIONS.organizationSettings,
                    TENANCY_ACTIONS.organizationBilling,
                  ]
            }
            onAction={onSwitcherAction}
          >
            {[
              ...memberships.organizations.map((organization) => (
                <DropdownSection
                  key={organization.id}
                  title={organization.name}
                  showDivider
                >
                  {
                    organization.teams.map((team) => (
                      <DropdownItem key={team.id}>{
                          team.name
                        }</DropdownItem>
                    ))
                  }
                </DropdownSection>
              )),
              <DropdownSection
                key='tenancy-actions'
                title={t('team.switcher.manage')}
              >
                <DropdownItem key={TENANCY_ACTIONS.teamSettings}>{
                    t('team.settings.navLink')
                  }</DropdownItem>
                <DropdownItem key={TENANCY_ACTIONS.organizationSettings}>{
                    t('organization.settings.navLink')
                  }</DropdownItem>
                <DropdownItem key={TENANCY_ACTIONS.organizationBilling}>{
                    t('billing.navLink')
                  }</DropdownItem>
                <DropdownItem key={TENANCY_ACTIONS.newOrganization}>{
                    t('organization.create.action')
                  }</DropdownItem>
              </DropdownSection>,
            ]}
          </DropdownMenu>
        </Dropdown>
      </NavbarBrand>
      <NavbarContent justify='end'>
        <NavbarItem>
          <Link to={UrlTree.creativeConcepts} className='opacity-80 hover:opacity-100'>{
              t('creativeConcepts.navLink')
            }</Link>
        </NavbarItem>
        {/* 🐺 anubis:nav */}
        { current
          ? <NavbarItem>
              <Link
                to={getTeamDevelopersUrl(current.team.id)}
                className='opacity-80 hover:opacity-100'
              >{
                  t('developers.navLink')
                }</Link>
            </NavbarItem>
          : null
        }
        { current
          ? <NavbarItem>
              <Link
                to={getTeamSettingsUrl(current.team.id)}
                className='opacity-80 hover:opacity-100'
              >{
                  t('team.settings.navLink')
                }</Link>
            </NavbarItem>
          : null
        }
        <NavbarItem>
          <Dropdown placement='bottom-end'>
            <DropdownTrigger>
              <Avatar
                as='button'
                size='sm'
                showFallback
                // The trigger shows a picture or two initials, so it needs a
                // name of its own: without one, the only control that reaches
                // settings and sign-out is unaddressable to a screen reader.
                aria-label={t('common.accountMenu')}
                src={api.avatarUrl(props.user)}
                name={props.user.email.slice(0, 2).toUpperCase()}
                className='transition-transform'
              />
            </DropdownTrigger>
            <DropdownMenu aria-label={t('dashboard.greeting', { email: props.user.email })}>
              <DropdownItem key='signed-in-as' isReadOnly className='opacity-70'>{
                  t('dashboard.greeting', { email: props.user.email })
                }</DropdownItem>
              <DropdownItem
                key='settings'
                onPress={() => navigate(UrlTree.settingsProfile)}
              >{
                  t('settings.navLink')
                }</DropdownItem>
              <DropdownItem key='sign-out' color='danger' onPress={onSignOut}>{
                  t('common.signOut')
                }</DropdownItem>
            </DropdownMenu>
          </Dropdown>
        </NavbarItem>
      </NavbarContent>
    </Navbar>
    <main className='container mx-auto max-w-5xl p-6'>
      { props.breadcrumbs?.length
        ? <div className='relaxed'>
            <Breadcrumbs items={props.breadcrumbs} />
          </div>
        : null
      }
      {props.children}
    </main>
    <CreateOrganizationModal
      isOpen={isCreatingOrganization}
      onClose={() => setIsCreatingOrganization(false)}
    />
  </div>
}
