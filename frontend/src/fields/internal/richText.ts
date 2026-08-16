// Copyright © 2026 Jalapeno Labs

/**
 * How rich text renders, in the editor and on the page that displays it.
 *
 * Tailwind's preflight strips headings, lists, and quotes back to plain text,
 * so a document written in `RichTextField` would read as one undifferentiated
 * paragraph without this. The rules are child selectors rather than a
 * typography plugin, because a framework should not decide that every
 * application installs one; an application that does install
 * `@tailwindcss/typography` ejects `RichTextView` and swaps this for `prose`.
 *
 * It lives in one constant so the editor and the view can never drift: what a
 * person writes is what the show page renders.
 */
export const RICH_TEXT_PROSE = [
  '[&_p]:my-2',
  '[&_h1]:my-3 [&_h1]:text-2xl [&_h1]:font-semibold',
  '[&_h2]:my-3 [&_h2]:text-xl [&_h2]:font-semibold',
  '[&_h3]:my-2 [&_h3]:text-lg [&_h3]:font-semibold',
  '[&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-6',
  '[&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-6',
  '[&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:border-default-300',
  '[&_blockquote]:pl-3 [&_blockquote]:opacity-80',
  '[&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-medium [&_pre]:bg-default-200 [&_pre]:p-3',
  '[&_code]:font-mono [&_code]:text-small',
  '[&_a]:text-primary [&_a]:underline',
  '[&_hr]:my-4 [&_hr]:border-default-200',
].join(' ')
