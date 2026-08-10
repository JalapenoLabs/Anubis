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
  // default exports. The repo bans default exports everywhere else, so the
  // exception is scoped tightly.
  {
    files: [
      '*.config.ts',
      'eslint.config.ts',
    ],
    rules: {
      'import/no-default-export': 'off',
      'no-restricted-exports': 'off',
    },
  },
])
