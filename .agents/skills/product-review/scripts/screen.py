import base64
import codecs
import collections
import re
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFont


Cell = collections.namedtuple("Cell", [*pyte.screens.Char._fields, "dim"])
PALETTE = dict(zip(
    ["black", "red", "green", "brown", "blue", "magenta", "cyan", "white",
     "brightblack", "brightred", "brightgreen", "brightbrown", "brightblue",
     "brightmagenta", "brightcyan", "brightwhite"],
    ["000000", "cd0000", "00cd00", "cdcd00", "0000ee", "cd00cd", "00cdcd", "e5e5e5",
     "7f7f7f", "ff0000", "00ff00", "ffff00", "5c5cff", "ff00ff", "00ffff", "ffffff"],
))


class Screen(pyte.Screen):
    def reset(self):
        super().reset()
        self.cursor.attrs = self.default_char

    @property
    def default_char(self):
        return Cell(*super().default_char, False)

    def select_graphic_rendition(self, *attrs):
        dim = self.cursor.attrs.dim
        remaining = list(attrs or (0,))
        while remaining:
            attr = remaining.pop(0)
            if attr in (0, 22):
                dim = False
            elif attr == 2:
                dim = True
            elif attr in (38, 48) and remaining:
                kind = remaining.pop(0)
                remaining = remaining[3 if kind == 2 else 1:]
        super().select_graphic_rendition(*attrs)
        self.cursor.attrs = self.cursor.attrs._replace(dim=dim)

    def write_process_input(self, data):
        if hasattr(self, "reply"):
            self.reply(data.encode())


class Capture:
    def __init__(self, cols, rows, reply=lambda data: None):
        self.screen = Screen(cols, rows)
        self.screen.reply = reply
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.clipboard = []

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            start = self.pending.find("\x1b]")
            if start < 0:
                end = len(self.pending) - int(self.pending.endswith("\x1b"))
                self.stream.feed(self.pending[:end])
                self.pending = self.pending[end:]
                break
            self.stream.feed(self.pending[:start])
            self.pending = self.pending[start:]
            match = re.search("\x07|\x1b\\\\", self.pending)
            if match is None:
                if len(self.pending) > 2_000_000:
                    raise ValueError("Terminal control sequence exceeded capture limit")
                break
            payload = self.pending[2:match.start()]
            if payload.startswith("52;"):
                parts = payload.split(";", 2)
                if len(parts) == 3 and parts[2] != "?":
                    self.clipboard.append(base64.b64decode(parts[2], validate=True).decode("utf-8"))
            else:
                self.stream.feed(self.pending[:match.end()])
            self.pending = self.pending[match.end():]

    def snapshot(self):
        screen = self.screen
        return {
            "cols": screen.columns,
            "rows": screen.lines,
            "cursor": {"x": screen.cursor.x, "y": screen.cursor.y, "hidden": screen.cursor.hidden},
            "text": list(screen.display),
            "cells": [[screen.buffer[y][x]._asdict() for x in range(screen.columns)]
                      for y in range(screen.lines)],
        }


def font_path(explicit=None):
    candidates = [explicit] if explicit else [
        "/System/Library/Fonts/Menlo.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return str(Path(candidate).resolve())
    raise ValueError("No monospace font found; supply --font /absolute/path/to/font.ttf")


def rgb(value, default):
    value = PALETTE.get(value, value)
    if not re.fullmatch("[0-9a-fA-F]{6}", value):
        value = default
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def render_png(snapshot, destination, font):
    face = ImageFont.truetype(font, 16)
    width = round(face.getlength("M"))
    height = 22
    image = Image.new("RGB", (snapshot["cols"] * width, snapshot["rows"] * height))
    draw = ImageDraw.Draw(image)
    for y, row in enumerate(snapshot["cells"]):
        for x, cell in enumerate(row):
            bg = rgb(cell["fg"], "e5e5e5") if cell["reverse"] else rgb(cell["bg"], "151515")
            left, top = x * width, y * height
            draw.rectangle((left, top, left + width - 1, top + height - 1), fill=bg)
    for y, row in enumerate(snapshot["cells"]):
        for x, cell in enumerate(row):
            fg = rgb(cell["fg"], "e5e5e5")
            bg = rgb(cell["bg"], "151515")
            if cell["reverse"]:
                fg, bg = bg, fg
            if cell["dim"]:
                fg = tuple((a + b) // 2 for a, b in zip(fg, bg))
            left, top = x * width, y * height
            if cell["data"].strip():
                glyph = Image.new("RGBA", (width * 3, height))
                ImageDraw.Draw(glyph).text(
                    (width, 1), cell["data"], font=face, fill=fg,
                    stroke_width=int(cell["bold"]),
                )
                if cell["italics"]:
                    glyph = glyph.transform(glyph.size, Image.Transform.AFFINE, (1, -0.2, 2, 0, 1, 0))
                image.paste(glyph, (left - width, top), glyph)
            if cell["underscore"]:
                draw.line((left, top + height - 3, left + width - 1, top + height - 3), fill=fg)
            if cell["strikethrough"]:
                draw.line((left, top + height // 2, left + width - 1, top + height // 2), fill=fg)
    cursor = snapshot["cursor"]
    if not cursor["hidden"]:
        x, y = cursor["x"] * width, cursor["y"] * height
        draw.rectangle((x, y, x + width - 1, y + height - 1), outline="#eeeeee")
    image.save(destination)
