// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// Core
import { useState } from 'react'

// User interface
import { Tooltip } from '@heroui/react'
import { TextualField } from './internal/TextualField'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  autoComplete?: string
  /** Translated words for the reveal toggle. */
  showLabel?: string
  hideLabel?: string
}

/**
 * The `password_field` scaffolder type: a masked secret, revealable.
 *
 * Pass `autoComplete` to tell the browser which secret this is, since only the
 * form knows whether it is a sign-in, a new password, or a one-off token.
 *
 * The reveal toggle is the difference between a user who mistypes a new
 * password twice and one who does not, so it is on by default. Its two glyphs
 * are inline SVG rather than an icon package: a framework that pulls a whole
 * icon set into every application's dependency tree for two paths has made a
 * bad trade, and an application that already has one restyles this file
 * through `anubis eject PasswordField`.
 */
export function PasswordField<Values extends FieldValues>(props: Props<Values>) {
  const [ isRevealed, setIsRevealed ] = useState(false)
  const toggleLabel = isRevealed
    ? (props.hideLabel ?? 'Hide password')
    : (props.showLabel ?? 'Show password')

  return <TextualField
    {...props}
    type={isRevealed ? 'text' : 'password'}
    endContent={
      <Tooltip content={toggleLabel}>
        <button
          type='button'
          aria-label={toggleLabel}
          aria-pressed={isRevealed}
          className='text-default-500 outline-none focus-visible:text-foreground'
          disabled={props.isDisabled}
          onClick={() => setIsRevealed((revealed) => !revealed)}
        >{
          isRevealed ? <EyeOffGlyph /> : <EyeGlyph />
        }</button>
      </Tooltip>
    }
  />
}

function EyeGlyph() {
  return <svg
    aria-hidden='true'
    viewBox='0 0 24 24'
    fill='none'
    stroke='currentColor'
    strokeWidth={1.5}
    className='h-5 w-5'
  >
    <path
      strokeLinecap='round'
      strokeLinejoin='round'
      d='M2.036 12.322a1.012 1.012 0 0 1 0-.639C3.423 7.51 7.36 4.5 12 4.5c4.638 0 8.573 3.007
        9.963 7.178.07.207.07.431 0 .639C20.577 16.49 16.64 19.5 12 19.5c-4.638
        0-8.573-3.007-9.963-7.178Z'
    />
    <path
      strokeLinecap='round'
      strokeLinejoin='round'
      d='M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z'
    />
  </svg>
}

function EyeOffGlyph() {
  return <svg
    aria-hidden='true'
    viewBox='0 0 24 24'
    fill='none'
    stroke='currentColor'
    strokeWidth={1.5}
    className='h-5 w-5'
  >
    <path
      strokeLinecap='round'
      strokeLinejoin='round'
      d='M3.98 8.223A10.477 10.477 0 0 0 1.934 12C3.226 16.338 7.244 19.5 12 19.5c.993 0
        1.953-.138 2.863-.395M6.228 6.228A10.451 10.451 0 0 1 12 4.5c4.756 0 8.773 3.162
        10.065 7.498a10.523 10.523 0 0 1-4.293 5.774M6.228 6.228 3 3m3.228 3.228 3.65
        3.65m7.894 7.894L21 21m-3.228-3.228-3.65-3.65m0 0a3 3 0 1 0-4.243-4.243m4.242 4.242L9.88 9.88'
    />
  </svg>
}
