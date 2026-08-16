// Copyright © 2026 Jalapeno Labs

import type { FileReference } from './types'
import type { ReactNode } from 'react'
import type { Control, UseFormReturn } from 'react-hook-form'

// Core
import { describe, expect, it } from 'vitest'
import { useForm } from 'react-hook-form'

// User interface
import { HeroUIProvider } from '@heroui/react'
import { BooleanField } from './BooleanField'
import { ButtonsField } from './ButtonsField'
import { ColorPickerField } from './ColorPickerField'
import { DateAndTimeField, toLocalDateTimeInput, toUtcTimestamp } from './DateAndTimeField'
import { DateField } from './DateField'
import { EmailField } from './EmailField'
import { EmojiField, lastGrapheme } from './EmojiField'
import { FileField } from './FileField'
import { ImageField } from './ImageField'
import { NumberField } from './NumberField'
import { OptionsField } from './OptionsField'
import { PasswordField } from './PasswordField'
import { PhoneField } from './PhoneField'
import { RichTextView } from './RichTextView'
import { SuperSelectField } from './SuperSelectField'
import { TextAreaField } from './TextAreaField'
import { TextField } from './TextField'
import { formatBytes } from './internal/UploadField'

// Utility
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

type HarnessValues = {
  text: string
  flag: boolean
  count: number | null
  choice: string | null
  choices: string[]
  date: string | null
  timestamp: string | null
  color: string
  emoji: string
  attachment: FileReference | null
}

const defaultHarnessValues: HarnessValues = {
  text: '',
  flag: false,
  count: null,
  choice: null,
  choices: [],
  date: null,
  timestamp: null,
  color: '',
  emoji: '',
  attachment: null,
}

const statusOptions = [
  {
    value: 'draft',
    label: 'Draft',
  },
  {
    value: 'published',
    label: 'Published',
  },
]

type HarnessProps = {
  values?: Partial<HarnessValues>
  formRef: { current: UseFormReturn<HarnessValues> | null }
  children: (control: Control<HarnessValues>) => ReactNode
}

function FieldHarness(props: HarnessProps) {
  const form = useForm<HarnessValues>({
    defaultValues: {
      ...defaultHarnessValues,
      ...props.values,
    },
  })

  // The tests read submitted state straight off the form, which is the whole
  // point: a field is correct when react-hook-form holds the right value.
  props.formRef.current = form

  return <HeroUIProvider>{
      props.children(form.control)
    }</HeroUIProvider>
}

function renderField(
  children: (control: Control<HarnessValues>) => ReactNode,
  values?: Partial<HarnessValues>,
) {
  const formRef: { current: UseFormReturn<HarnessValues> | null } = { current: null }

  render(<FieldHarness
    formRef={formRef}
    values={values}
  >{
      children
    }</FieldHarness>)

  return formRef
}

describe('FieldWrapper, through TextField', () => {
  it('should render the label, and mark it required', () => {
    renderField((control) => <TextField
      control={control}
      name='text'
      label='Display name'
      isRequired
    />)

    expect(screen.getByText('Display name')).toBeTruthy()
    expect(screen.getByText('*')).toBeTruthy()
  })

  it('should render help text when the field is valid', () => {
    renderField((control) => <TextField
      control={control}
      name='text'
      label='Display name'
      help='Shown to everyone on the team.'
    />)

    expect(screen.getByText('Shown to everyone on the team.')).toBeTruthy()
  })

  it('should replace the help text with the error message', () => {
    renderField((control) => <TextField
      control={control}
      name='text'
      label='Display name'
      help='Shown to everyone on the team.'
      error='Enter a display name.'
    />)

    expect(screen.getByRole('alert').textContent).toBe('Enter a display name.')
    expect(screen.queryByText('Shown to everyone on the team.')).toBeNull()
  })

  it('should leave the required marker off an optional field', () => {
    renderField((control) => <TextField
      control={control}
      name='text'
      label='Display name'
    />)

    expect(screen.queryByText('*')).toBeNull()
  })
})

describe('TextField', () => {
  it('should propagate typing into the form', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <TextField
      control={control}
      name='text'
      label='Display name'
    />)

    await user.type(screen.getByLabelText('Display name'), 'Anubis')

    expect(formRef.current?.getValues('text')).toBe('Anubis')
  })
})

