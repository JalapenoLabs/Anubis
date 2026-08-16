// Copyright © 2026 Jalapeno Labs

import type { FieldValues } from 'react-hook-form'
import type { UploadProps } from './internal/UploadField'

// User interface
import { DEFAULT_UPLOAD_LABELS, UploadField } from './internal/UploadField'

/**
 * The `file_field` scaffolder type: one stored file, chosen and replaced.
 *
 * The form value is a `FileReference | null`, and `onUpload` is the seam: it
 * receives the chosen `File`, stores it however the application stores files,
 * and resolves the reference the form holds. Nothing about the field assumes
 * where the bytes went, which is what lets one component serve an application
 * on S3, one on Postgres, and one behind a signed CDN URL.
 */
export function FileField<Values extends FieldValues>(props: UploadProps<Values>) {
  return <UploadField
    {...props}
    labels={{
      ...DEFAULT_UPLOAD_LABELS,
      ...props.labels,
    }}
    withPreview={false}
  />
}
