// Copyright © 2026 Jalapeno Labs

import type { AnubisFieldProps } from './types'
import type { FieldValues } from 'react-hook-form'

// Core
import { useId } from 'react'
import { useController } from 'react-hook-form'

/**
 * Binds a field component to react-hook-form and resolves its display state.
 *
 * Every field calls this once, which is what keeps the id wiring and the
 * error-message policy defined in exactly one place: an explicit `error` prop
 * wins over the resolver's message, and a field with a message is invalid.
 */
export function useFieldState<Values extends FieldValues>(props: AnubisFieldProps<Values>) {
  const generatedId = useId()

  const { field, fieldState } = useController<Values>({
    control: props.control,
    name: props.name,
  })

  const errorMessage = props.error ?? fieldState.error?.message ?? null

  return {
    fieldId: props.id ?? generatedId,
    field,
    errorMessage,
    isInvalid: Boolean(errorMessage),
  } as const
}
