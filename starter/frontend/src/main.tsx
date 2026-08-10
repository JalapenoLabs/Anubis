// Copyright © 2026 Jalapeno Labs

// Core
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router'

// UI
import { HeroUIProvider } from '@heroui/react'
import { App } from './App'

// Misc
import { AnubisProvider } from '@jalapenolabs/anubis'
import { api } from './api'
import './i18n'
import './styles.css'

const rootElement = document.getElementById('root')
if (!rootElement) {
  throw new Error('Root element #root is missing from index.html')
}

createRoot(rootElement).render(
  <StrictMode>
    <BrowserRouter>
      <HeroUIProvider>
        <AnubisProvider api={api}>
          <App />
        </AnubisProvider>
      </HeroUIProvider>
    </BrowserRouter>
  </StrictMode>,
)
