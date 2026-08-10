// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

// Core
import { useTranslation } from 'react-i18next'

// UI
import { Card, CardBody, CardHeader } from '@heroui/react'

type Props = {
  title: string
  subtitle: string
  children: ReactNode
}

/** Centered single-card layout shared by every auth page. */
export function AuthLayout(props: Props) {
  const { t } = useTranslation()

  return <main className='flex min-h-screen items-center justify-center p-6'>
    <div className='w-full max-w-md'>
      <div className='relaxed text-center'>
        <h1 className='title'>{
            t('app.title')
          }</h1>
      </div>
      <Card className='w-full p-2'>
        <CardHeader className='flex-col items-start'>
          <h2 className='text-xl font-semibold'>{
              props.title
            }</h2>
          <p className='opacity-70'>{
              props.subtitle
            }</p>
        </CardHeader>
        <CardBody>{
            props.children
          }</CardBody>
      </Card>
    </div>
  </main>
}
