#!/usr/bin/env python3

import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import shutil
import subprocess
import sys
import types
import xml.etree.ElementTree as ElementTree


FONT_SHA256 = "420c80f80c2e1f094b91ae993799c93dcba4afc7372a0ccfe8a29d53c64593a7"
REPOSITORY = Path(__file__).resolve().parent.parent
ASSETS = REPOSITORY / "crates/gpui_icons/assets"
COORDINATE_SIZE = 24.0
# Glyphs fill their em box, as in Cursor's font. Zed's icon boxes are larger than Cursor's, so
# `ui::Icon` draws glyphs at 0.8 of its box instead of every glyph carrying that inset.
DISPLAY_VIEW_BOX = [0.0, 0.0, COORDINATE_SIZE, COORDINATE_SIZE]


def load_font(path, node):
    if importlib.util.find_spec("brotli") is None and importlib.util.find_spec("brotlicffi") is None:
        # FontTools only needs decompression; Node's built-in decoder avoids a second install.
        def decompress(data):
            return subprocess.run(
                [node, "-e", "process.stdout.write(require('node:zlib').brotliDecompressSync(require('node:fs').readFileSync(0)))"],
                input=data,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            ).stdout

        sys.modules["brotli"] = types.SimpleNamespace(decompress=decompress)
    from fontTools.ttLib import TTFont

    return TTFont(path, recalcTimestamp=False)


def number(value):
    value = round(value, 12)
    return str(int(value)) if value == int(value) else format(value, ".12g")


def glyph_path(glyph_set, glyph_name, scale, baseline):
    from fontTools.pens.basePen import BasePen

    class PathPen(BasePen):
        def __init__(self):
            super().__init__(glyph_set)
            self.commands = []
            self.open = False

        def command(self, command, *points):
            self.commands.append(command + " ".join(number(value) for point in points for value in point))

        def point(self, point):
            return [point[0] * scale, baseline - point[1] * scale]

        def _moveTo(self, point):
            if self.open:
                raise ValueError("Unclosed font contour")
            self.open = True
            self.command("M", self.point(point))

        def _lineTo(self, point):
            self.command("L", self.point(point))

        def _qCurveToOne(self, control, point):
            self.command("Q", self.point(control), self.point(point))

        def _curveToOne(self, first, second, point):
            self.command("C", self.point(first), self.point(second), self.point(point))

        def _closePath(self):
            self.command("Z")
            self.open = False

        def _endPath(self):
            raise ValueError("Cursor glyph contains an open contour")

    pen = PathPen()
    glyph_set[glyph_name].draw(pen)
    if pen.open:
        raise ValueError(f"Glyph {glyph_name} did not close its final contour")
    return " ".join(pen.commands)


def display_svg(contents):
    view_box = " ".join(number(value) for value in DISPLAY_VIEW_BOX)
    result, count = re.subn(r'viewBox="[^"]*"', f'viewBox="{view_box}"', contents, count=1)
    if count != 1:
        raise ValueError("Icon SVG has no viewBox to inset")
    return result


def append_bytes(payload, data):
    offset = len(payload)
    payload.extend(data)
    return {"offset": offset, "length": len(data)}


def validate_mappings(names):
    mappings = json.loads((ASSETS / "mappings.json").read_text())
    for source in ("legacy", "kit"):
        for path, reference in mappings[source].items():
            if reference["name"] not in names:
                raise ValueError(f"Unknown mapped icon {source}:{path}: {reference}")
            if reference["variant"] not in ("outline", "filled"):
                raise ValueError(f"Unknown mapped variant {source}:{path}: {reference}")
    return mappings


