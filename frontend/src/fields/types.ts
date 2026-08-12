// Copyright © 2026 Jalapeno Labs

import type { Control, FieldPath, FieldValues } from 'react-hook-form'

/**
 * One choice in a `buttons`, `options`, or `super_select` field.
 *
 * Labels and descriptions arrive already translated: the scaffolder emits the
 * option list into the model's locale file, and the page maps it through `t`.
 */
export type FieldOption = {
  /** The value written into the form, and onto the wire. */
  value: string
  label: string
  /** Secondary text shown under the label, where the control supports it. */
  description?: string
  isDisabled?: boolean
}

/**
 * The contract every Anubis field component honors.
 *
 * `control` and `name` bind the field to react-hook-form, so a scaffolded form
 * is one component per model attribute with no wiring in between. Every string
 * is already translated: the library never imports i18next, the application
 * owns translation and passes `t(...)` results down.
 */
export type AnubisFieldProps<Values extends FieldValues> = {
  control: Control<Values>
  name: FieldPath<Values>
  /** Rendered above the control, with the required marker when `isRequired`. */
  label?: string
  /** Hint text under the control. The error message replaces it while invalid. */
  help?: string
  placeholder?: string
  /**
   * Overrides the message react-hook-form resolved for this field. Pass the
   * translated text when the resolver's own message is not user facing.
   */
  error?: string
  isRequired?: boolean
  isDisabled?: boolean
  isReadOnly?: boolean
  autoFocus?: boolean
  /** Overrides the generated control id. Rarely needed. */
  id?: string
  /** Extra classes on the field wrapper, not on the control. */
  className?: string
}
