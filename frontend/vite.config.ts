// Copyright © 2026 Jalapeno Labs

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import dts from 'vite-plugin-dts'

import { dependencies, peerDependencies } from './package.json'

/**
 * Every declared dependency stays out of the bundle.
 *
 * This package is a library, so its consumer's bundler resolves and splits
 * what it uses. That is also what makes the heavy editors behind `rich_text`
 * and `code_editor` free for everyone else: an external module reached by a
 * dynamic import survives into the output as a dynamic import, so the
 * application's own build gives it a chunk of its own rather than folding it
 * into the entry. Reading the list off the manifest keeps it from drifting.
 */
const bundledElsewhere = [
  ...Object.keys(dependencies),
  ...Object.keys(peerDependencies),
]

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
      external: (id) => bundledElsewhere.some(
        (name) => id === name || id.startsWith(`${name}/`),
      ),
    },
    sourcemap: true,
  },
})
