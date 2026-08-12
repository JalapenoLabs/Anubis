// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { TextualField } from './internal/TextualField'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  autoComplete?: string
}

/** The `text_field` scaffolder type: a single line of free text. */
export function TextField<Values extends FieldValues>(props: Props<Values>) {
  return <TextualField
    {...props}
    type='text'
  />
}
