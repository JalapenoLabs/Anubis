// Copyright © 2026 Jalapeno Labs

// Core
import i18next from 'i18next'
import { initReactI18next } from 'react-i18next'

// Misc
import enUS from './locales/en-US.json'
import creativeConceptsEnUS from './locales/models/creativeConcepts.en-US.json'
import tangibleThingsEnUS from './locales/models/tangibleThings.en-US.json'
import granularDetailsEnUS from './locales/models/granularDetails.en-US.json'
// 🐺 anubis:locale-imports

export const i18n = i18next.use(initReactI18next)

// JSON carries no comments, so a scaffolded model gets its own locale file
// under `locales/models/` and this TypeScript module carries the anchors.
i18n.init({
  lng: 'en-US',
  fallbackLng: 'en-US',
  resources: {
    'en-US': {
      translation: {
        ...enUS,
        ...creativeConceptsEnUS,
        ...tangibleThingsEnUS,
        ...granularDetailsEnUS,
        // 🐺 anubis:locales
      },
    },
  },
  interpolation: {
    // React already escapes rendered values.
    escapeValue: false,
  },
})