describe('TextAreaField', () => {
  it('should propagate typing into the form', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <TextAreaField
      control={control}
      name='text'
      label='Description'
    />)

    await user.type(screen.getByLabelText('Description'), 'Two lines')

    expect(formRef.current?.getValues('text')).toBe('Two lines')
  })
})

describe('NumberField', () => {
  it('should store a number, and null once cleared', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <NumberField
      control={control}
      name='count'
      label='Seats'
    />)

    const input = screen.getByLabelText('Seats')
    await user.type(input, '42')
    expect(formRef.current?.getValues('count')).toBe(42)

    await user.clear(input)
    expect(formRef.current?.getValues('count')).toBeNull()
  })
})

describe('EmailField', () => {
  it('should render an email input and propagate typing', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <EmailField
      control={control}
      name='text'
      label='Email'
    />)

    const input = screen.getByLabelText('Email')
    expect(input.getAttribute('type')).toBe('email')

    await user.type(input, 'team@example.com')
    expect(formRef.current?.getValues('text')).toBe('team@example.com')
  })
})

describe('PasswordField', () => {
  it('should mask the value and propagate typing', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <PasswordField
      control={control}
      name='text'
      label='Password'
    />)

    const input = screen.getByLabelText('Password')
    expect(input.getAttribute('type')).toBe('password')

    await user.type(input, 'hunter2hunter2')
    expect(formRef.current?.getValues('text')).toBe('hunter2hunter2')
  })
})

describe('PhoneField', () => {
  it('should render a telephone input and propagate typing', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <PhoneField
      control={control}
      name='text'
      label='Phone'
    />)

    const input = screen.getByLabelText('Phone')
    expect(input.getAttribute('type')).toBe('tel')

    await user.type(input, '+1 405 555 0199')
    expect(formRef.current?.getValues('text')).toBe('+1 405 555 0199')
  })
})

describe('BooleanField', () => {
  it('should toggle the form value', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <BooleanField
      control={control}
      name='flag'
      label='Published'
    />)

    await user.click(screen.getByRole('switch', { name: 'Published' }))

    expect(formRef.current?.getValues('flag')).toBe(true)
  })
})

describe('ButtonsField', () => {
  it('should select the pressed option', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <ButtonsField
      control={control}
      name='choice'
      label='Status'
      options={statusOptions}
    />)

    await user.click(screen.getByRole('radio', { name: 'Published' }))

    expect(formRef.current?.getValues('choice')).toBe('published')
    expect(screen.getByRole('radio', { name: 'Published' }).getAttribute('aria-checked')).toBe('true')
  })
})

describe('OptionsField', () => {
  it('should select a radio option', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <OptionsField
      control={control}
      name='choice'
      label='Status'
      variant='radio'
      options={statusOptions}
    />)

    await user.click(screen.getByRole('radio', { name: 'Draft' }))

    expect(formRef.current?.getValues('choice')).toBe('draft')
  })

  it('should select an option from the dropdown', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <OptionsField
      control={control}
      name='choice'
      label='Status'
      options={statusOptions}
    />)

    await user.click(screen.getByRole('button', { name: 'Status' }))
    await user.click(await screen.findByRole('option', { name: 'Published' }))

    expect(formRef.current?.getValues('choice')).toBe('published')
  })
})

describe('SuperSelectField', () => {
  it('should select a single searchable option', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <SuperSelectField
      control={control}
      name='choice'
      label='Owner'
      options={statusOptions}
    />)

    await user.click(screen.getByLabelText('Owner'))
    await user.click(await screen.findByRole('option', { name: 'Draft' }))

    expect(formRef.current?.getValues('choice')).toBe('draft')
  })

  it('should collect several values, and drop one from its chip', async () => {
    const user = userEvent.setup()
    const formRef = renderField(
      (control) => <SuperSelectField
        control={control}
        name='choices'
        label='Tags'
        options={statusOptions}
        isMultiple
      />,
      { choices: [ 'draft' ]},
    )

    await user.click(screen.getByLabelText('Tags'))
    await user.click(await screen.findByRole('option', { name: 'Published' }))
    expect(formRef.current?.getValues('choices')).toEqual([ 'draft', 'published' ])

    // The open listbox hides the rest of the field from assistive technology,
    // so dismiss it before reaching for a chip.
    await user.keyboard('{Escape}')
    await user.click(screen.getAllByRole('button', { name: 'close chip' })[0])
    expect(formRef.current?.getValues('choices')).toEqual([ 'published' ])
  })
})

