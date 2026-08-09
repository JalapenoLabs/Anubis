"""Normalize the vendored Bullet Train docs for offline and agent consumption.

Upstream authors these files as Markdown, so no prose is ever rewritten. This
pass only rewrites the parts that exist to serve the bullettrain.co website and
that degrade outside it:

  * Tailwind-styled callout `<div>`s become Markdown blockquotes.
  * `<pre><code>` blocks become fenced blocks with an inferred language.
  * An embedded tweet becomes a blockquote, dropping its loader `<script>`.
  * Decorative icon glyphs, `<br>`, `<hr>`, and `<sup>` footnote markers are
    reduced to their Markdown equivalents.
  * Site-absolute `/docs/...` links become relative paths that resolve on disk.

HTML inside fenced code blocks is sample code, never chrome, so link rewriting
tracks fences and leaves those regions untouched.

Usage: python normalize_docs.py [docs_root]
"""

import html
import os
import pathlib
import re
import sys

# --- Block-level conversions ------------------------------------------------

CALLOUT = re.compile(
    r'^<div class="rounded-md[^"]*">\n(?P<body>.*?)^</div>\n',
    re.DOTALL | re.MULTILINE,
)
CALLOUT_PART = re.compile(r"<(?P<tag>h3|p)\b[^>]*>(?P<text>.*?)</(?P=tag)>", re.DOTALL)
PRE_CODE = re.compile(r"<pre><code>(?P<code>.*?)</code></pre>", re.DOTALL)
TWEET = re.compile(r"^<center>\n(?P<body>.*?)^</center>\n", re.DOTALL | re.MULTILINE)
TWEET_TEXT = re.compile(r"<p[^>]*>(?P<text>.*?)</p>", re.DOTALL)
TWEET_CREDIT = re.compile(
    r"&mdash;\s*(?P<who>[^<]+?)\s*<a href=\"(?P<url>[^\"]+)\"[^>]*>(?P<when>[^<]+)</a>"
)

# --- Inline conversions -----------------------------------------------------
# Ordered: tags that carry a link or code span are unwrapped last, so that the
# markup they sit inside has already been flattened.

INLINE_SUBS = [
    # Decorative "opens in a new window" glyphs from the website's link styling.
    (re.compile(r"\s*<i class=\"ti [^\"]*\"></i>"), ""),
    (re.compile(r"<sup><a href=\"#footnote-1\">1</a></sup>"), "[1]"),
    (re.compile(r"<sup><a name=\"footnote-1\"></a>1</sup>"), "[1]"),
    # Editorial asides whose text is worth keeping but whose tags carry nothing.
    (re.compile(r"</?(?:aside|small|center)>"), ""),
    (re.compile(r"^<hr>$", re.MULTILINE), "---"),
    (re.compile(r"<br\s*/?>\n?"), "\n"),
    (re.compile(r"<code>(.*?)</code>"), r"`\1`"),
    (re.compile(r"<a href=\"([^\"]+)\"[^>]*>(.*?)</a>"), r"[\2](\1)"),
]

# --- Link rewriting ---------------------------------------------------------

FENCE = re.compile(r"^\s*(```|~~~)")
LINK = re.compile(r"\]\((?P<target>[^)\s]+)(?P<title>\s+\"[^\"]*\")?\)")
EXTERNAL = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//|#)")
# Routes served by a running Bullet Train app, not documentation pages.
APP_ROUTES = ("/users/",)


def collapse(text):
    """Flatten an HTML fragment to a single line of prose."""
    return re.sub(r"\s+", " ", text).strip()


def convert_callout(match):
    """Render a website notice box as a Markdown blockquote."""
    lines = []
    for part in CALLOUT_PART.finditer(match.group("body")):
        text = collapse(part.group("text"))
        if not text:
            continue
        # The box's <h3> is its lede. Bold preserves that emphasis without
        # adding a heading that would pollute the document outline.
        lines.append(f"**{text}**" if part.group("tag") == "h3" else text)
    return "\n>\n".join(f"> {line}" for line in lines) + "\n"


