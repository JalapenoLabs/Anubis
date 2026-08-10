// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'

// Misc
import { ANUBIS_VERSION } from '@jalapenolabs/anubis'

export function App() {
  const { t } = useTranslation()

  return <main className='container'>
    <h1 className='title'>{
        t('app.title')
      }</h1>
    <p>{
        t('app.tagline')
      }</p>
    <p className='opacity-80'>{
        t('app.frameworkVersion', { version: ANUBIS_VERSION })
      }</p>
  </main>
}
