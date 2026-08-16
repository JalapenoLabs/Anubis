// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps, RichTextLabels } from './types'
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
 * Tiptap and its ProseMirror runtime are the heaviest thing this package can
 * put on a screen, so they are reached through a dynamic import rather than a
 * static one: an application that never scaffolds a `rich_text` field never
 * downloads them, and one that does downloads them when the form opens rather
 * than when the application boots. `React.lazy` wants a default export and the
 * package exports names, which is what the mapping below is for.
 */
const RichTextEditor = lazy(async () => {
  const editor = await import('./internal/RichTextEditor')
  return { default: editor.RichTextEditor }
})

const DEFAULT_LABELS: RichTextLabels = {
  bold: 'Bold',
  italic: 'Italic',
  strike: 'Strikethrough',
  heading: 'Heading',
  subheading: 'Subheading',
  bulletList: 'Bulleted list',
  orderedList: 'Numbered list',
  quote: 'Quote',
  code: 'Code block',
  undo: 'Undo',
  redo: 'Redo',
}

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  /** Translated toolbar words. Anything left out keeps the English default. */
  labels?: Partial<RichTextLabels>
}

/**
 * The `rich_text` scaffolder type: Bullet Train's `trix_editor`, on Tiptap.
 *
 * The form value is an **HTML string**, and `''` while the document is empty.
 * HTML is the interoperable choice: an email body, an export, a webhook
 * payload, and an `/api/v1` response all carry markup without first agreeing
 * on an editor's own document model, and it is what Trix stores, so a Bullet
 * Train application's data moves across unchanged. Tiptap can serialize its
 * ProseMirror JSON instead, and an application that wants that ejects this
 * component and changes one call.
 *
 * ## Rendering the value is the consumer's problem, and it is a real one
 *
 * Stored HTML written by one user and shown to another is a cross-site
 * scripting hole unless something sanitizes it. Nothing in this component
 * does: it produces markup, it does not display it. Render a stored value
 * with `RichTextView`, which sanitizes before it writes to the DOM, and never
 * with a bare `dangerouslySetInnerHTML`.
 */
export function RichTextField<Values extends FieldValues>(props: Props<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Suspense fallback={
      <div className='min-h-32 w-full animate-pulse rounded-medium bg-default-100' />
    }>
      <RichTextEditor
        id={fieldId}
        ariaLabel={props.label}
        html={field.value == null ? '' : String(field.value)}
        labels={{
          ...DEFAULT_LABELS,
          ...props.labels,
        }}
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
