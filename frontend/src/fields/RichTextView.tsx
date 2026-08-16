// Copyright © 2026 Jalapeno Labs

// Core
import { useMemo } from 'react'

// Utility
import DOMPurify from 'dompurify'

// Misc
import { RICH_TEXT_PROSE } from './internal/richText'

type Props = {
  /** The stored markup. `null` renders nothing. */
  html: string | null | undefined
  className?: string
}

/**
 * The display half of `rich_text`: stored markup, sanitized, on the page.
 *
 * `RichTextField` produces HTML that one user wrote and another user reads,
 * which makes rendering it the one genuinely dangerous thing a generated show
 * page does. This component is the answer, and the reason the framework ships
 * one at all: a show page that reaches for `dangerouslySetInnerHTML` is an
 * XSS hole the first time a team member is not trusted, and a show page that
 * renders the markup as text shows the tags. DOMPurify drops scripts, event
 * handlers, and `javascript:` URLs, and keeps the formatting the editor
 * writes.
 *
 * Sanitizing here rather than at the API boundary is defense in depth, not the
 * whole defense: a field written through `/api/v1` by a bearer token never
 * passes through this package at all, so an application that accepts rich text
 * over its API should sanitize on write too.
 */
export function RichTextView(props: Props) {
  const sanitized = useMemo(
    () => {
      if (!props.html) {
        return ''
      }
      // DOMPurify returns its input unchanged on a platform it cannot secure,
      // which for this component would be the difference between rendering
      // markup and running someone else's script. Refusing to render is the
      // only safe reading of that, and it is loud rather than silent.
      if (!DOMPurify.isSupported) {
        console.warn('RichTextView will not render: DOMPurify cannot sanitize on this platform')
        return ''
      }
      return DOMPurify.sanitize(props.html)
    },
    [ props.html ],
  )

  if (!sanitized) {
    return null
  }

  const className = props.className
    ? `${RICH_TEXT_PROSE} ${props.className}`
    : RICH_TEXT_PROSE

  return <div
    className={className}
    // Sanitized one statement above, which is the only condition under which
    // this API is safe, and the only reason this component exists.
    dangerouslySetInnerHTML={{ __html: sanitized }}
  />
}
