// Copyright © 2026 Jalapeno Labs

import type { ReactNode } from 'react'

type Props = {
  /** The id of the control this label points at. */
  htmlFor?: string
  label?: string
  isRequired?: boolean
  help?: string
  /** Shown in place of the help text while the field is invalid. */
  error?: string | null
  className?: string
  children: ReactNode
}

/**
 * The layout every Anubis field shares: label, required marker, control, and
 * one line of help or error text.
 *
 * Every field renders its label here rather than through the control's own
 * label prop, so a text input, a switch, and a radio group all line up on the
 * same grid. Controls therefore receive `aria-label` for their accessible name.
 */
export function FieldWrapper(props: Props) {
  const className = props.className
    ? `compact ${props.className}`
    : 'compact'

  return <div className={className}>
    { props.label
      ? <label className='mb-1 block text-small font-medium' htmlFor={props.htmlFor}>
          <span>{props.label}</span>
          { props.isRequired
            ? <span className='ml-0.5 text-danger' aria-hidden='true'>*</span>
            : null
          }
        </label>
      : null
    }
    {props.children}
    { props.error
      ? <p className='mt-1 text-tiny text-danger' role='alert'>{props.error}</p>
      : null
    }
    { !props.error && props.help
      ? <p className='mt-1 text-tiny opacity-70'>{props.help}</p>
      : null
    }
  </div>
}
