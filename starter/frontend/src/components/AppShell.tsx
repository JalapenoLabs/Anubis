// Copyright © 2026 Jalapeno Labs

import type { User } from '@jalapenolabs/anubis'
import type { ReactNode } from 'react'

// Core
import { useTranslation } from 'react-i18next'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'

// UI
import {
  Avatar,
  Dropdown,
  DropdownItem,
  DropdownMenu,
  DropdownTrigger,
  Navbar,
  NavbarBrand,
  NavbarContent,
  NavbarItem,
} from '@heroui/react'

type Props = {
  user: User
  children: ReactNode
}

/** Signed-in application frame: top navbar with the user menu. */
export function AppShell(props: Props) {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { refresh } = useCurrentUser()

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
      <NavbarBrand>
        <span className='text-lg font-bold'>{
            t('app.title')
          }</span>
      </NavbarBrand>
      <NavbarContent justify='end'>
        <NavbarItem>
          <Dropdown placement='bottom-end'>
            <DropdownTrigger>
              <Avatar
                as='button'
                size='sm'
                name={props.user.email.slice(0, 2).toUpperCase()}
                className='transition-transform'
              />
            </DropdownTrigger>
            <DropdownMenu aria-label={t('dashboard.greeting', { email: props.user.email })}>
              <DropdownItem key='signed-in-as' isReadOnly className='opacity-70'>{
                  t('dashboard.greeting', { email: props.user.email })
                }</DropdownItem>
              <DropdownItem key='sign-out' color='danger' onPress={onSignOut}>{
                  t('common.signOut')
                }</DropdownItem>
            </DropdownMenu>
          </Dropdown>
        </NavbarItem>
      </NavbarContent>
    </Navbar>
    <main className='container mx-auto max-w-5xl p-6'>{
        props.children
      }</main>
  </div>
}
