// Copyright © 2026 Jalapeno Labs

import type { Breadcrumb } from './Breadcrumbs'
import type { User } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
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

// Misc
import { UrlTree } from '../urls'

type Props = {
  user: User
  /** The page's trail, rendered above the content. */
  breadcrumbs?: Breadcrumb[]
  children: ReactNode
}

/** Signed-in application frame: navbar, breadcrumbs, and the page content. */
export function AppShell(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const navigate = useNavigate()
  const { refresh } = useCurrentUser()
  const { memberships, current, selectTeam } = useTeamContext()

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
            onAction={(key) => selectTeam(String(key))}
          >
            {
              memberships.organizations.map((organization) => (
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
              ))
            }
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
        <NavbarItem>
          <Link to={UrlTree.members} className='opacity-80 hover:opacity-100'>{
              t('team.members.navLink')
            }</Link>
        </NavbarItem>
        <NavbarItem>
          <Dropdown placement='bottom-end'>
            <DropdownTrigger>
              <Avatar
                as='button'
                size='sm'
                showFallback
                src={api.avatarUrl(props.user.id)}
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
  </div>
}
