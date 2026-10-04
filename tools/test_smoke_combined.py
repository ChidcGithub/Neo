"""Synthetic protocol fixtures only: no real GUI/capture/microphone."""
import copy
import json
import struct
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import zlib

if __package__:
    from . import smoke_combined as s
else:
    import smoke_combined as s


FAKE = r'''
import json, sys
from pathlib import Path
mode = sys.argv[1]
state = dict(app="drawing", revision_scope="document", document_id="doc:synthetic", page_id="page:synthetic", revision=0,
             visible=False, hidden_confirmed=False, has_window=False, dirty=False, closed=False, configured=False,
             desired_visible=False, effective_visible=False, close_pending=False, connected=True, window_status="no_window",
             permissions=dict(classroom_safe=True, desktop_capture_allowed=False, agent_allowed=False))
methods = ["configure", "show", "get_state", "close", "objects.apply", "objects.list", "undo", "redo", "document.save"]
def emit(value):
    print(json.dumps(dict(version=1, **value)), flush=True)
emit(dict(type="event", event="ready", data=dict(app="drawing", headless=True, has_window=False,
     max_line_bytes=65536, revision_scope="document", methods=methods)))
objects, history = [], []
for line in sys.stdin:
    frame = json.loads(line)
    method, params = frame["method"], frame["params"]
    result, error = state, None
    if method == "configure": state["configured"] = True
    elif method == "show": state["desired_visible"] = True
    elif method == "objects.apply":
        ops = params["operations"]
        if ops[0]["op"] == "delete":
            if mode == "partial": objects.pop(0)
            error = "invalid_document"
        else:
            objects = [x["object"] for x in ops]
            history = objects[:]
            state["revision"] += 1
            state["dirty"] = True
    elif method == "objects.list": result = dict(total=len(objects), objects=objects)
    elif method == "undo":
        objects = []
        state["revision"] += 1
        state["dirty"] = False
        result = dict(changed=True, state=state)
    elif method == "redo":
        objects = history[:]
        state["revision"] += 1
        state["dirty"] = True
        result = dict(changed=True, state=state)
    elif method == "document.save":
        Path(params["path"]).write_text(json.dumps(dict(document=dict(pages=[dict(objects=objects)]))))
        state["dirty"] = False
    elif method == "close":
        if params: raise RuntimeError("No discard allowed")
        if state["dirty"]: error = "unsaved_changes"
        else: state["closed"] = True
    emit(dict(type="event", event="state_changed", data=state))
    if error: emit(dict(type="response", id=frame["id"], ok=False, error=dict(code=error, message=error)))
    else: emit(dict(type="response", id=frame["id"], ok=True, result=result))
    if state["closed"]: break
'''


class SmokeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def test_response_null_is_valid_but_error_null_is_not(self):
        good = b'{"version":1,"type":"response","id":"neo:1","ok":true,"result":null}'
        self.assertIsNone(s.decode_frame(good)["result"])
        with self.assertRaises(ValueError):
            s.decode_frame(good[:-1] + b',"error":null}')

    def test_duplicate_bad_version_and_oversize_rejected(self):
        for raw in (b'{"version":1,"version":1}', b'{"version":true,"type":"event","event":"x","data":{}}',
                    b"x" * 65537, b'{"version":1,"type":"response","id":"neo: ","ok":true,"result":{}}'):
            with self.assertRaises(ValueError):
                s.decode_frame(raw)

    def test_headless_ready_not_hosted_ready(self):
        data = {"app": "drawing", "headless": True, "has_window": False, "max_line_bytes": 65536,
                "revision_scope": "document", "methods": sorted(s.REQUIRED)}
        frame = {"type": "event", "event": "ready", "data": data}
        s.validate_ready(frame, "drawing")
        data["headless"] = False
        data["has_window"] = True
        with self.assertRaises(ValueError):
            s.validate_ready(frame, "drawing")

    def test_no_capture_capability(self):
        data = {"app": "drawing", "headless": True, "has_window": False, "max_line_bytes": 65536,
                "revision_scope": "document", "methods": [*s.REQUIRED, "capture.request"]}
        with self.assertRaises(ValueError):
            s.validate_ready({"type": "event", "event": "ready", "data": data}, "drawing")

    def test_full_fake_stdio_session_and_interleaving(self):
        report = s.run_session([sys.executable, "-u", "-c", FAKE, "good"], "drawing", self.root, 3)
        self.assertTrue(report["passed"])
        self.assertFalse(report["forced_termination"])
        self.assertTrue(report["dirty_close_protected"])
        self.assertEqual(report["rpc_count"], len(report["requests"]))
        self.assertGreater(report["interleaved_events"], 0)
        self.assertEqual(report["requests"][-3:], ["document.save", "get_state", "close"])

    def test_partial_atomic_apply_fails(self):
        with self.assertRaisesRegex(ValueError, "partially applied"):
            s.run_session([sys.executable, "-u", "-c", FAKE, "partial"], "drawing", self.root, 3)

    def test_eof_and_timeout_fail_bounded(self):
        for code in ("pass", "import time; time.sleep(10)"):
            client = s.Client([sys.executable, "-c", code], self.root, .2)
            try:
                with self.assertRaises((ValueError, TimeoutError)):
                    client.next(time.monotonic() + .2)
            finally:
                client.cleanup()

    def test_stderr_is_drained_not_parsed(self):
        code = 'import sys; sys.stderr.write("x" * 100000); sys.stderr.flush(); print(\'{"version":1,"type":"event","event":"hello","data":{}}\', flush=True)'
        client = s.Client([sys.executable, "-u", "-c", code], self.root, 3)
        try:
            self.assertEqual(client.next(time.monotonic() + 3)["event"], "hello")
            client.finish()
        finally:
            client.cleanup()
        self.assertEqual(len(client.stderr), 65536)

    def test_synthetic_png_is_valid_and_has_no_external_input(self):
        png = s.tiny_png()
        self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
        offset, kinds = 8, []
        while offset < len(png):
            size = struct.unpack(">I", png[offset:offset + 4])[0]
            kind = png[offset + 4:offset + 8]
            data = png[offset + 8:offset + 8 + size]
            crc = struct.unpack(">I", png[offset + 8 + size:offset + 12 + size])[0]
            self.assertEqual(crc, zlib.crc32(kind + data))
            kinds.append(kind)
            if kind == b"IHDR":
                self.assertEqual(struct.unpack(">IIBBBBB", data), (2, 2, 8, 6, 0, 0, 0))
            if kind == b"IDAT":
                self.assertEqual(len(zlib.decompress(data)), 18)
            offset += size + 12
        self.assertEqual(kinds, [b"IHDR", b"IDAT", b"IEND"])
        self.assertEqual(offset, len(png))

    def test_resource_chunk_total_crc_and_corruption_checks(self):
        png = s.tiny_png()
        class Resource:
            def __init__(self, corrupt=None):
                self.corrupt = corrupt
            def call(self, method, params):
                self_test.assertEqual(method, "resources.read")
                off = params["offset"]
                data = list(png[off:off + params["length"]])
                end = off + len(data)
                result = dict(asset_ref="asset:synthetic", offset=off, total_bytes=len(png),
                              bytes=data, next_offset=end, eof=end == len(png), mime_type="image/png")
                if self.corrupt == "crc": result["bytes"][0] ^= 1
                elif self.corrupt == "total": result["total_bytes"] += 1
                elif self.corrupt == "offset": result["offset"] += 1
                elif self.corrupt == "next": result["next_offset"] += 1
                elif self.corrupt == "eof": result["eof"] = not result["eof"]
                elif self.corrupt == "boolean-byte": result["bytes"][0] = True
                elif self.corrupt == "empty": result["bytes"] = []
                return result
        self_test = self
        report = s.read_synthetic_resource(Resource(), "asset:synthetic", png)
        self.assertEqual(report["bytes"], len(png))
        self.assertEqual(report["crc32"], zlib.crc32(png))
        self.assertGreater(report["chunks"], 1)
        for failure in ("crc", "total", "offset", "next", "eof", "boolean-byte", "empty"):
            with self.subTest(failure=failure), self.assertRaises(ValueError):
                s.read_synthetic_resource(Resource(failure), "asset:synthetic", png)

    def test_explicit_fixture_required_and_capture_always_rejected(self):
        with self.assertRaisesRegex(ValueError, "Only explicit synthetic"):
            s.Client([], self.root, fixture="real")
        for fixture, method in ((None, "host.ask_agent"), ("synthetic", "host.capture_region"), ("synthetic", "shell.exec")):
            frame = dict(version=1, type="request", id="runtime:1", method=method, params={})
            code = "print(" + repr(json.dumps(frame)) + ", flush=True)"
            client = s.Client([sys.executable, "-u", "-c", code], self.root, 2, fixture=fixture)
            try:
                with self.assertRaisesRegex(ValueError, "Unexpected host request"):
                    client.next(time.monotonic() + 2)
            finally:
                client.cleanup()

    def test_mock_request_schema_exact_asset_authorization_and_revision(self):
        state = dict(document_id="doc:s", page_id="page:s", revision=3)
        expected = dict(document_id="doc:s", page_id="page:s", revision=3, user_authorized=True,
                        job_id="job:s", prompt=s.SYNTHETIC_PROMPT, asset_refs=["asset:s"], write_back=True)
        class Fake:
            def call(self, method, params):
                self_test.assertEqual(method, "agent.request")
                self_test.assertEqual(params["expected_revision"], 3)
                self_test.assertTrue(params["user_authorized"])
                return dict(job_id="job:s", status="pending")
            def wait_host(self, method):
                return dict(id="runtime:ask", method=method, params=copy.deepcopy(expected))
        self_test = self
        self.assertEqual(s.start_mock_job(Fake(), state, "asset:s")[0], "job:s")
        for key, value in (("revision", 4), ("write_back", False), ("asset_refs", ["asset:unrelated"]), ("page_id", "page:wrong")):
            previous = expected[key]
            expected[key] = value
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "schema/context"):
                s.start_mock_job(Fake(), state, "asset:s")
            expected[key] = previous

    def test_bidirectional_subprocess_interleaves_resource_response_and_job_event(self):
        code = r'''
import json, sys
def send(**kw): print(json.dumps(dict(version=1, **kw)), flush=True)
request = json.loads(sys.stdin.readline())
send(type="request", id="runtime:ask", method="host.ask_agent", params={"job_id":"job:local"})
send(type="response", id=request["id"], ok=True, result={"job_id":"job:local","status":"pending"})
read = json.loads(sys.stdin.readline())
assert read["method"] == "resources.read"
send(type="event", event="state_changed", data={})
send(type="response", id=read["id"], ok=True, result={"bytes":[1,2,3]})
answer = json.loads(sys.stdin.readline())
assert answer["type"] == "response" and answer["id"] == "runtime:ask" and answer["result"]["job_id"] == "host:job"
cancel = json.loads(sys.stdin.readline())
assert cancel["params"]["job_id"] == "job:local"
send(type="request", id="runtime:cancel", method="jobs.cancel", params={"job_id":"host:job","request_id":"runtime:ask"})
send(type="event", event="job.finished", data={"job_id":"job:local","ok":False,"error":{"code":"cancelled","message":"synthetic"}})
send(type="response", id=cancel["id"], ok=True, result={"cancelled":True,"job_id":"job:local"})
ack = json.loads(sys.stdin.readline())
assert ack["id"] == "runtime:cancel" and ack["result"] == {"cancelled":True,"job_id":"host:job"}
late = json.loads(sys.stdin.readline())
assert late["type"] == "event" and late["data"]["job_id"] == "host:job" and late["data"]["request_id"] == "runtime:ask"
'''
        client = s.Client([sys.executable, "-u", "-c", code], self.root, 3, fixture="synthetic")
        try:
            pending = client.call("agent.request", {})
            ask = client.wait_host("host.ask_agent")
            self.assertEqual(client.call("resources.read", {})["bytes"], [1, 2, 3])
            client.reply(ask, {"job_id": "host:job"})
            self.assertTrue(client.call("jobs.cancel", {"job_id": pending["job_id"]})["cancelled"])
            cancel = client.wait_host("jobs.cancel")
            self.assertEqual(cancel["params"], {"job_id": "host:job", "request_id": ask["id"]})
            client.wait_job("job:local", error="cancelled")
            client.reply(cancel, {"cancelled": True, "job_id": "host:job"})
            client.complete_host_job("host:job", ask["id"], s.mock_result(s.synthetic_operations()))
            self.assertEqual(client.finish(), 0)
            self.assertIn(b"runtime:ask", client.stdout_log)
            self.assertIn(b"SYNTHETIC MOCK HOST", client.stdin_log)
            self.assertFalse(client.job_events)
        finally:
            client.cleanup()

    def test_failure_retains_private_protocol_logs_without_overwrite(self):
        logs = self.root / "private"
        with patch.object(s, "confined", side_effect=lambda p: Path(p)):
            with self.assertRaisesRegex(ValueError, "partially applied"):
                s.run_session([sys.executable, "-u", "-c", FAKE, "partial"], "drawing", self.root, 3, log_dir=logs)
            result = json.loads((logs / "session.local.json").read_text())
            self.assertFalse(result["passed"])
            self.assertTrue(result["forced_termination"])
            self.assertFalse(result["host_tested"])
            self.assertGreater((logs / "stdout.private.jsonl").stat().st_size, 0)
            with self.assertRaises(FileExistsError):
                s.run_session([], "drawing", self.root, log_dir=logs)

    def test_job_error_is_not_a_success_or_duplicate_null_error(self):
        client = s.Client([sys.executable, "-c", "pass"], self.root, 2, fixture="synthetic")
        try:
            client.job_events.append(dict(job_id="job:1", ok=False, error=dict(code="revision_conflict")))
            with self.assertRaisesRegex(ValueError, "Mock-host job failed"):
                client.wait_job("job:1")
            client.job_events.append(dict(job_id="job:2", ok=True, result={}, error=None))
            with self.assertRaises(ValueError):
                client.wait_job("job:2")
        finally:
            client.cleanup()

    def test_smoke_carries_native_status_without_claiming_legal_or_source_attestation(self):
        package = self.root / "package"
        package.mkdir()
        (self.root / "COMPLETE.txt").write_text("local")
        for name in ("neo.exe", "FILES.sha256.json"):
            (package / name).write_text("synthetic")
        for native in (True, "unknown"):
            source = dict(native_tts_removed=native, runtime_build=dict(verified_against_receipt=True,
                          limitations=["recovered after build"], reproducible_build_verified=False))
            (package / "SOURCE.json").write_text(json.dumps(source))
            cli = s.subprocess.CompletedProcess([], 0, b"", b"synthetic help/version")
            session = dict(passed=True, rpc_count=102, requests=["synthetic"] * 102)
            with patch.object(s, "confined", side_effect=lambda path: Path(path)), \
                 patch.object(s, "verify_payload") as verify, \
                 patch.object(s, "digest", return_value=dict(sha256="a" * 64, bytes=1)), \
                 patch.object(s.subprocess, "run", return_value=cli) as launch, \
                 patch.object(s, "run_session", side_effect=lambda *a, **kw: copy.deepcopy(session)) as run:
                report = s.smoke(self.root, fixture="synthetic")
            self.assertTrue(report["passed"])
            self.assertEqual(report["rpc_count"], 204)
            self.assertEqual(report["native_tts_removed"], native)
            self.assertEqual(report["runtime_build"], source["runtime_build"])
            self.assertFalse(report["neo_executed"])
            self.assertFalse(report["legal_approved"])
            self.assertFalse(report["reproducible_build_verified"])
            self.assertTrue(report["payload_verified_after_smoke"])
            self.assertEqual(verify.call_count, 2)
            self.assertEqual(run.call_count, 2)
            for call in run.call_args_list:
                self.assertEqual(call.args[0][-1], "--headless")
                self.assertNotEqual(Path(call.args[0][0]).name, "neo.exe")
            for call in launch.call_args_list:
                self.assertNotEqual(Path(call.args[0][0]).name, "neo.exe")
            (self.root / "smoke.local.json").unlink()

    def test_payload_tamper_and_extra_file_rejected(self):
        package = self.root / "package"
        package.mkdir()
        (package / "NOT_FOR_DISTRIBUTION.txt").write_text("test")
        (package / "SOURCE.json").write_text(json.dumps({"mode": "LOCAL EVALUATION", "public_approved": False}))
        files = {p.name: s.digest(p) for p in package.iterdir()}
        (package / "FILES.sha256.json").write_text(json.dumps(files))
        s.verify_payload(package)
        (package / "private.txt").write_text("must not be here")
        with self.assertRaisesRegex(ValueError, "allowlist"):
            s.verify_payload(package)
        (package / "private.txt").unlink()
        (package / "SOURCE.json").write_text("modified")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            s.verify_payload(package)


if __name__ == "__main__":
    unittest.main()
