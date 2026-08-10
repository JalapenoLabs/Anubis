// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { TextualField } from './internal/TextualField'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  autoComplete?: string
}

/**
 * The `password_field` scaffolder type: a masked secret.
 *
 * Pass `autoComplete` to tell the browser which secret this is, since only the
 * form knows whether it is a sign-in, a new password, or a one-off token.
 */
export function PasswordField<Values extends FieldValues>(props: Props<Values>) {
  return <TextualField
    {...props}
    type='password'
  />
}
