// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { Input } from '@heroui/react'
import { FieldWrapper } from './FieldWrapper'

// Misc
import { useFieldState } from './useFieldState'

/**
 * The last grapheme of `text`, or `''`.
 *
 * An emoji is frequently several code points (a flag is two, a family is
 * seven joined by zero-width joiners), so neither `text[0]` nor `Array.from`
 * gives "one emoji". `Intl.Segmenter` is the only correct answer, and it is in
 * every browser this framework targets; where it is missing the field keeps
 * the whole value rather than cutting an emoji in half.
 *
 * The **last** grapheme rather than the first, because typing over a filled
 * field should replace what is there, which is what a one-character field
 * feels like it should do.
 */
export function lastGrapheme(text: string): string {
  if (!text) {
    return ''
  }
  if (typeof Intl.Segmenter !== 'function') {
    console.debug('lastGrapheme has no Intl.Segmenter, keeping the value whole', text)
    return text
  }

  const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
  let last = ''
  for (const segment of segmenter.segment(text)) {
    last = segment.segment
  }
  return last
}

/**
 * The `emoji_field` scaffolder type: exactly one emoji, stored as text.
 *
 * Bullet Train's `emoji_field` opens Emoji Mart. This one does not, and the
 * reason is a trade worth stating plainly: Emoji Mart's picker carries a
 * 1.4 MB emoji dataset, ships types that omit its own data module's export,
 * and pins React through a peer range narrower than this package's. Every
 * platform a user reaches this field from already has a complete, native,
 * always-current picker on a keyboard shortcut (`Win` `.` on Windows,
 * `Ctrl` `Cmd` `Space` on macOS, the emoji key on every touch keyboard), and
 * the field is a single character.
 *
 * So the control is an ordinary text input that keeps one grapheme, which
 * makes the column an ordinary `TEXT` that scaffolds end to end and renders on
 * a show page as itself. An application that wants the in-page picker anyway
 * ejects this component and adds the dependency to its own tree, where its
 * React range is its own problem rather than the framework's.
 */
export function EmojiField<Values extends FieldValues>(props: AnubisFieldProps<Values>) {
  const { fieldId, field, errorMessage, isInvalid } = useFieldState(props)

  return <FieldWrapper
    htmlFor={fieldId}
    label={props.label}
    isRequired={props.isRequired}
    help={props.help}
    error={errorMessage}
    className={props.className}
  >
    <Input
      id={fieldId}
      ref={field.ref}
      name={field.name}
      type='text'
      aria-label={props.label}
      className='w-24'
      classNames={{ input: 'text-center text-2xl' }}
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      autoComplete='off'
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      isReadOnly={props.isReadOnly}
      isInvalid={isInvalid}
      value={field.value == null ? '' : String(field.value)}
      onValueChange={(value) => field.onChange(lastGrapheme(value))}
      onBlur={field.onBlur}
    />
  </FieldWrapper>
}
