// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// User interface
import { TextualField } from './internal/TextualField'

type Props<Values extends FieldValues> = AnubisFieldProps<Values> & {
  autoComplete?: string
}

/**
 * The `phone_field` scaffolder type: a telephone number, with the dial keyboard
 * on touch devices.
 *
 * The number is stored exactly as typed. Country selection and E.164
 * normalization need a country database, so they arrive with the intl-tel-input
 * style upgrade rather than pulling that weight into every application now.
 */
export function PhoneField<Values extends FieldValues>(props: Props<Values>) {
  return <TextualField
    autoComplete='tel'
    {...props}
    type='tel'
    inputMode='tel'
  />
}
