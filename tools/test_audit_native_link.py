"""Synthetic PE/MAP tests; never execute a native binary."""
import io
from pathlib import Path
import struct
import tempfile
import unittest

if __package__:
    from .audit_native_link import PE, audit, parse_map, risk_family
else:
    from audit_native_link import PE, audit, parse_map, risk_family


def fixture(symbol=b"espeak_Initialize", forwarder=False):
    data = bytearray(0x600)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0x8664, 1, 0x12345678, 0, 0, 240, 0x22)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<Q", data, optional + 24, 0x140000000)
    struct.pack_into("<I", data, optional + 108, 16)
    struct.pack_into("<II", data, optional + 112, 0x1100, 0x100)
    section = optional + 240
    data[section:section + 8] = b".text\0\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x400, 0x1000, 0x400, 0x200)
    struct.pack_into("<I", data, section + 36, 0x60000020)
    data[0x220:0x240] = bytes(range(32))
    struct.pack_into("<IIHHIIIIIII", data, 0x300, 0, 0, 0, 0, 0, 1, 1, 1, 0x1140, 0x1144, 0x1148)
    struct.pack_into("<IIH", data, 0x340, 0x1180 if forwarder else 0x1020, 0x1160, 0)
    data[0x360:0x360 + len(symbol) + 1] = symbol + b"\0"
    data[0x380:0x38D] = b"other.espeak\0"
    return bytes(data)


def map_text(stamp="12345678", va="0000000140001020", segment="0001"):
    return f""" neo
 Timestamp is {stamp}
 Preferred load address is 0000000140000000
 Address Publics by Value Rva+Base Lib:Object
 {segment}:00000020 espeak_Initialize {va} f lib:espeak_api.obj
 entry point at 0001:00000020
"""


class NativeLinkTests(unittest.TestCase):
    def run_audit(self, data, mapping=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            exe = root / "test.exe"
            exe.write_bytes(data)
            path = None
            if mapping is not None:
                path = root / "test.map"
                path.write_text(mapping, encoding="utf-8")
            return audit(exe, path)

    def test_exports_are_real_mapped_bytes(self):
        report = self.run_audit(fixture())
        self.assertEqual(report["verdict"], "retained-risk-markers")
        row = report["export_findings"][0]
        self.assertEqual(row["file_offset"], 0x220)
        self.assertEqual(row["sample_hex"], bytes(range(32)).hex())

    def test_forwarder_is_not_local_code(self):
        report = self.run_audit(fixture(forwarder=True))
        self.assertEqual(report["verdict"], "inconclusive")
        self.assertEqual(report["export_findings"][0]["forwarder"], "other.espeak")

    def test_map_agrees_with_export(self):
        report = self.run_audit(fixture(), map_text())
        self.assertTrue(report["map_findings"][0]["export_rva_matches"])
        self.assertTrue(report["map_findings"][0]["mapped"])

    def test_wrong_map_does_not_override_independent_pe_evidence(self):
        report = self.run_audit(fixture(), map_text(stamp="11111111"))
        self.assertFalse(report["map_header_consistent"])
        self.assertFalse(report["map_findings"][0]["mapped"])
        self.assertEqual(report["verdict"], "retained-risk-markers")

    def test_bad_segment_rejected(self):
        report = self.run_audit(fixture(b"unrelated"), map_text(segment="0002"))
        self.assertFalse(report["map_findings"][0]["mapped"])
        self.assertEqual(report["verdict"], "inconclusive")

    def test_export_map_disagreement_rejected(self):
        text = map_text(va="0000000140001030").replace("00000020 espeak", "00000030 espeak")
        report = self.run_audit(fixture(), text)
        self.assertFalse(report["map_findings"][0]["mapped"])

    def test_missing_marker_never_passes(self):
        report = self.run_audit(fixture(b"unrelated"))
        self.assertEqual(report["verdict"], "inconclusive")

    def test_input_and_discarded_rows_are_not_retained_symbols(self):
        text = "Searching espeak-ng.lib\nLoaded espeak_api.obj\n" + map_text()
        text += "Discarded\n 0001:00000030 espeak_Synth 0000000140001030 f lib:espeak_api.obj\n"
        parsed = parse_map(io.StringIO(text))
        self.assertEqual(len(parsed["rows"]), 1)

    def test_symbol_containing_summary_does_not_end_table(self):
        text = map_text().replace(" 0001:00000020 espeak", " 0001:00000010 ?Summary@Other@@ 0000000140001010 f other.obj\n 0001:00000020 espeak")
        rows = parse_map(io.StringIO(text))["rows"]
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[1]["symbol"], "espeak_Initialize")

    def test_icf_alias_is_preserved_not_counted_as_unique_function(self):
        text = map_text().replace("entry point", " 0001:00000020 espeak_Cancel 0000000140001020 f i lib:espeak_api.obj\n entry point")
        rows = parse_map(io.StringIO(text))["rows"]
        self.assertEqual(rows[0]["va"], rows[1]["va"])
        self.assertIn("i", rows[1]["flags"])

    def test_bss_is_not_file_backed_evidence(self):
        image = PE(fixture())
        image.sections[0]["virtual_size"] = 0x800
        with self.assertRaises(ValueError):
            image.sample(0x1500)

    def test_truncated_pe_rejected(self):
        with self.assertRaises(ValueError):
            PE(fixture()[:0x300])

    def test_unrelated_named_pipe_not_piper(self):
        self.assertIsNone(risk_family("RustNamedPipeRead"))
        self.assertIsNone(risk_family("phoneme", "other:table.obj"))
        self.assertEqual(risk_family("espeak_ng_Initialize"), "espeak")
        self.assertEqual(risk_family("?phonemize_eSpeak@piper@@signature"), "piper")


if __name__ == "__main__":
    unittest.main()
