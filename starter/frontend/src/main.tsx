// Copyright © 2026 Jalapeno Labs

// Core
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router'

// UI
import { HeroUIProvider } from '@heroui/react'
import { App } from './App'

// Misc
import { AnubisProvider, RealtimeClient, RealtimeProvider } from '@jalapenolabs/anubis'
import { api } from './api'
import './i18n'
import './styles.css'

// One connection for the whole application, opened lazily: nothing connects
// until a component subscribes, so a signed-out page costs no socket. The
// notification bell is the first subscriber the starter ships.
const realtime = new RealtimeClient()

const rootElement = document.getElementById('root')
if (!rootElement) {
  throw new Error('Root element #root is missing from index.html')
}

createRoot(rootElement).render(
  <StrictMode>
    <BrowserRouter>
      <HeroUIProvider>
        <AnubisProvider api={api}>
          <RealtimeProvider client={realtime}>
            <App />
          </RealtimeProvider>
        </AnubisProvider>
      </HeroUIProvider>
    </BrowserRouter>
  </StrictMode>,
)
