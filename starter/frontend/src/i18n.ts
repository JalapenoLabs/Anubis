// Copyright © 2026 Jalapeno Labs

// Core
import i18next from 'i18next'
import { initReactI18next } from 'react-i18next'

// Misc
import enUS from './locales/en-US.json'

export const i18n = i18next.use(initReactI18next)

i18n.init({
  lng: 'en-US',
  fallbackLng: 'en-US',
  resources: {
    'en-US': {
      translation: enUS,
    },
  },
  interpolation: {
    // React already escapes rendered values.
    escapeValue: false,
  },
})