describe('DateField', () => {
  it('should store the calendar date verbatim', () => {
    const formRef = renderField((control) => <DateField
      control={control}
      name='date'
      label='Due date'
    />)

    fireEvent.change(screen.getByLabelText('Due date'), { target: { value: '2026-08-10' }})

    expect(formRef.current?.getValues('date')).toBe('2026-08-10')
  })
})

describe('DateAndTimeField', () => {
  it('should render the stored timestamp in the local zone', () => {
    renderField(
      (control) => <DateAndTimeField
        control={control}
        name='timestamp'
        label='Starts at'
      />,
      { timestamp: '2026-08-10T15:30:00Z' },
    )

    const expected = toLocalDateTimeInput('2026-08-10T15:30:00Z')
    expect(screen.getByLabelText<HTMLInputElement>('Starts at').value).toBe(expected)
  })

  it('should store an edited value as a UTC timestamp', () => {
    const formRef = renderField((control) => <DateAndTimeField
      control={control}
      name='timestamp'
      label='Starts at'
    />)

    fireEvent.change(screen.getByLabelText('Starts at'), { target: { value: '2026-08-10T09:15' }})

    expect(formRef.current?.getValues('timestamp')).toBe(new Date('2026-08-10T09:15').toISOString())
  })

  it('should round-trip a timestamp through both conversions', () => {
    const stored = new Date('2026-08-10T09:15').toISOString()

    expect(toUtcTimestamp(toLocalDateTimeInput(stored))).toBe(stored)
  })

  it('should clear on empty input, and ignore unparseable values', () => {
    expect(toUtcTimestamp('')).toBeNull()
    expect(toUtcTimestamp('not a timestamp')).toBeNull()
    expect(toLocalDateTimeInput('not a timestamp')).toBe('')
    expect(toLocalDateTimeInput(null)).toBe('')
  })
})

describe('ColorPickerField', () => {
  it('should propagate a typed hex value', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <ColorPickerField
      control={control}
      name='color'
      label='Brand color'
    />)

    await user.type(screen.getByLabelText('Brand color'), '#ff8800')

    expect(formRef.current?.getValues('color')).toBe('#ff8800')
  })

  it('should keep the swatch out of the tab order', () => {
    const { container } = render(<FieldHarness
      formRef={{ current: null }}
    >{
        (control) => <ColorPickerField
          control={control}
          name='color'
          label='Brand color'
        />
      }</FieldHarness>)

    const swatch = container.querySelector('input[type="color"]')
    expect(swatch?.getAttribute('tabindex')).toBe('-1')
  })
})

describe('PasswordField reveal toggle', () => {
  it('should swap the input type, and say which state it is in', async () => {
    const user = userEvent.setup()
    renderField((control) => <PasswordField
      control={control}
      name='text'
      label='Password'
    />)

    const input = screen.getByLabelText('Password')
    expect(input.getAttribute('type')).toBe('password')

    await user.click(screen.getByRole('button', { name: 'Show password' }))
    expect(input.getAttribute('type')).toBe('text')

    await user.click(screen.getByRole('button', { name: 'Hide password' }))
    expect(input.getAttribute('type')).toBe('password')
  })
})

describe('EmojiField', () => {
  it('should keep only the last grapheme typed', async () => {
    const user = userEvent.setup()
    const formRef = renderField((control) => <EmojiField
      control={control}
      name='emoji'
      label='Icon'
    />)

    await user.type(screen.getByLabelText('Icon'), 'ab')

    expect(formRef.current?.getValues('emoji')).toBe('b')
  })

  it('should treat a multi-code-point emoji as one character', () => {
    // A family is seven code points joined by zero-width joiners, and a flag
    // is two regional indicators: neither survives `text[0]`.
    expect(lastGrapheme('x👩‍👩‍👧‍👦')).toBe('👩‍👩‍👧‍👦')
    expect(lastGrapheme('🇺🇸')).toBe('🇺🇸')
    expect(lastGrapheme('')).toBe('')
  })
})

