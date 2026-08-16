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
 * The words on `RichTextField`'s toolbar, which a form overrides one by one.
 *
 * The package never imports i18next, so the defaults are English and a
 * translated form passes its own. They are one object rather than eleven
 * props because a form that translates one of them translates all of them.
 */
export type RichTextLabels = {
  bold: string
  italic: string
  strike: string
  heading: string
  subheading: string
  bulletList: string
  orderedList: string
  quote: string
  code: string
  undo: string
  redo: string
}

/**
 * The languages `CodeEditorField` highlights without an `extensions` prop.
 *
 * The list is the set of CodeMirror language packages the framework depends
 * on, each reached through its own dynamic import, so a form that edits SQL
 * downloads the SQL grammar and nothing else. Anything outside it is reached
 * by passing CodeMirror extensions directly.
 */
export type CodeLanguage =
  | 'css'
  | 'html'
  | 'javascript'
  | 'json'
  | 'jsx'
  | 'markdown'
  | 'sql'
  | 'tsx'
  | 'typescript'

/**
 * A stored file, as `FileField` and `ImageField` hold it in the form.
 *
 * The reference is what the application's own upload endpoint answers with:
 * the field never speaks to a server itself, it calls the `onUpload` the form
 * gives it and stores what comes back. `url` is the only member a control
 * needs; the rest is what makes the chosen file readable to a person.
 */
export type FileReference = {
  /** Where the stored file is served from. */
  url: string
  /** The original filename, shown beside the link. */
  name?: string
  /** The size in bytes, rendered beside the name when it is known. */
  size?: number
  /** The MIME type the upload reported. */
  contentType?: string
}

/**
 * The words `FileField` and `ImageField` put on screen.
 *
 * Same rule as the rest of the library: the package never imports i18next, so
 * the defaults are English and a translated form passes its own.
 */
export type UploadLabels = {
  choose: string
  replace: string
  remove: string
  uploading: string
  /** Shown with the limit substituted for `{max}`, e.g. `Larger than {max}.` */
  tooLarge: string
  failed: string
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