def convert_pre_code(match):
    """Turn a raw <pre><code> block into a fenced block with a guessed language."""
    code = html.unescape(match.group("code")).strip("\n")
    language = "erb" if code.lstrip().startswith("<%") else "yaml"
    return f"```{language}\n{code}\n```"


def convert_tweet(match):
    """Reduce an embedded tweet, and its loader <script>, to a blockquote."""
    body = match.group("body")
    text = TWEET_TEXT.search(body)
    if not text:
        return match.group(0)
    quote = collapse(html.unescape(text.group("text")))
    credit = TWEET_CREDIT.search(body)
    out = f"> {quote}\n"
    if credit:
        who = html.unescape(credit.group("who"))
        out += f">\n> {who} [{credit.group('when')}]({credit.group('url')})\n"
    return out


def resolve(root, target, source):
    """Map a documentation link to a path relative to the file containing it.

    Upstream links take three shapes: site-absolute with an extension
    (`/docs/teams.md`), site-absolute without one (`/docs/upgrades`), and bare
    relative (`indirection`). All three resolve to an on-disk relative path.
    Returns None when the target is not a document in this tree.
    """
    path, _, anchor = target.partition("#")
    if not path:
        return None

    if path.startswith("/docs/"):
        stem = path[len("/docs/") :]
        candidates = [root / stem, root / f"{stem}.md"]
    elif not path.startswith("/"):
        stem = path[2:] if path.startswith("./") else path
        here = (root / source).parent
        candidates = [here / stem, here / f"{stem}.md", root / stem, root / f"{stem}.md"]
    else:
        return None

    for candidate in candidates:
        if candidate.suffix == ".md" and candidate.is_file():
            relative = os.path.relpath(candidate, (root / source).parent)
            return relative.replace(os.sep, "/") + (f"#{anchor}" if anchor else "")
    return None


def rewrite_links(root, text, source, unresolved):
    out, in_fence = [], False
    for line in text.splitlines(keepends=True):
        if FENCE.match(line):
            in_fence = not in_fence
            out.append(line)
            continue
        if in_fence:
            out.append(line)
            continue

        def replace(match):
            target = match.group("target")
            title = match.group("title") or ""
            if EXTERNAL.match(target) or target.startswith(APP_ROUTES):
                return match.group(0)
            # Already points at something on disk, so leave it alone. This
            # covers non-Markdown neighbours such as MIT-LICENSE.
            if ((root / source).parent / target.partition("#")[0]).is_file():
                return match.group(0)
            resolved = resolve(root, target, source)
            if resolved is None:
                unresolved.append(f"{source}: {target}")
                return match.group(0)
            return f"]({resolved}{title})"

        out.append(LINK.sub(replace, line))
    return "".join(out)


def main():
    default_root = pathlib.Path(__file__).resolve().parent.parent / "docs"
    root = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else default_root
    unresolved, changed = [], 0

    for path in sorted(root.rglob("*.md")):
        source = path.relative_to(root).as_posix()
        # README.md documents the vendoring itself. It is not an upstream page,
        # and it quotes the very markup this script rewrites.
        if source == "README.md":
            continue
        text = original = path.read_text(encoding="utf-8")

        text = CALLOUT.sub(convert_callout, text)
        text = PRE_CODE.sub(convert_pre_code, text)
        text = TWEET.sub(convert_tweet, text)
        for pattern, replacement in INLINE_SUBS:
            text = pattern.sub(replacement, text)
        text = rewrite_links(root, text, source, unresolved)
        text = re.sub(r"\n{3,}\Z", "\n", text)

        if text != original:
            path.write_text(text, encoding="utf-8", newline="\n")
            changed += 1

    print(f"normalized {changed} file(s)")
    if unresolved:
        print("unresolved links:", file=sys.stderr)
        for item in unresolved:
            print(f"  {item}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
