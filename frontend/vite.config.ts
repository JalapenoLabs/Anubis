// Copyright © 2026 Jalapeno Labs

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import dts from 'vite-plugin-dts'

export default defineConfig({
  plugins: [
    react(),
    dts({
      rollupTypes: true,
      tsconfigPath: './tsconfig.json',
    }),
  ],
  build: {
    lib: {
      entry: 'src/index.ts',
      name: 'Anubis',
      formats: [ 'es', 'cjs' ],
      fileName: (format) => {
        if (format === 'cjs') {
          return 'index.cjs'
        }
        return 'index.js'
      },
    },
    rollupOptions: {
      external: [
        'react',
        'react-dom',
        'react/jsx-runtime',
        'ky',
        'swr',
      ],
    },
    sourcemap: true,
  },
})
