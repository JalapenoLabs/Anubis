// Copyright © 2026 Jalapeno Labs

import type { FieldValues } from 'react-hook-form'
import type { UploadProps } from './internal/UploadField'

// User interface
import { DEFAULT_UPLOAD_LABELS, UploadField } from './internal/UploadField'

/** What an image field accepts when the form does not narrow it further. */
const DEFAULT_ACCEPT = 'image/*'

/**
 * The `image` scaffolder type: `file_field` that shows what was chosen.
 *
 * Identical to [`FileField`](./FileField.tsx) but for the two things an image
 * changes: the stored value renders as a thumbnail instead of a link, and the
 * picker defaults to offering images. Same `onUpload` seam, same
 * `FileReference` value, so an application wires one endpoint for both.
 */
export function ImageField<Values extends FieldValues>(props: UploadProps<Values>) {
  return <UploadField
    {...props}
    accept={props.accept ?? DEFAULT_ACCEPT}
    labels={{
      ...DEFAULT_UPLOAD_LABELS,
      ...props.labels,
    }}
    withPreview
  />
}