describe('RichTextView', () => {
  it('should render the markup the editor writes', () => {
    const { container } = render(<RichTextView html='<p>Hello <strong>world</strong></p>' />)

    expect(container.querySelector('strong')?.textContent).toBe('world')
  })

  it('should render nothing at all for an empty value', () => {
    const { container } = render(<RichTextView html={null} />)

    expect(container.firstChild).toBeNull()
  })
})

describe('formatBytes', () => {
  it('should count in the largest unit that leaves a whole number', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(1_500_000)).toBe('1.4 MB')
  })
})

describe('FileField', () => {
  it('should store the reference the upload resolves', async () => {
    const user = userEvent.setup()
    const stored: FileReference = {
      url: 'https://files.example.com/report.pdf',
      name: 'report.pdf',
      size: 2048,
    }
    const formRef = renderField((control) => <FileField
      control={control}
      name='attachment'
      label='Attachment'
      onUpload={() => Promise.resolve(stored)}
    />)

    await user.upload(
      screen.getByLabelText<HTMLInputElement>('Attachment'),
      new File([ 'a report' ], 'report.pdf', { type: 'application/pdf' }),
    )

    expect(formRef.current?.getValues('attachment')).toEqual(stored)
    expect(await screen.findByText('report.pdf')).toBeTruthy()
    expect(screen.getByText('2.0 KB')).toBeTruthy()
  })

  it('should refuse an oversized file before uploading it', async () => {
    const user = userEvent.setup()
    let uploads = 0
    const formRef = renderField((control) => <FileField
      control={control}
      name='attachment'
      label='Attachment'
      maxBytes={4}
      onUpload={() => {
        uploads += 1
        return Promise.resolve({ url: 'https://files.example.com/big.pdf' })
      }}
    />)

    await user.upload(
      screen.getByLabelText<HTMLInputElement>('Attachment'),
      new File([ 'far too many bytes' ], 'big.pdf', { type: 'application/pdf' }),
    )

    expect(uploads).toBe(0)
    expect(formRef.current?.getValues('attachment')).toBeNull()
    expect(screen.getByRole('alert').textContent).toContain('4 B')
  })

  it('should clear the stored file', async () => {
    const user = userEvent.setup()
    const formRef = renderField(
      (control) => <FileField
        control={control}
        name='attachment'
        label='Attachment'
        onUpload={() => Promise.resolve({ url: 'https://files.example.com/report.pdf' })}
      />,
      { attachment: { url: 'https://files.example.com/report.pdf', name: 'report.pdf' }},
    )

    await user.click(screen.getByRole('button', { name: 'Remove' }))

    expect(formRef.current?.getValues('attachment')).toBeNull()
  })
})

describe('ImageField', () => {
  it('should show the stored image rather than a link', () => {
    const { container } = render(<FieldHarness
      formRef={{ current: null }}
      values={{ attachment: { url: 'https://files.example.com/logo.png', name: 'logo.png' }}}
    >{
        (control) => <ImageField
          control={control}
          name='attachment'
          label='Logo'
          onUpload={() => Promise.resolve({ url: 'https://files.example.com/logo.png' })}
        />
      }</FieldHarness>)

    expect(container.querySelector('img')?.getAttribute('src'))
      .toBe('https://files.example.com/logo.png')
    expect(container.querySelector('a')).toBeNull()
  })
})

describe('SuperSelectField incremental search', () => {
  it('should report the typed query once the typing stops', async () => {
    const user = userEvent.setup()
    const queries: string[] = []
    renderField((control) => <SuperSelectField
      control={control}
      name='choice'
      label='Owner'
      options={statusOptions}
      onSearch={(query) => queries.push(query)}
    />)

    await user.type(screen.getByLabelText('Owner'), 'dra')

    await waitFor(() => expect(queries.at(-1)).toBe('dra'))
    // Three keystrokes are one search: the debounce is the whole feature.
    expect(queries).toEqual([ 'dra' ])
  })
})
