// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it, vi } from 'vitest'

// User interface
import { RichTextView } from './RichTextView'

// Utility
import { render } from '@testing-library/react'

/**
 * The sanitizer, stubbed, so this file tests the one thing that is ours.
 *
 * Whether DOMPurify strips a given payload is DOMPurify's own (very large)
 * test suite's job, and it cannot be re-proven here anyway: `happy-dom`'s
 * parser is different enough from a browser's that DOMPurify passes scripts
 * straight through under it, which would make an assertion about payloads
 * report on the test environment rather than on the library.
 *
 * What is ours, and what a regression would silently break, is the routing:
 * every byte this component writes into the DOM has to come back out of the
 * sanitizer. Stubbing it is the only way to state that as a test.
 */
vi.mock('dompurify', () => ({
  default: {
    isSupported: true,
    sanitize: (html: string) => `sanitized:${html.replace(/<[^>]*>/g, '')}`,
  },
}))

describe('RichTextView', () => {
  it('should render the sanitizer output, never its input', () => {
    const { container } = render(<RichTextView html='<p onclick="steal()">Hi</p>' />)

    expect(container.textContent).toBe('sanitized:Hi')
    expect(container.innerHTML).not.toContain('onclick')
  })

  it('should not reach the sanitizer for an empty value', () => {
    const { container } = render(<RichTextView html={null} />)

    expect(container.firstChild).toBeNull()
  })
})
