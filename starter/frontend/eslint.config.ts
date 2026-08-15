// Copyright © 2026 Jalapeno Labs

import { defineConfig, globalIgnores } from 'eslint/config'
import cliBaseConfig from '@jalapenolabs/cli/eslint'

export default defineConfig([
  {
    extends: [ cliBaseConfig ],
  },
  globalIgnores([
    '.yarn/**',
    'dist/**',
    'coverage/**',
    '**/*.d.ts',
  ]),
  {
    files: [ '**/*.{ts,tsx}' ],
    rules: {
      'license-header/header': [
        'error',
        [
          `// Copyright © ${new Date().getFullYear()} Jalapeno Labs`,
        ],
      ],
    },
  },
  // Tooling configs (`export default defineConfig(...)`) legitimately rely on
  // default exports, Tailwind's `@plugin` directive requires one from hero.ts,
  // and Playwright loads its global setup by default export. The repo bans
  // default exports everywhere else, so the exception is scoped tightly.
  {
    files: [
      '*.config.ts',
      'eslint.config.ts',
      'src/hero.ts',
      'e2e/global-setup.ts',
    ],
    rules: {
      'import/no-default-export': 'off',
      'no-restricted-exports': 'off',
    },
  },
])