def read_drawn(names, mappings):
    sources = json.loads((ASSETS / "drawn-sources.json").read_text())
    results = []
    known_paths = set()
    paint_attributes = {"fill", "stroke", "stroke-width", "fill-rule", "stroke-linecap", "stroke-linejoin", "fill-opacity", "stroke-opacity"}
    geometry_tags = {"path", "line", "circle", "ellipse", "rect", "polyline", "polygon"}
    for source in sources:
        name = source["name"]
        if name in names:
            raise ValueError(f"Drawn icon shadows a Cursor icon: {name}")
        svg_path = f"drawn/{name}.svg"
        for path in source["paths"]:
            if path in known_paths:
                raise ValueError(f"Duplicate drawn asset path: {path}")
            known_paths.add(path)
            # Mappings resolve before drawn paths, so a mapped path would never reach the drawing.
            for prefix, table in (("icons/", "legacy"), ("kit/icons/", "kit")):
                if path.startswith(prefix) and path.removeprefix(prefix).removesuffix(".svg") in mappings[table]:
                    raise ValueError(f"Drawn asset path is already mapped: {path}")
        contents = (ASSETS / svg_path).read_bytes()
        root = ElementTree.fromstring(contents)
        viewbox = [float(value) for value in root.attrib.get("viewBox", "").replace(",", " ").split()]
        if viewbox != [0.0, 0.0, COORDINATE_SIZE, COORDINATE_SIZE]:
            raise ValueError(f"Drawn icons use the 24-unit Cursor grid: {svg_path}")
        nodes = []

        def visit(element, inherited, inherited_opacity):
            tag = element.tag.rsplit("}", 1)[-1]
            if tag in ("title", "desc"):
                return
            if tag not in geometry_tags | {"svg", "g"}:
                raise ValueError(f"Unsupported drawn SVG element {tag}: {svg_path}")
            if any(attribute in element.attrib for attribute in ("style", "transform", "clip-path", "mask", "filter")):
                raise ValueError(f"Drawn SVG requires an unsupported paint operation: {svg_path}")
            paint = {**inherited, **{key: value for key, value in element.attrib.items() if key in paint_attributes}}
            opacity = inherited_opacity * float(element.attrib.get("opacity", "1"))
            if not math.isfinite(opacity) or opacity < 0 or opacity > 1:
                raise ValueError(f"Invalid drawn SVG opacity: {svg_path}")
            if tag in geometry_tags and opacity > 0:
                nodes.append([tag, {**element.attrib, **paint, "opacity": opacity}])
            for child in element:
                visit(child, paint, opacity)

        visit(root, {"fill": "black", "stroke": "none", "stroke-width": "1"}, 1.0)
        if not nodes:
            raise ValueError(f"Drawn icon has no visible geometry: {svg_path}")
        results.append({**source, "svg_contents": contents.decode(), "viewbox": viewbox, "nodes": nodes})
    return results


