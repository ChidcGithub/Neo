"""Offline PE/MAP evidence for Sherpa/eSpeak/Piper (stdlib only).

Never runs the inspected program, builds, downloads, or issues a compliance pass.
Exit 1: retained risk markers; 2: inconclusive/no markers (NOT a clean bill).
Only writes JSON below target/license-audit/sherpa in this checkout.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import struct

ROOT = Path(__file__).resolve().parents[1]
OUTPUT_ROOT = ROOT / "target/license-audit/sherpa"
MAP_ROW = re.compile(
    r"^\s+([0-9A-Fa-f]{4}):([0-9A-Fa-f]{8,16})\s+(\S+)\s+"
    r"([0-9A-Fa-f]{8,16})\s+(.*?)\s*$"
)
ASR_VAD = {"SherpaOnnxCreateOfflineRecognizer", "SherpaOnnxCreateVoiceActivityDetector"}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def identity(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path.resolve()), "size": path.stat().st_size, "sha256": digest}


def risk_family(symbol, origin=""):
    # Avoid substring matches such as Rust NamedPipeRead and generic phoneme tables.
    if re.match(r"^_?espeak(?:_ng)?_", symbol, re.I):
        return "espeak"
    if re.match(r"^\?phonemize_(?:eSpeak|codepoints)@piper@@", symbol):
        return "piper"
    if re.search(r"(?:^|[:/\\])(?:piper-phonemize-lexicon|phonemize|espeak_api)\.obj$", origin, re.I):
        return "phonemizer-object"
    return None


class PE:
    """Read x64 PE32+ sections/exports; do not load or execute the image."""

    def __init__(self, data):
        self.data = data
        if self.take(0, 2) != b"MZ":
            raise ValueError("Not an MZ image")
        pe = self.unpack("<I", 0x3C)[0]
        if self.take(pe, 4) != b"PE\0\0":
            raise ValueError("Missing PE signature")
        machine, count, self.timestamp, _, _, optional_size, _ = self.unpack("<HHIIIHH", pe + 4)
        optional = pe + 24
        if machine != 0x8664 or self.unpack("<H", optional)[0] != 0x20B:
            raise ValueError("Only x64 PE32+ is supported")
        if optional_size < 120 or not 1 <= count <= 96:
            raise ValueError("Invalid PE optional header/section count")
        self.image_base = self.unpack("<Q", optional + 24)[0]
        self.export_rva, self.export_size = self.unpack("<II", optional + 112)
        if self.unpack("<I", optional + 108)[0] < 1:
            self.export_rva = self.export_size = 0
        self.sections = []
        for index in range(count):
            offset = optional + optional_size + index * 40
            name = self.take(offset, 8).split(b"\0")[0].decode("ascii", "replace")
            virtual_size, rva, raw_size, raw_offset = self.unpack("<IIII", offset + 8)
            flags = self.unpack("<I", offset + 36)[0]
            self.take(raw_offset, raw_size)
            self.sections.append(dict(name=name, rva=rva, virtual_size=virtual_size,
                                      raw_size=raw_size, raw_offset=raw_offset, flags=flags))

    def take(self, offset, size):
        if offset < 0 or size < 0 or offset + size > len(self.data):
            raise ValueError("Truncated/out-of-range PE data")
        return self.data[offset:offset + size]

    def unpack(self, fmt, offset):
        return struct.unpack(fmt, self.take(offset, struct.calcsize(fmt)))

    def location(self, rva, size=1):
        for section in self.sections:
            delta = rva - section["rva"]
            if 0 <= delta and delta + size <= min(section["virtual_size"], section["raw_size"]):
                return section, section["raw_offset"] + delta
        raise ValueError(f"RVA 0x{rva:x} is not file-backed mapped section data")

    def at_rva(self, rva, size):
        _, offset = self.location(rva, size)
        return self.take(offset, size)

    def cstring(self, rva):
        section, offset = self.location(rva)
        limit = min(section["raw_offset"] + min(section["virtual_size"], section["raw_size"]),
                    offset + 65536)
        end = self.data.find(b"\0", offset, limit)
        if end < 0:
            raise ValueError("Unterminated PE string")
        return self.data[offset:end].decode("ascii", "strict")

    def sample(self, rva):
        section, offset = self.location(rva)
        size = min(32, min(section["virtual_size"], section["raw_size"]) - (rva - section["rva"]))
        data = self.take(offset, size)
        return {"section": section["name"], "executable": bool(section["flags"] & 0x20000000),
                "file_offset": offset, "sample_hex": data.hex(), "sample_sha256": sha256(data)}

    def exports(self):
        if not self.export_rva:
            return []
        fields = struct.unpack("<IIHHIIIIIII", self.at_rva(self.export_rva, 40))
        _, _, _, _, _, base, functions, names, eat, ent, eot = fields
        if names > 1000000 or functions > 1000000:
            raise ValueError("Unreasonable export count")
        result = []
        for index in range(names):
            name_rva = struct.unpack("<I", self.at_rva(ent + index * 4, 4))[0]
            ordinal = struct.unpack("<H", self.at_rva(eot + index * 2, 2))[0]
            if ordinal >= functions:
                raise ValueError("Export ordinal out of bounds")
            rva = struct.unpack("<I", self.at_rva(eat + ordinal * 4, 4))[0]
            symbol = self.cstring(name_rva)
            forwarder = self.export_rva <= rva < self.export_rva + self.export_size
            result.append({"symbol": symbol, "ordinal": base + ordinal, "rva": rva,
                           "forwarder": self.cstring(rva) if forwarder else None})
        return result


def parse_map(lines):
    """Only address-bearing public/static rows, never input/discarded listings."""
    result = {"image_base": None, "timestamp": None, "rows": []}
    active = False
    for number, line in enumerate(lines, 1):
        base = re.search(r"Preferred load address is ([0-9a-fA-F]+)", line)
        stamp = re.search(r"Timestamp is ([0-9a-fA-F]+)", line)
        if base:
            result["image_base"] = int(base[1], 16)
        if stamp:
            result["timestamp"] = int(stamp[1], 16)
        if "Publics by Value" in line or line.strip() == "Static symbols":
            active = True
            continue
        if re.match(r"^\s*(?:entry point at\b|Discarded\b|Exports\s*$|Summary\s*$)", line, re.I):
            active = False
        match = MAP_ROW.match(line) if active else None
        if not match:
            continue
        segment, offset, symbol, va, rest = match.groups()
        tokens = rest.split()
        flags = []
        while tokens and tokens[0] in {"f", "i"}:
            flags.append(tokens.pop(0))
        result["rows"].append({"line": number, "segment": int(segment, 16),
                               "offset": int(offset, 16), "symbol": symbol,
                               "va": int(va, 16), "flags": flags, "origin": " ".join(tokens)})
    return result


def audit(exe_path, map_path=None):
    image = PE(exe_path.read_bytes())
    exports = image.exports()
    report = {"schema": 1, "exe": identity(exe_path), "image_base": image.image_base,
              "pe_timestamp": image.timestamp, "sections": image.sections,
              "named_export_count": len(exports), "export_findings": [], "map_findings": [],
              "asr_vad": [], "errors": [], "verdict": "inconclusive",
              "limits": ["No legal/compliance pass, including when no marker is found.",
                         "MAP timestamp/base agreement is a consistency check, not cryptographic provenance.",
                         "32-byte samples are not function sizes; ICF may alias multiple symbols.",
                         "A retained symbol does not prove the application calls it at runtime."]}
    by_name = {entry["symbol"]: entry for entry in exports}
    for entry in exports:
        family = risk_family(entry["symbol"])
        if not family:
            continue
        row = dict(entry, family=family)
        if entry["rva"] and not entry["forwarder"]:
            row.update(image.sample(entry["rva"]))
        report["export_findings"].append(row)
    if map_path:
        with map_path.open(encoding="utf-8", errors="replace") as stream:
            mapping = parse_map(stream)
        report["map"] = identity(map_path)
        consistent = mapping["image_base"] == image.image_base and mapping["timestamp"] == image.timestamp
        report["map_header_consistent"] = consistent
        report["map_symbol_count"] = len(mapping["rows"])
        if not consistent:
            report["errors"].append("MAP/PE timestamp or preferred base mismatch (or missing header)")
        for row in mapping["rows"]:
            family = risk_family(row["symbol"], row["origin"])
            if not family and row["symbol"] not in ASR_VAD:
                continue
            row = dict(row, family=family, rva=row["va"] - image.image_base)
            try:
                if not consistent:
                    raise ValueError("Unpaired MAP")
                sample = image.sample(row["rva"])
                section = image.sections[row["segment"] - 1] if 1 <= row["segment"] <= len(image.sections) else None
                if section is None or section["rva"] + row["offset"] != row["rva"]:
                    raise ValueError("MAP segment/offset disagrees with PE RVA")
                row.update(sample)
                exported = by_name.get(row["symbol"])
                if exported:
                    if exported["rva"] != row["rva"] or exported["forwarder"]:
                        raise ValueError("MAP/export RVA disagreement")
                    row["export_rva_matches"] = True
                row["mapped"] = True
            except ValueError as exc:
                row["mapped"] = False
                row["error"] = str(exc)
            report["map_findings" if family else "asr_vad"].append(row)
    positive_exports = any(r.get("executable") and not r["forwarder"] for r in report["export_findings"])
    positive_map = any(r.get("mapped") and r.get("executable") for r in report["map_findings"])
    if positive_exports or positive_map:
        report["verdict"] = "retained-risk-markers"
    report["map_origin_counts"] = dict(Counter(r["origin"] for r in report["map_findings"] if r.get("mapped")))
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--map", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)
    output = args.output.resolve()
    if not output.is_relative_to(OUTPUT_ROOT.resolve()):
        parser.error("Output must be below target/license-audit/sherpa")
    if output in {args.exe.resolve(), args.map.resolve() if args.map else None}:
        parser.error("Output cannot overwrite an input")
    try:
        report = audit(args.exe, args.map)
    except (OSError, ValueError, struct.error) as exc:
        report = {"schema": 1, "verdict": "inconclusive", "errors": [str(exc)]}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(report["verdict"] + ": " + str(output))
    return 1 if report["verdict"] == "retained-risk-markers" else 2


if __name__ == "__main__":
    raise SystemExit(main())
