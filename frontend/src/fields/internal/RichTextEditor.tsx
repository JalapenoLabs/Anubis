// Copyright © 2026 Jalapeno Labs

import type { RichTextLabels } from '../types'
import type { Editor } from '@tiptap/core'

// Core
import { useEffect } from 'react'
import { EditorContent, useEditor } from '@tiptap/react'

// User interface
import { Button, Tooltip } from '@heroui/react'

// Utility
import StarterKit from '@tiptap/starter-kit'

// Misc
import { RICH_TEXT_PROSE } from './richText'

type Props = {
  id: string
  ariaLabel?: string
  /** The stored HTML, or `''` for an empty document. */
  html: string
  labels: RichTextLabels
  isInvalid: boolean
  isDisabled?: boolean
  isReadOnly?: boolean
  autoFocus?: boolean
  onChange: (html: string) => void
  onBlur: () => void
}

/**
 * The Tiptap editor behind `RichTextField`, loaded on demand.
 *
 * Kept out of `RichTextField.tsx` on purpose: that file is what a form
 * imports, and this one is what a dynamic import fetches, so ProseMirror and
 * its schema stay out of an application's initial chunk until a form that
 * edits rich text is actually rendered.
 *
 * The value is **HTML**, which is what Bullet Train's `trix_editor` stores and
 * the only representation every other consumer of a record already
 * understands: an email body, an export, a webhook payload, and a `/api/v1`
 * response all carry markup without agreeing on an editor's document model
 * first. An empty document is stored as `''` rather than as `<p></p>`, so a
 * nullable column ends up NULL when a user clears it.
 */
export function RichTextEditor(props: Props) {
  const isEditable = !props.isDisabled && !props.isReadOnly

  const editor = useEditor({
    extensions: [ StarterKit ],
    content: props.html,
    editable: isEditable,
    autofocus: props.autoFocus,
    editorProps: {
      attributes: {
        'id': props.id,
        'role': 'textbox',
        'aria-multiline': 'true',
        'aria-label': props.ariaLabel ?? '',
        'class': `min-h-32 w-full px-3 py-2 outline-none ${RICH_TEXT_PROSE}`,
      },
    },
    onUpdate: ({ editor: updated }) => {
      props.onChange(updated.isEmpty ? '' : updated.getHTML())
    },
    onBlur: props.onBlur,
  })

  // The form owns the value, so a reset (a create that succeeded, or a
  // different record being edited) has to reach the document. Comparing
  // before writing is what stops the editor's own keystrokes from bouncing
  // back and collapsing the selection on every character.
  useEffect(() => {
    if (!editor || editor.isDestroyed) {
      return
    }
    const current = editor.isEmpty ? '' : editor.getHTML()
    if (current !== props.html) {
      editor.commands.setContent(props.html, false)
    }
  }, [ editor, props.html ])

  useEffect(() => {
    editor?.setEditable(isEditable)
  }, [ editor, isEditable ])

  const border = props.isInvalid
    ? 'border-danger'
    : 'border-default-200'

  return <div className={`overflow-hidden rounded-medium border bg-default-100 ${border}`}>
    { isEditable && editor
      ? <RichTextToolbar
          editor={editor}
          labels={props.labels}
        />
      : null
    }
    <EditorContent editor={editor} />
  </div>
}

type ToolbarProps = {
  editor: Editor
  labels: RichTextLabels
}

/**
 * The formatting controls, typographic rather than iconographic.
 *
 * A `B` and an `I` need no icon set, which keeps the package's dependency
 * list to the editor itself. Every button carries a tooltip and an accessible
 * name, because none of them carries a word.
 */
function RichTextToolbar(props: ToolbarProps) {
  const { editor, labels } = props

  const actions = [
    {
      key: 'bold',
      face: <span className='font-bold'>B</span>,
      title: labels.bold,
      isActive: editor.isActive('bold'),
      run: () => editor.chain().focus().toggleBold().run(),
    },
    {
      key: 'italic',
      face: <span className='italic'>I</span>,
      title: labels.italic,
      isActive: editor.isActive('italic'),
      run: () => editor.chain().focus().toggleItalic().run(),
    },
    {
      key: 'strike',
      face: <span className='line-through'>S</span>,
      title: labels.strike,
      isActive: editor.isActive('strike'),
      run: () => editor.chain().focus().toggleStrike().run(),
    },
    {
      key: 'heading',
      face: <span>H2</span>,
      title: labels.heading,
      isActive: editor.isActive('heading', { level: 2 }),
      run: () => editor.chain().focus().toggleHeading({ level: 2 }).run(),
    },
    {
      key: 'subheading',
      face: <span>H3</span>,
      title: labels.subheading,
      isActive: editor.isActive('heading', { level: 3 }),
      run: () => editor.chain().focus().toggleHeading({ level: 3 }).run(),
    },
    {
      key: 'bulletList',
      face: <span>&bull;</span>,
      title: labels.bulletList,
      isActive: editor.isActive('bulletList'),
      run: () => editor.chain().focus().toggleBulletList().run(),
    },
    {
      key: 'orderedList',
      face: <span>1.</span>,
      title: labels.orderedList,
      isActive: editor.isActive('orderedList'),
      run: () => editor.chain().focus().toggleOrderedList().run(),
    },
    {
      key: 'quote',
      face: <span>&ldquo;</span>,
      title: labels.quote,
      isActive: editor.isActive('blockquote'),
      run: () => editor.chain().focus().toggleBlockquote().run(),
    },
    {
      key: 'code',
      face: <span className='font-mono'>&lt;&gt;</span>,
      title: labels.code,
      isActive: editor.isActive('codeBlock'),
      run: () => editor.chain().focus().toggleCodeBlock().run(),
    },
    {
      key: 'undo',
      face: <span>&#8617;</span>,
      title: labels.undo,
      isActive: false,
      run: () => editor.chain().focus().undo().run(),
    },
    {
      key: 'redo',
      face: <span>&#8618;</span>,
      title: labels.redo,
      isActive: false,
      run: () => editor.chain().focus().redo().run(),
    },
  ]

  return <div className='flex flex-wrap gap-1 border-b border-default-200 p-1'>{
    actions.map((action) => <Tooltip
      key={action.key}
      content={action.title}
    >
      <div>
        <Button
          type='button'
          size='sm'
          isIconOnly
          variant={action.isActive ? 'flat' : 'light'}
          color={action.isActive ? 'primary' : 'default'}
          aria-label={action.title}
          aria-pressed={action.isActive}
          onPress={action.run}
        >{
          action.face
        }</Button>
      </div>
    </Tooltip>)
  }</div>
}