def write_icon_names(source_icons):
    variants = set()
    declarations = []
    aliases = {}
    for entry in source_icons:
        name = entry["name"]
        variant = "".join(part[:1].upper() + part[1:] for part in re.split("[^a-zA-Z0-9]+", name))
        if variant[:1].isdigit():
            variant = "Icon" + variant
        if variant in variants:
            variant += "Alias"
            suffix = 2
            while variant in variants:
                variant = variant.removesuffix(str(suffix - 1)) + str(suffix)
                suffix += 1
            aliases[name] = variant
        variants.add(variant)
        declarations.append(f'    {variant} => "{name}",')
    (ASSETS / "icon_names.rs").write_text("icon_names! {\n" + "\n".join(declarations) + "\n}\n")
    return aliases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--font", type=Path, default=Path("/Applications/Cursor.app/Contents/Resources/app/out/media/cursor-icons-16.woff2"))
    parser.add_argument("--node", default=shutil.which("node"))
    arguments = parser.parse_args()
    font_bytes = arguments.font.read_bytes()
    if hashlib.sha256(font_bytes).hexdigest() != FONT_SHA256:
        raise ValueError("Font hash differs from the reviewed Cursor source; re-extract and review its catalog first")
    source_icons = json.loads((ASSETS / "source-icons.json").read_text())
    rtl_names = set(json.loads((ASSETS / "rtl-icons.json").read_text()))
    if len(source_icons) != 818 or len({entry["name"] for entry in source_icons}) != 818:
        raise ValueError("Expected exactly 818 unique Cursor icon names")
    mappings = validate_mappings({entry["name"] for entry in source_icons})
    drawn = read_drawn({entry["name"] for entry in source_icons}, mappings)
    enum_aliases = write_icon_names(source_icons)
    font = load_font(arguments.font, arguments.node)
    em_units = font["head"].unitsPerEm
    scale = COORDINATE_SIZE / em_units
    baseline = font["hhea"].ascent * scale
    glyph_set = font.getGlyphSet()
    cmap = font.getBestCmap()
    codepoints = sorted({entry[f"{variant}_codepoint"] for entry in source_icons for variant in ("outline", "filled")})
    if len(codepoints) != 1008:
        raise ValueError("Expected 1008 distinct mapped glyph codepoints")
    payload = bytearray()
    records = []
    for codepoint in codepoints:
        glyph_name = cmap[codepoint]
        path = glyph_path(glyph_set, glyph_name, scale, baseline)
        advance, bearing = font["hmtx"].metrics[glyph_name]
        svg = display_svg(f'<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" overflow="visible"><path d="{path}" fill="currentColor" fill-rule="nonzero"/></svg>')
        nodes = [["path", {"d": path, "fill": "currentColor", "stroke": "none", "fill-rule": "nonzero"}]]
        records.append({
            "id": codepoint,
            "source": "cursor",
            "codepoint": codepoint,
            "glyph_name": glyph_name,
            "em_units": em_units,
            "baseline": baseline,
            "advance": advance * scale,
            "advance_units": advance,
            "left_side_bearing": bearing * scale,
            "left_side_bearing_units": bearing,
            "svg": append_bytes(payload, svg.encode()),
            "view_box": [0.0, 0.0, COORDINATE_SIZE, COORDINATE_SIZE],
            "nodes": append_bytes(payload, json.dumps(nodes, separators=(",", ":")).encode()),
        })
    drawn_entries = []
    for drawn_index, drawing in enumerate(drawn):
        nodes = [[tag, {key: str(value) for key, value in attributes.items()}] for tag, attributes in drawing["nodes"]]
        drawn_entries.append({
            "name": drawing["name"],
            "glyph": len(records),
            "paths": drawing["paths"],
            "derived_from": drawing.get("derived_from"),
        })
        records.append({
            "id": 0x110000 + drawn_index,
            "source": "drawn",
            "codepoint": 0,
            "glyph_name": drawing["name"],
            "em_units": em_units,
            "baseline": baseline,
            "advance": COORDINATE_SIZE,
            "advance_units": em_units,
            "left_side_bearing": 0.0,
            "left_side_bearing_units": 0,
            "svg": append_bytes(payload, display_svg(drawing["svg_contents"]).encode()),
            "view_box": drawing["viewbox"],
            "nodes": append_bytes(payload, json.dumps(nodes, separators=(",", ":")).encode()),
        })
    indices = {codepoint: index for index, codepoint in enumerate(codepoints)}
    entries = [{
        "name": entry["name"],
        "outline": indices[entry["outline_codepoint"]],
        "filled": indices[entry["filled_codepoint"]],
        "has_explicit_filled": entry["has_explicit_filled"],
        "mirrored_in_rtl": entry["name"] in rtl_names,
    } for entry in source_icons]
    metadata = {
        "version": 4,
        "coordinate_size": COORDINATE_SIZE,
        "em_units": em_units,
        "baseline": baseline,
        "ascent": font["hhea"].ascent,
        "descent": font["hhea"].descent,
        "line_gap": font["hhea"].lineGap,
        "font_sha256": FONT_SHA256,
        "display_view_box": DISPLAY_VIEW_BOX,
        "entries": entries,
        "drawn_entries": drawn_entries,
        "glyphs": records,
    }
    (ASSETS / "glyphs.bin").write_bytes(payload)
    (ASSETS / "catalog.json").write_text(json.dumps(metadata, separators=(",", ":")) + "\n")
    manifest = {
        "font": {"filename": arguments.font.name, "sha256": FONT_SHA256, "bytes": len(font_bytes)},
        "source_icons_sha256": hashlib.sha256((ASSETS / "source-icons.json").read_bytes()).hexdigest(),
        "rtl_icons_sha256": hashlib.sha256((ASSETS / "rtl-icons.json").read_bytes()).hexdigest(),
        "names": len(entries),
        "glyphs": len(codepoints),
        "drawn_glyphs": len(drawn_entries),
        "total_glyphs": len(records),
        "explicit_filled": sum(entry["has_explicit_filled"] for entry in entries),
        "outline_fallback": sum(not entry["has_explicit_filled"] for entry in entries),
        "rtl_names": len(rtl_names),
        "legacy_mappings": len(mappings["legacy"]),
        "kit_mappings": len(mappings["kit"]),
        "payload_bytes": len(payload),
        "payload_sha256": hashlib.sha256(payload).hexdigest(),
        "coordinate_transform": "x = font_x * 24 / unitsPerEm; y = ascent * 24 / unitsPerEm - font_y * 24 / unitsPerEm; no bounding-box fit",
        "region": "Not generated: gpui_icons::region normalizes each glyph's nodes lazily at runtime, as upstream morphicons does.",
        "drawn": {
            "sources_sha256": hashlib.sha256((ASSETS / "drawn-sources.json").read_bytes()).hexdigest(),
            "style": "Cursor's outline style for icons the font lacks: 24-unit grid, 1.92 centerline strokes with round caps and joins, Cursor keylines, and slashes from (1.52, 1.52) to (22.48, 22.48) with a 1.12 clear gap.",
            "lucide_license": "drawn/LICENSE-LUCIDE covers the entries whose derived_from names a Lucide icon.",
        },
        "enum_aliases": enum_aliases,
    }
    (ASSETS / "source-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({key: manifest[key] for key in ("names", "glyphs", "drawn_glyphs", "explicit_filled", "outline_fallback", "rtl_names", "legacy_mappings", "kit_mappings", "payload_bytes")}, indent=2))


if __name__ == "__main__":
    main()
