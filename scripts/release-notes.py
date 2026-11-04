#!/usr/bin/env python3
"""Prints one version's section of CHANGELOG.md, as Markdown or (--html) as
HTML for Sparkle's update window. Exits with status 1 if the changelog has
no section for that version.

Usage: python3 -I scripts/release-notes.py [--html] <version>

The changelog uses a small Markdown subset: `###` headings, `- ` bullets
(wrapped lines indented by two spaces), paragraphs, `code` and
[links](https://example.com).
"""

import html
import re
import sys
from pathlib import Path


def section(changelog: str, version: str) -> str | None:
    """The body under `## [version]`, up to the next `## ` heading."""
    lines = changelog.splitlines()
    heading = f"## [{version}]"
    for start, line in enumerate(lines):
        if line.startswith(heading):
            body = []
            for line in lines[start + 1 :]:
                if line.startswith("## "):
                    break
                body.append(line)
            return "\n".join(body).strip()
    return None


def inline(text: str) -> str:
    text = html.escape(text, quote=False)
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    return re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", r'<a href="\2">\1</a>', text)


def to_html(markdown: str) -> str:
    out: list[str] = []
    block: list[str] = []  # lines of the current paragraph or bullet
    kind = None  # "p" or "li"
    in_list = False

    def flush() -> None:
        nonlocal kind
        if block:
            out.append(f"<{kind}>{inline(' '.join(block))}</{kind}>")
            block.clear()
        kind = None

    def close_list() -> None:
        nonlocal in_list
        if in_list:
            out.append("</ul>")
            in_list = False

    for line in markdown.splitlines():
        if not line.strip():
            flush()
        elif line.startswith("### "):
            flush()
            close_list()
            out.append(f"<h3>{inline(line[4:].strip())}</h3>")
        elif line.startswith("- "):
            flush()
            if not in_list:
                out.append("<ul>")
                in_list = True
            kind = "li"
            block.append(line[2:].strip())
        elif line.startswith("  ") and kind == "li":
            block.append(line.strip())
        else:
            if kind != "p":
                flush()
                close_list()
                kind = "p"
            block.append(line.strip())
    flush()
    close_list()
    return "\n".join(out)


def main() -> int:
    args = sys.argv[1:]
    as_html = "--html" in args
    args = [a for a in args if a != "--html"]
    if len(args) != 1:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    changelog = (Path(__file__).resolve().parent.parent / "CHANGELOG.md").read_text()
    body = section(changelog, args[0])
    if not body:
        print(f"no section for {args[0]} in CHANGELOG.md", file=sys.stderr)
        return 1
    print(to_html(body) if as_html else body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
