#!/usr/bin/env python3
"""Repair the CyberPlanet REA corpus so a strict PDF reader can open it.

The corpus generator (gen_corpus.js) emitted two defects in every file:

  * the document catalog points at `/Pages 2 0 R`, but object 2 is an empty
    `<< >>`; the real page tree is a later object;
  * every `/Type /Page` carries `/Parent 2 0 R`, i.e. the empty object too.

PdfCraft tolerated both and rendered the pages; pdfium (and any conforming
reader) synthesizes a blank default page instead. The content itself is fine.

This script rewrites the catalog and page parents to point at the real page
tree and rebuilds the xref, leaving every other byte of every object untouched.
Originals are never modified: it reads from one directory and writes to another.

Usage:
    python3 scripts/repair_rea_corpus.py <src_dir> <dst_dir>
"""

import os
import re
import sys

OBJ_RE = re.compile(rb"(\d+) 0 obj\b")
PAGES_RE = re.compile(rb"/Type\s*/Pages\b")
PAGE_RE = re.compile(rb"/Type\s*/Page\b(?!s)")


def objects(body: bytes):
    """Split a PDF body into (number, bytes) pairs, in file order."""
    marks = list(OBJ_RE.finditer(body))
    out = []
    for i, m in enumerate(marks):
        start = m.start()
        end = marks[i + 1].start() if i + 1 < len(marks) else len(body)
        out.append((int(m.group(1)), body[start:end]))
    return out


def repair(data: bytes) -> bytes:
    if b"xref" not in data:
        raise ValueError("no xref table found")
    body = data[: data.rfind(b"xref")]

    objs = objects(body)
    if not objs:
        raise ValueError("no objects found")

    pages_num = None
    for num, chunk in objs:
        if PAGES_RE.search(chunk):
            pages_num = num
            break
    if pages_num is None:
        raise ValueError("no /Type /Pages object")

    fixed = []
    for num, chunk in objs:
        if b"/Type /Catalog" in chunk:
            chunk = re.sub(rb"/Pages\s+\d+\s+0\s+R", b"/Pages %d 0 R" % pages_num, chunk)
        elif PAGE_RE.search(chunk):
            chunk = re.sub(rb"/Parent\s+\d+\s+0\s+R", b"/Parent %d 0 R" % pages_num, chunk)
        fixed.append((num, chunk))

    size = max(n for n, _ in fixed) + 1
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for num, chunk in fixed:
        offsets[num] = len(out)
        out += chunk
        if not out.endswith(b"\n"):
            out += b"\n"

    xref_at = len(out)
    out += b"xref\n0 %d\n" % size
    out += b"0000000000 65535 f \n"
    for num in range(1, size):
        if num in offsets:
            out += b"%010d 00000 n \n" % offsets[num]
        else:
            out += b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        size,
        xref_at,
    )
    return bytes(out)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    os.makedirs(dst, exist_ok=True)

    repaired = skipped = 0
    for name in sorted(os.listdir(src)):
        if not name.lower().endswith(".pdf"):
            continue
        raw = open(os.path.join(src, name), "rb").read()
        try:
            out = repair(raw)
        except ValueError as e:
            print(f"SKIP {name}: {e}")
            skipped += 1
            continue
        open(os.path.join(dst, name), "wb").write(out)
        repaired += 1

    print(f"repaired {repaired}, skipped {skipped} -> {dst}")
    return 0 if repaired else 1


if __name__ == "__main__":
    raise SystemExit(main())