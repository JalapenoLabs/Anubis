# Bullet Train Developer Documentation (vendored)

A local copy of the Bullet Train developer documentation published at
[bullettrain.co/docs](https://bullettrain.co/docs), kept here so agents and
editors can read the framework's own guidance without network access.

Start at [index.md](index.md), which is upstream's table of contents.

## Provenance

| | |
| --- | --- |
| Source | [`bullet-train-co/bullet_train-core`](https://github.com/bullet-train-co/bullet_train-core), path `bullet_train/docs` |
| Revision | `2937695052a8f142f7166ca564ee6a76d95f9650` |
| Pages | 57 |
| License | MIT, see [MIT-LICENSE](MIT-LICENSE), Copyright 2022 Bullet Train, Inc. |

The website renders these Markdown files directly, so this copy is the authored
source rather than a scrape of the rendered HTML.

## What was changed

Prose, code samples, and headings are verbatim. Only markup that exists to serve
the website, and that degrades once the pages leave it, was rewritten:

- Tailwind-styled callout `<div>`s became Markdown blockquotes.
- `<pre><code>` blocks became fenced blocks with a language tag.
- An embedded tweet became a blockquote, dropping its loader `<script>`.
- Decorative icon glyphs, line-break, rule, and superscript footnote tags were
  reduced to their Markdown equivalents.
- Site-absolute links (`/docs/teams.md`, `/docs/upgrades`) and bare relative
  links (`indirection`) became relative paths that resolve on disk, so following
  a reference opens the right file.

HTML inside fenced code blocks is sample code and was left untouched.

## Refreshing

```bash
tools/sync_docs.sh          # or: tools/sync_docs.sh <ref>
```

The script fetches every page under the upstream docs directory, so pages added
upstream are picked up automatically, then re-applies the normalization above.
Update the revision in the table when you sync.
