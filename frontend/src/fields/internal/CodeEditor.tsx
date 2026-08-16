// Copyright © 2026 Jalapeno Labs

import type { CodeLanguage } from '../types'
import type { Extension } from '@codemirror/state'

// Core
import { useEffect, useState } from 'react'

// User interface
import CodeMirror from '@uiw/react-codemirror'

/**
 * How each supported language's grammar is fetched.
 *
 * One dynamic import per entry, so a form that edits SQL downloads the SQL
 * grammar and not the JavaScript, HTML, CSS, and Markdown ones. The map is
 * `satisfies Record<CodeLanguage, ...>`, which is what makes a language added
 * to the public union a compile error here until it has a loader.
 */
const grammarLoaders = {
  css: async () => (await import('@codemirror/lang-css')).css(),
  html: async () => (await import('@codemirror/lang-html')).html(),
  javascript: async () => (await import('@codemirror/lang-javascript')).javascript(),
  json: async () => (await import('@codemirror/lang-json')).json(),
  jsx: async () => (await import('@codemirror/lang-javascript')).javascript({ jsx: true }),
  markdown: async () => (await import('@codemirror/lang-markdown')).markdown(),
  sql: async () => (await import('@codemirror/lang-sql')).sql(),
  tsx: async () => (await import('@codemirror/lang-javascript')).javascript({
    jsx: true,
    typescript: true,
  }),
  typescript: async () => (await import('@codemirror/lang-javascript')).javascript({
    typescript: true,
  }),
} as const satisfies Record<CodeLanguage, () => Promise<Extension>>

type Props = {
  id: string
  ariaLabel?: string
  value: string
  language?: CodeLanguage
  /** CodeMirror extensions for anything outside the supported languages. */
  extensions?: Extension[]
  height: string
  isInvalid: boolean
  isDisabled?: boolean
  isReadOnly?: boolean
  autoFocus?: boolean
  placeholder?: string
  onChange: (value: string) => void
  onBlur: () => void
}

/**
 * The CodeMirror editor behind `CodeEditorField`, loaded on demand.
 *
 * CodeMirror 6 rather than Bullet Train's Monaco, and the reasoning is size:
 * Monaco is an IDE that carries a worker-based TypeScript service and weighs
 * megabytes, which is a steep price for a form control that edits a template
 * or a snippet of configuration. CodeMirror 6 is tree-shakeable, ships its
 * grammars as separate packages, and works on touch devices, which Monaco
 * does not. An application that genuinely needs IntelliSense ejects this file.
 */
export function CodeEditor(props: Props) {
  const [ grammar, setGrammar ] = useState<Extension | null>(null)
  const language = props.language

  useEffect(() => {
    if (!language) {
      setGrammar(null)
      return undefined
    }

    // A language that resolves after the field has moved on (or unmounted)
    // must not write to state, so the effect's own cleanup disowns it.
    let isCurrent = true
    grammarLoaders[language]()
      .then((loaded) => {
        if (isCurrent) {
          setGrammar(loaded)
        }
      })
      .catch((error: unknown) => {
        console.debug('CodeEditor could not load the grammar for', language, error)
      })

    return () => {
      isCurrent = false
    }
  }, [ language ])

  const extensions = props.extensions ?? []
  const border = props.isInvalid
    ? 'border-danger'
    : 'border-default-200'

  return <div className={`overflow-hidden rounded-medium border ${border}`}>
    <CodeMirror
      id={props.id}
      aria-label={props.ariaLabel}
      value={props.value}
      height={props.height}
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      editable={!props.isDisabled}
      readOnly={props.isReadOnly}
      extensions={grammar ? [ grammar, ...extensions ] : extensions}
      onChange={props.onChange}
      onBlur={props.onBlur}
    />
  </div>
}
