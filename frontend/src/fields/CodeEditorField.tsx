// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, CodeLanguage } from './types'
import type { Extension } from '@codemirror/state'
import type { FieldValues } from 'react-hook-form'

// Core
import { Suspense, lazy } from 'react'

// User interface
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * The editor, fetched the first time a form renders one.
 *
 * CodeMirror is behind a dynamic import for the same reason the rich text
 * editor is: an application that never scaffolds a `code_editor` field never
 * downloads it. The grammar for the chosen language is a second dynamic
 * import inside, so the cost is the editor plus one language, never all of
 * them.
 */
const CodeEditor = lazy(async () => {
  const editor = await import('./internal/CodeEditor')
  return { default: editor.CodeEditor }
})

/** Tall enough for a snippet, short enough to sit in a form. */
const DEFAULT_HEIGHT = '16rem'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /** Which grammar to highlight with. Plain text when it is left out. */
  language?: CodeLanguage
  /** CodeMirror extensions, for a language or a behavior outside the list. */
  extensions?: Extension[]
  /** Any CSS length. Defaults to `16rem`. */
  height?: string
}

/**
 * The `code_editor` scaffolder type: Bullet Train's Monaco field, on
 * CodeMirror 6.
 *
 * The form value is the source as plain text, so the column is an ordinary
 * `TEXT` and a show page renders it with no sanitizing and no ceremony: code
 * displayed as text is exactly right, which is the whole difference between
 * this field and `rich_text`.
 *
 * `language` picks a grammar from the set the framework depends on; anything
 * else is reached by passing CodeMirror `extensions` directly, which is also
 * how an application adds linting, folding, or a theme.
 */
export function CodeEditorField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)
  const height = props.height ?? DEFAULT_HEIGHT

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Suspense fallback={
      <div
        className='w-full animate-pulse rounded-medium bg-default-100'
        style={{ height }}
      />
    }>
      <CodeEditor
        id={fieldId}
        ariaLabel={props.label}
        value={field.value == null ? '' : String(field.value)}
        language={props.language}
        extensions={props.extensions}
        height={height}
        placeholder={props.placeholder}
        isInvalid={isInvalid}
        isDisabled={props.isDisabled}
        isReadOnly={props.isReadOnly}
        autoFocus={props.autoFocus}
        onChange={field.onChange}
        onBlur={field.onBlur}
      />
    </Suspense>
  </FieldWrapper>
}
