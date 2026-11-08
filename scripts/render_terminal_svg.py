#!/usr/bin/env python3
"""Render a terminal session (plain text) as an SVG image for the docs.

Usage: scripts/render_terminal_svg.py <session.txt> <out.svg> [--title T] [--wrap N] [--indent N]

Lines starting with "$ " are commands; everything else is output. Output
lines are colored by a few rules that match sediros-audit and the CLI
(check marks, warnings, failures). Lines longer than --wrap columns are
wrapped at a space and continued at --indent columns, so the image stays
readable when GitHub scales it down.
"""
import argparse
import html
import re

FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
SIZE = 13
CHAR_W = 8.6    # advance width of a 13px monospace glyph, with some slack
LINE_H = 19
PAD_X, PAD_TOP, PAD_BOTTOM, BAR_H = 16, 14, 16, 30

C = {
    "bg": "#1e1f24", "bar": "#2a2b31", "fg": "#d8d8dc", "dim": "#8b8c94",
    "cmd": "#ffffff", "prompt": "#9fd3b5", "ok": "#7ccf9a", "warn": "#e8c06a",
    "fail": "#ef7b7b", "title": "#a1a1a6",
}


BOLD = ' font-weight="bold"'


def nbsp(text):
    # Viewers collapse runs of spaces despite xml:space; no-break spaces keep the columns.
    return html.escape(text).replace(" ", "\u00a0")


def wrap(line, width, indent):
    out = []
    while len(line) > width:
        cut = line.rfind(" ", indent + 1, width + 1)
        if cut <= indent:
            cut = width
        out.append(line[:cut].rstrip())
        line = " " * indent + line[cut:].lstrip()
    out.append(line)
    return out


def spans(line):
    """Split a line into (text, color, bold) segments."""
    if line.startswith("$ "):
        return [("$ ", C["prompt"], True), (line[2:], C["cmd"], True)]
    m = re.match(r"^(\s*)([✓·!✗])(\s+\S+)(.*)$", line)
    if m:
        mark = {"✓": C["ok"], "·": C["dim"], "!": C["warn"], "✗": C["fail"]}[m.group(2)]
        return [(m.group(1), C["fg"], False), (m.group(2), mark, True),
                (m.group(3), C["fg"], True), (m.group(4), C["fg"], False)]
    m = re.match(r"^(\d+ passed)(, \d+ warnings?)(, \d+ failed)$", line)
    if m:
        return [(m.group(1), C["ok"], True), (m.group(2), C["warn"] if not m.group(2).startswith(", 0") else C["fg"], False),
                (m.group(3), C["fail"] if not m.group(3).startswith(", 0") else C["fg"], False)]
    if line.startswith("==> "):
        return [("==> ", C["prompt"], True), (line[4:], C["fg"], True)]
    if line.startswith("ALL DATA ON "):
        return [(line, C["fail"], True)]
    if re.match(r"^(Next:|Waiting)", line):
        return [(line, C["dim"], False)]
    return [(line, C["fg"], False)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("session")
    ap.add_argument("out")
    ap.add_argument("--title", default="")
    ap.add_argument("--wrap", type=int, default=100)
    ap.add_argument("--indent", type=int, default=2)
    a = ap.parse_args()

    with open(a.session, encoding="utf-8") as f:
        raw = [l.rstrip("\n").rstrip() for l in f]
    while raw and not raw[-1]:
        raw.pop()
    lines = []
    for l in raw:
        # Indented lines (audit rows) continue at --indent, others at the margin.
        indent = 2 if l.startswith("$ ") else a.indent if l[:1].isspace() else 0
        lines += wrap(l, a.wrap, indent)

    cols = max(max(len(l) for l in lines), 40)
    width = round(PAD_X * 2 + cols * CHAR_W)
    height = BAR_H + PAD_TOP + len(lines) * LINE_H + PAD_BOTTOM

    svg = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(a.title or "terminal")}">',
        f'<rect width="{width}" height="{height}" rx="10" fill="{C["bg"]}"/>',
        f'<path d="M0 10a10 10 0 0 1 10-10h{width - 20}a10 10 0 0 1 10 10v{BAR_H - 10}h-{width}z" fill="{C["bar"]}"/>',
    ]
    for i, col in enumerate(("#ff5f57", "#febc2e", "#28c840")):
        svg.append(f'<circle cx="{18 + i * 20}" cy="{BAR_H / 2}" r="6" fill="{col}"/>')
    if a.title:
        svg.append(f'<text x="{width / 2}" y="{BAR_H / 2 + 4}" text-anchor="middle" font-family="{FONT}" font-size="12" fill="{C["title"]}">{html.escape(a.title)}</text>')
    svg.append(f'<g font-family="{FONT}" font-size="{SIZE}" xml:space="preserve">')
    y = BAR_H + PAD_TOP + SIZE
    for l in lines:
        parts = "".join(
            f'<tspan fill="{col}"{BOLD if bold else ""}>{nbsp(t)}</tspan>'
            for t, col, bold in spans(l) if t)
        svg.append(f'<text x="{PAD_X}" y="{y}">{parts}</text>')
        y += LINE_H
    svg.append("</g></svg>")
    with open(a.out, "w", encoding="utf-8") as f:
        f.write("\n".join(svg) + "\n")
    print(f"{a.out}: {len(lines)} lines, {width}x{height}")


if __name__ == "__main__":
    main()
