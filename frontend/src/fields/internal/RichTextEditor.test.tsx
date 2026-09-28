// Copyright © 2026 Jalapeno Labs

import type { RichTextLabels } from '../types'

// Core
import { beforeEach, describe, expect, it, vi } from 'vitest'

// User interface
import { HeroUIProvider } from '@heroui/react'
import { RichTextEditor } from './RichTextEditor'

// Utility
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

type Context = {
  onChange: ReturnType<typeof vi.fn<(html: string) => void>>
}

const labels: RichTextLabels = {
  bold: 'Bold',
  italic: 'Italic',
  strike: 'Strike',
  heading: 'Heading',
  subheading: 'Subheading',
  bulletList: 'Bulleted list',
  orderedList: 'Numbered list',
  quote: 'Quote',
  code: 'Code block',
  undo: 'Undo',
  redo: 'Redo',
}

function renderEditor(html: string, onChange: (html: string) => void) {
  const view = (current: string) => <HeroUIProvider>
    <RichTextEditor
      id='body'
      ariaLabel='Body'
      html={current}
      labels={labels}
      isInvalid={false}
      onChange={onChange}
      onBlur={() => {}}
    />
  </HeroUIProvider>

  const result = render(view(html))
  return {
    rerender: (next: string) => result.rerender(view(next)),
  } as const
}

/**
 * The editor's contract with the form that owns its value, held across Tiptap
 * majors: stored markup loads untouched, the toolbar tracks the selection,
 * and only a person's edit reaches `onChange`.
 */
describe('RichTextEditor', () => {
  beforeEach<Context>((context) => {
    context.onChange = vi.fn<(html: string) => void>()
  })

  it<Context>('loads stored markup without appending to it or reporting a change', async (context) => {
    renderEditor('<ul><li><p>one</p></li></ul>', context.onChange)

    const textbox = await screen.findByRole('textbox', { name: 'Body' })

    // A trailing paragraph added on load would change the stored document the
    // moment a record is opened for editing.
    expect(textbox.lastElementChild?.tagName).toBe('UL')
    expect(context.onChange).not.toHaveBeenCalled()
  })

  it<Context>('presses a toolbar button when the selection enters its block', async (context) => {
    renderEditor('<p>one</p>', context.onChange)

    const bulletList = await screen.findByRole('button', { name: 'Bulleted list' })
    expect(bulletList.getAttribute('aria-pressed')).toBe('false')

    await userEvent.click(bulletList)

    await waitFor(() => {
      expect(bulletList.getAttribute('aria-pressed')).toBe('true')
    })
    expect(context.onChange).toHaveBeenLastCalledWith('<ul><li><p>one</p></li></ul>')
  })

  it<Context>('takes a value the form replaced without echoing it back', async (context) => {
    const { rerender } = renderEditor('<p>draft</p>', context.onChange)
    const textbox = await screen.findByRole('textbox', { name: 'Body' })

    rerender('<p>reset</p>')

    await waitFor(() => {
      expect(textbox.innerHTML).toBe('<p>reset</p>')
    })
    expect(context.onChange).not.toHaveBeenCalled()
  })
})
