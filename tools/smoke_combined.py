"""Bounded, synthetic, headless-only protocol-1 smoke of a local evaluation.

  python -B tools/smoke_combined.py --evaluation target/combined-evaluation/local-agent-synthetic --fixture synthetic

Never launches neo.exe, GUI, capture, microphone, model inference or installers.
Python mirrors Neo's wire envelope/state rules, NOT its hosted-only ready check:
Neo's real native client requires headless=false/has_window=true and is not run.
Synthetic documents and recovery directories exist only in a temporary directory
under the evaluation; clean close saves first and NEVER discards dirty changes.
--fixture synthetic opts into Python mock-host replies, never Neo/model replies.
Bounded raw stdin/stdout and stderr evidence is retained locally, never uploaded.
Resources may be copied by the packager; neither their execution nor legal
approval is validated here. Native technical status is carried from the hashed
package manifest, not inferred from headless runtime success.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib

if __package__:
    from .package_combined import checked, confined, digest, write_json
else:
    from package_combined import checked, confined, digest, write_json

MAX_LINE = 65536
PERMISSIONS = {"classroom_safe": True, "desktop_capture_allowed": False, "agent_allowed": False}
AGENT_PERMISSIONS = dict(PERMISSIONS, agent_allowed=True)
AGENT_METHODS = {"agent.request", "jobs.cancel", "pages.add", "pages.select", "resources.import_png",
                 "resources.begin", "resources.chunk", "resources.finish", "resources.abort",
                 "resources.read", "resources.release"}
LOG_LIMIT = 4 * 1024 * 1024
SYNTHETIC_PROMPT = "SYNTHETIC FIXTURE ONLY: mock structured edit; no Neo model, network, photo or real document."
REQUIRED = {"configure", "show", "get_state", "close", "objects.apply", "objects.list", "undo", "redo", "document.save"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, "Duplicate JSON field")
        value[key] = item
    return value


def decode_frame(raw):
    require(len(raw.rstrip(b"\r\n")) <= MAX_LINE, "Oversized JSONL frame")
    frame = json.loads(raw.decode("utf-8"), object_pairs_hook=unique,
                       parse_constant=lambda value: (_ for _ in ()).throw(ValueError("Non-finite JSON")))
    require(isinstance(frame, dict) and type(frame.get("version")) is int and frame["version"] == 1, "Invalid protocol version")
    kind = frame.get("type")
    require(kind in {"event", "response", "request"}, "Invalid envelope type")
    if kind == "event":
        require(isinstance(frame.get("event"), str) and "data" in frame, "Invalid event")
    else:
        ident = frame.get("id")
        require(isinstance(ident, str) and ident.startswith(("neo:", "runtime:"))
                and ident.split(":", 1)[1] and len(ident.encode()) <= 256
                and not any(c.isspace() or ord(c) < 32 for c in ident), "Invalid request ID")
        if kind == "response":
            ok = frame.get("ok")
            require(type(ok) is bool, "Response missing boolean ok")
            require(("result" in frame and "error" not in frame) if ok else
                    ("result" not in frame and isinstance(frame.get("error"), dict)), "Invalid result/error envelope")
            if not ok:
                require(isinstance(frame["error"].get("code"), str) and isinstance(frame["error"].get("message"), str), "Invalid error")
        else:
            require(isinstance(frame.get("method"), str) and isinstance(frame.get("params"), dict), "Invalid request")
    return frame


def validate_ready(frame, app):
    require(frame.get("type") == "event" and frame.get("event") == "ready", "First frame must be ready")
    data = frame["data"]
    require(isinstance(data, dict) and data.get("app") == app and data.get("headless") is True
            and data.get("has_window") is False and data.get("max_line_bytes") == MAX_LINE
            and data.get("revision_scope") == "document", "Not compatible headless ready")
    methods = data.get("methods")
    require(isinstance(methods, list) and all(isinstance(x, str) for x in methods), "Invalid methods")
    require(len(set(methods)) == len(methods) and REQUIRED <= set(methods), "Missing/duplicate runtime methods")
    require("capture.request" not in methods, "Headless runtime advertises capture")
    return data


def validate_state(state, app, permissions=PERMISSIONS):
    require(isinstance(state, dict) and state.get("app") == app and state.get("revision_scope") == "document", "Invalid state identity")
    for key in ("visible", "hidden_confirmed", "has_window", "dirty", "closed", "configured",
                "desired_visible", "effective_visible", "close_pending", "connected"):
        require(type(state.get(key)) is bool, "Invalid boolean state: " + key)
    for key in ("document_id", "page_id"):
        require(isinstance(state.get(key), str) and 0 < len(state[key].encode()) <= 256, "Invalid state ID")
    require(type(state.get("revision")) is int and 0 <= state["revision"] < 2**64, "Invalid revision")
    require(state.get("permissions") == permissions and all(type(x) is bool for x in state["permissions"].values()), "Unexpected permissions")
    require(not state["visible"] and not state["has_window"] and not state["hidden_confirmed"]
            and state.get("window_status") == "no_window", "Headless window invariant failed")
    return state


class Client:
    def __init__(self, command, cwd, timeout=15, *, fixture=None):
        require(fixture in (None, "synthetic"), "Only explicit synthetic mock-host fixture is supported")
        self.fixture = fixture
        self.deadline = time.monotonic() + 90
        self.timeout = timeout
        self.stdout_log = bytearray()
        self.stdin_log = bytearray()
        self.host_requests = []
        self.job_events = []
        self.host_ids = set()
        self.frames = queue.Queue(maxsize=256)
        self.stderr = bytearray()
        self.error = None
        self.events = 0
        self.counter = 0
        self.requests = []
        env = dict(os.environ)
        for key in list(env):
            if key.startswith(("NEO_DRAW", "NEO_BLACKBOARD", "TEXTELLER", "ORT_")):
                env.pop(key)
        env.update({"LOCALAPPDATA": str(cwd), "APPDATA": str(cwd), "TEMP": str(cwd), "TMP": str(cwd)})
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.threads = [threading.Thread(target=self._stdout, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for thread in self.threads:
            thread.start()

    def _stdout(self):
        try:
            while True:
                line = self.process.stdout.readline(MAX_LINE + 3)
                if not line:
                    self.frames.put_nowait(None)
                    return
                require(len(self.stdout_log) + len(line) <= LOG_LIMIT, "Private stdout log budget exceeded")
                self.stdout_log.extend(line)
                require(len(line.rstrip(b"\r\n")) <= MAX_LINE, "Oversized stdout frame")
                self.frames.put_nowait(decode_frame(line))
        except (ValueError, UnicodeError, queue.Full, OSError) as exc:
            self.error = str(exc)

    def _stderr(self):
        try:
            while data := self.process.stderr.read(4096):
                # Drain forever but retain at most 64 KiB of local-only diagnostics.
                self.stderr.extend(data[:max(0, MAX_LINE - len(self.stderr))])
        except OSError:
            pass

    def next(self, deadline):
        deadline = min(deadline, self.deadline)
        while time.monotonic() < deadline:
            if self.error:
                raise ValueError(self.error)
            try:
                frame = self.frames.get(timeout=min(.1, max(.001, deadline - time.monotonic())))
            except queue.Empty:
                continue
            require(frame is not None, "Unexpected protocol EOF")
            if frame["type"] == "request":
                require(self.fixture == "synthetic" and frame["method"] in {"host.ask_agent", "jobs.cancel"},
                        "Unexpected host request; only synthetic Agent/cancel is authorized")
                require(frame["id"].startswith("runtime:") and frame["id"] not in self.host_ids,
                        "Invalid or reused runtime request ID")
                self.host_ids.add(frame["id"])
            require(not (frame["type"] == "event" and frame["event"] == "protocol_error"), "Runtime protocol_error")
            return frame
        raise TimeoutError("Runtime protocol deadline exceeded")

    def send(self, value):
        raw = json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()
        require(len(raw) <= MAX_LINE, "Outbound frame too large")
        require(len(self.stdin_log) + len(raw) + 1 <= LOG_LIMIT, "Private stdin log budget exceeded")
        self.stdin_log.extend(raw + b"\n")
        self.process.stdin.write(raw + b"\n")
        self.process.stdin.flush()

    def dispatch(self, frame):
        if frame["type"] == "request":
            require(len(self.host_requests) < 32, "Too many unhandled mock-host requests")
            self.host_requests.append(frame)
            return True
        if frame["type"] == "event":
            self.events += 1
            if frame["event"] == "job.finished":
                require(len(self.job_events) < 64, "Too many unhandled job events")
                self.job_events.append(frame["data"])
            return True
        return False

    def wait_host(self, method):
        deadline = time.monotonic() + self.timeout
        while True:
            if self.host_requests:
                frame = self.host_requests.pop(0)
                require(frame["method"] == method, "Unexpected mock-host method/order")
                return frame
            require(self.dispatch(self.next(deadline)), "Unexpected response while waiting for host request")

    def wait_job(self, job_id, error=None):
        deadline = time.monotonic() + self.timeout
        while True:
            for index, event in enumerate(self.job_events):
                if event.get("job_id") == job_id:
                    self.job_events.pop(index)
                    if error:
                        require(event.get("ok") is False and "result" not in event
                                and event.get("error", {}).get("code") == error, "Expected job error: " + error)
                    else:
                        require(event.get("ok") is True and "result" in event and "error" not in event,
                                "Mock-host job failed: " + str(event))
                    return event
            require(self.dispatch(self.next(deadline)), "Unexpected response while waiting for job completion")

    def reply(self, request, result):
        require(self.fixture == "synthetic" and request["id"] in self.host_ids, "Not an observed synthetic host request")
        self.send({"version": 1, "type": "response", "id": request["id"], "ok": True, "result": result})

    def complete_host_job(self, host_job, request_id, result):
        require(self.fixture == "synthetic", "Mock completion requires synthetic fixture")
        self.send({"version": 1, "type": "event", "event": "job.finished",
                   "data": {"job_id": host_job, "request_id": request_id, "ok": True, "result": result}})

    def call(self, method, params=None, error=None):
        self.counter += 1
        ident = f"neo:{self.counter}"
        self.send({"version": 1, "type": "request", "id": ident, "method": method, "params": params or {}})
        self.requests.append(method)
        deadline = time.monotonic() + self.timeout
        while True:
            frame = self.next(deadline)
            if self.dispatch(frame):
                continue
            require(frame["id"] == ident, "Unexpected or stale response ID")
            if error:
                require(not frame["ok"] and frame["error"]["code"] == error, "Expected error: " + error)
                return frame["error"]
            require(frame["ok"], "RPC failed: " + method + " " + str(frame.get("error")))
            return frame["result"]

    def finish(self):
        # Keep stdin open until protocol close has been acknowledged.
        code = self.process.wait(timeout=self.timeout)
        require(code == 0, "Nonzero runtime exit")
        for thread in self.threads:
            thread.join(timeout=2)
        require(self.error is None, "Protocol reader failed: " + str(self.error))
        # Do not hide protocol errors/extra job completions queued after the final response.
        while not self.frames.empty():
            frame = self.frames.get_nowait()
            if frame is not None:
                require(frame["type"] == "event" and frame["event"] != "protocol_error",
                        "Unexpected frame after clean close")
                self.dispatch(frame)
        return code

    def cleanup(self):
        forced = self.process.poll() is None
        if forced:
            # Only this smoke's fresh synthetic process can reach this failure path.
            self.process.kill()
            self.process.wait(timeout=5)
        for thread in self.threads:
            thread.join(timeout=2)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()
        return forced


def context(state):
    return {"document_id": state["document_id"], "page_id": state["page_id"], "expected_revision": state["revision"]}


def synthetic_operations():
    shape = {"id": "synthetic-shape", "kind": {"type": "shape", "shape": "rectangle",
             "points": [{"x": 10, "y": 10}, {"x": 110, "y": 60}],
             "style": {"color": {"r": 20, "g": 120, "b": 200, "a": 255}, "width": 2, "dashed": False}}}
    plot = {"id": "synthetic-plot", "kind": {"type": "function_plot", "position": {"x": 120, "y": 30},
            "width": 200, "height": 120, "expressions": ["x^2", "y^2=x"], "x_min": -3, "x_max": 3, "y_min": -3, "y_max": 3}}
    return [{"op": "add", "object": shape}, {"op": "add", "object": plot}]


def tiny_png():
    """Generate a 2x2 RGBA pattern, never read a photo or external image."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    pixels = b"\x00" + bytes([255, 0, 0, 255, 0, 255, 0, 255])
    pixels += b"\x00" + bytes([0, 0, 255, 255, 255, 255, 255, 255])
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def read_synthetic_resource(client, asset, png):
    assembled = bytearray()
    chunks = 0
    while len(assembled) < len(png):
        offset = len(assembled)
        result = client.call("resources.read", {"asset_ref": asset, "offset": offset, "length": 13})
        data = result.get("bytes")
        require(isinstance(data, list) and 0 < len(data) <= 13
                and all(type(x) is int and 0 <= x <= 255 for x in data), "Invalid resource byte chunk")
        next_offset = offset + len(data)
        require(result.get("asset_ref") == asset and result.get("offset") == offset
                and result.get("total_bytes") == len(png) and result.get("next_offset") == next_offset
                and next_offset <= len(png) and result.get("eof") is (next_offset == len(png))
                and result.get("mime_type") == "image/png", "Resource chunk offset/total/eof mismatch")
        assembled.extend(data)
        chunks += 1
    require(bytes(assembled) == png and zlib.crc32(assembled) == zlib.crc32(png), "Synthetic PNG bytes/CRC mismatch")
    return {"bytes": len(assembled), "crc32": zlib.crc32(assembled), "chunks": chunks,
            "sha256": hashlib.sha256(assembled).hexdigest()}


def start_mock_job(client, state, asset):
    pending = client.call("agent.request", {**context(state), "user_authorized": True,
                          "prompt": SYNTHETIC_PROMPT, "asset_refs": [asset], "write_back": True})
    require(pending.get("status") == "pending" and isinstance(pending.get("job_id"), str), "Invalid local Agent job")
    request = client.wait_host("host.ask_agent")
    require(request["params"] == {"document_id": state["document_id"], "page_id": state["page_id"],
            "revision": state["revision"], "user_authorized": True, "job_id": pending["job_id"],
            "prompt": SYNTHETIC_PROMPT, "asset_refs": [asset], "write_back": True}, "host.ask_agent schema/context/authorization mismatch")
    return pending["job_id"], request


def mock_result(operations):
    return {"answer": "SYNTHETIC MOCK HOST ONLY — not a Neo/model answer", "operations": operations}


def run_agent_fixture(client, app):
    """Exercise the real runtime with a deterministic, offline Python mock host."""
    require(client.fixture == "synthetic", "Agent tests require explicit synthetic fixture")
    state = validate_state(client.call("configure", AGENT_PERMISSIONS), app, AGENT_PERMISSIONS)
    def current():
        return validate_state(client.call("get_state"), app, AGENT_PERMISSIONS)

    png = tiny_png()
    imported = client.call("resources.import_png", {"bytes": list(png)})
    require(imported.get("width") == 2 and imported.get("height") == 2
            and imported.get("mime_type") == "image/png" and isinstance(imported.get("asset_ref"), str), "Synthetic import descriptor mismatch")
    asset = imported["asset_ref"]
    resource_reads = [read_synthetic_resource(client, asset, png)]
    # Exercise total-byte reservation + real runtime CRC rejection and acceptance.
    def upload(crc):
        begin = client.call("resources.begin", {"total_bytes": len(png), "crc32": crc})
        require(begin.get("max_chunk_bytes") == 8192, "Unexpected upload chunk limit")
        for offset in range(0, len(png), 17):
            data = list(png[offset:offset + 17])
            result = client.call("resources.chunk", {"upload_id": begin["upload_id"], "offset": offset, "bytes": data})
            require(result.get("next_offset") == offset + len(data), "Upload progress mismatch")
        return begin["upload_id"]
    bad = upload(zlib.crc32(png) ^ 1)
    client.call("resources.finish", {"upload_id": bad}, error="resource_integrity")
    good = upload(zlib.crc32(png))
    second = client.call("resources.finish", {"upload_id": good})
    resource_reads.append(read_synthetic_resource(client, second["asset_ref"], png))
    # The 32 MiB aggregate budget includes outstanding reservations, not just stored PNGs.
    reservations = []
    for _ in range(3):
        reservations.append(client.call("resources.begin", {"total_bytes": 8 * 1024 * 1024, "crc32": 0})["upload_id"])
    client.call("resources.begin", {"total_bytes": 8 * 1024 * 1024, "crc32": 0}, error="resource_limit")
    for upload_id in reservations:
        require(client.call("resources.abort", {"upload_id": upload_id}).get("aborted") is True, "Reservation cleanup failed")
    require(current()["revision"] == state["revision"], "Resource transport must not revise document")

    # Explicit per-request authorization is independent of configure.agent_allowed.
    client.call("agent.request", {**context(state), "user_authorized": False, "prompt": SYNTHETIC_PROMPT,
                                 "asset_refs": [asset], "write_back": True}, error="authorization_required")
    require(current()["pending_jobs"] == 0 and not client.host_requests, "Unauthorized Agent request escaped")

    original_page = state["page_id"]
    original_objects = client.call("objects.list", context(state))["objects"]
    require(original_objects == [op["object"] for op in synthetic_operations()], "Unexpected synthetic baseline")
    added = client.call("pages.add", context(state))
    state = validate_state(added["state"], app, AGENT_PERMISSIONS)
    other_page = state["page_id"]
    state = validate_state(client.call("pages.select", {"document_id": state["document_id"], "page_id": original_page}), app, AGENT_PERMISSIONS)
    before = state["revision"]
    job, request = start_mock_job(client, state, asset)
    # Reentrant resource requests while host.ask_agent is outstanding must not deadlock.
    resource_reads.append(read_synthetic_resource(client, asset, png))
    state = validate_state(client.call("pages.select", {"document_id": state["document_id"], "page_id": other_page}), app, AGENT_PERMISSIONS)
    require(state["revision"] == before, "Selecting a page unexpectedly revised document")
    operations = copy.deepcopy(synthetic_operations())
    operations[0]["object"]["id"] = "mock-added-shape"
    operations[1]["object"]["id"] = "mock-added-plot"
    updated = copy.deepcopy(original_objects[0])
    updated["kind"]["points"][1] = {"x": 150, "y": 95}
    operations += [{"op": "update", "object": updated}, {"op": "delete", "id": original_objects[1]["id"]}]
    expected = [updated, operations[0]["object"], operations[1]["object"]]
    client.reply(request, mock_result(operations))
    finished = client.wait_job(job)
    require(finished["result"].get("revision") == before + 1, "Host transaction did not increment exactly once")
    state = current()
    require(state["page_id"] == other_page and state["revision"] == before + 1, "Host writeback changed current page or wrong revision")
    require(client.call("objects.list", context(state))["total"] == 0, "Host wrote to selected page instead of original page")
    def original_listing(state):
        return client.call("objects.list", {**context(state), "page_id": original_page})["objects"]
    require(original_listing(state) == expected, "Host structured add/update/delete mismatch")
    undone = client.call("undo", context(state))
    require(undone.get("changed") is True, "Host atomic undo missing")
    state = current()
    require(original_listing(state) == original_objects, "Host operations were not one undo transaction")
    client.call("redo", context(state))
    state = current()
    require(original_listing(state) == expected, "Host atomic redo mismatch")
    state = validate_state(client.call("pages.select", {"document_id": state["document_id"], "page_id": original_page}), app, AGENT_PERMISSIONS)

    # A valid edit followed by an invalid delete must roll back the entire HOST result.
    before = state["revision"]
    job, request = start_mock_job(client, state, asset)
    invalid = [{"op": "delete", "id": "mock-added-shape"}, {"op": "delete", "id": "synthetic-missing"}]
    client.reply(request, mock_result(invalid))
    client.wait_job(job, error="invalid_document")
    state = current()
    require(state["revision"] == before and original_listing(state) == expected, "Invalid host result partially applied")

    # Deferred host job mapping: wrong request_id ignored, then stale revision rejected.
    job, request = start_mock_job(client, state, asset)
    host_job = "synthetic-host-stale"
    client.reply(request, {"job_id": host_job})
    late_ops = [{"op": "delete", "id": "mock-added-shape"}]
    client.complete_host_job(host_job, "runtime:synthetic-wrong-request", mock_result(late_ops))
    unchanged = current()
    require(unchanged["revision"] == state["revision"] and unchanged["pending_jobs"] == 1
            and original_listing(unchanged) == expected, "Mismatched host request_id was accepted")
    changed = copy.deepcopy(updated)
    changed["kind"]["points"][1]["x"] = 175
    state = validate_state(client.call("objects.apply", {**context(state), "operations": [{"op": "update", "object": changed}]}), app, AGENT_PERMISSIONS)
    expected[0] = changed
    before = state["revision"]
    client.complete_host_job(host_job, request["id"], mock_result(late_ops))
    client.wait_job(job, error="revision_conflict")
    state = current()
    require(state["revision"] == before and original_listing(state) == expected, "Stale host result modified document")

    cancellations = []
    for deferred in (False, True):
        job, request = start_mock_job(client, state, asset)
        host_job = "synthetic-host-cancel" if deferred else None
        if deferred:
            client.reply(request, {"job_id": host_job})
            require(current()["pending_jobs"] == 1, "Deferred job registration failed")
        cancel_result = client.call("jobs.cancel", {"job_id": job})
        require(cancel_result == {"cancelled": True, "job_id": job}, "Wrong local cancellation response")
        cancel = client.wait_host("jobs.cancel")
        mapped_job = host_job or job
        require(cancel["id"] != request["id"] and cancel["params"] == {"job_id": mapped_job, "request_id": request["id"]},
                "jobs.cancel request_id/host-job mapping mismatch")
        client.wait_job(job, error="cancelled")
        client.reply(cancel, {"cancelled": True, "job_id": mapped_job})
        if deferred:
            client.complete_host_job(host_job, request["id"], mock_result(late_ops))
        else:
            client.reply(request, mock_result(late_ops))
        after = current()  # Ordered barrier proves the late frame has been consumed, no sleep heuristic.
        require(after["pending_jobs"] == 0 and after["revision"] == state["revision"]
                and original_listing(after) == expected and not client.job_events, "Late cancelled answer was applied/completed twice")
        cancellations.append({"deferred": deferred, "local_job_id": job, "host_job_id": host_job,
                              "ask_request_id": request["id"], "cancel_request_id": cancel["id"], "late_answer_ignored": True})
        state = after
    require(not client.host_requests and not client.job_events, "Unconsumed mock-host traffic")
    # Imported resources remain readable and byte-exact after edits/cancellation.
    resource_reads.append(read_synthetic_resource(client, asset, png))
    return {"fixture": "synthetic", "host": "Python deterministic mock; NOT Neo/model/network",
            "original_page_atomic_writeback": True, "structured_operations": ["add shape", "add plot", "update full shape", "delete plot"],
            "single_undo_redo": True, "invalid_host_atomic_rollback": True, "stale_revision_rejected": True,
            "wrong_request_id_ignored": True, "unauthorized_request_rejected": True, "cancellations": cancellations,
            "resource": {"synthetic_png_bytes": len(png), "crc32": zlib.crc32(png), "reads": resource_reads,
                         "total_read_bytes": sum(x["bytes"] for x in resource_reads), "bad_upload_crc_rejected": True,
                         "aggregate_budget_bytes": 32 * 1024 * 1024, "reservation_limit_rejected": True,
                         "reservations_aborted": len(reservations)}}


def run_session(command, app, temporary, timeout=15, *, fixture=None, log_dir=None):
    if log_dir is not None:
        log_dir = confined(log_dir)
        log_dir.mkdir(parents=True, exist_ok=False)
    client = Client(command, temporary, timeout, fixture=fixture)
    report = {"app": app, "passed": False, "forced_termination": False, "fixture": fixture or "basic",
              "host_tested": False, "model_executed": False}
    try:
        ready = validate_ready(client.next(time.monotonic() + timeout), app)
        if fixture == "synthetic":
            require(AGENT_METHODS <= set(ready["methods"]) and "host.ask_agent" in ready.get("host_methods", []),
                    "Missing synthetic Agent/resource capabilities")
        state = validate_state(client.call("configure", PERMISSIONS), app)
        require(state["configured"] and not state["dirty"], "Fresh session must be clean/configured")
        state = validate_state(client.call("show"), app)
        require(state["desired_visible"], "show did not update desired visibility")
        state = validate_state(client.call("get_state"), app)
        initial_revision = state["revision"]
        ops = synthetic_operations()
        state = validate_state(client.call("objects.apply", {**context(state), "operations": ops}), app)
        require(state["dirty"] and state["revision"] == initial_revision + 1, "Atomic transaction revision/dirty mismatch")
        objects = client.call("objects.list", context(state))
        require(objects["total"] == 2 and objects["objects"] == [x["object"] for x in ops], "Shape/plot roundtrip mismatch")
        # A transaction containing one valid delete and one nonexistent delete must roll back entirely.
        client.call("objects.apply", {**context(state), "operations": [{"op": "delete", "id": "synthetic-shape"}, {"op": "delete", "id": "does-not-exist"}]}, error="invalid_document")
        unchanged = validate_state(client.call("get_state"), app)
        require(unchanged["revision"] == state["revision"], "Failed transaction advanced revision")
        require(client.call("objects.list", context(unchanged))["objects"] == objects["objects"], "Failed transaction partially applied")
        undone = client.call("undo", context(state))
        require(undone["changed"] is True, "Undo did not change")
        state = validate_state(undone["state"], app)
        require(not state["dirty"] and client.call("objects.list", context(state))["total"] == 0, "One undo did not remove entire transaction")
        redone = client.call("redo", context(state))
        require(redone["changed"] is True, "Redo did not change")
        state = validate_state(redone["state"], app)
        require(state["dirty"] and client.call("objects.list", context(state))["total"] == 2, "Redo failed")
        if fixture == "synthetic":
            report["mock_agent"] = run_agent_fixture(client, app)
            state = validate_state(client.call("configure", PERMISSIONS), app)
            require(state["pending_jobs"] == 0, "Mock jobs leaked after permission reset")
            objects = client.call("objects.list", context(state))
        client.call("close", error="unsaved_changes")
        state = validate_state(client.call("get_state"), app)
        require(state["dirty"] and not state["closed"], "Dirty close protection failed")
        save = temporary / "synthetic.neoboard"
        state = validate_state(client.call("document.save", {"path": str(save.resolve())}), app)
        require(save.is_file() and not state["dirty"], "Synthetic save failed")
        saved = json.loads(save.read_text(encoding="utf-8"))
        require(saved["document"]["pages"][0]["objects"] == objects["objects"], "Saved synthetic shape/plot mismatch")
        state = validate_state(client.call("get_state"), app)
        require(not state["dirty"], "Saved document still dirty")
        closed = validate_state(client.call("close"), app)
        require(closed["closed"] and not closed["dirty"], "Clean close failed")
        report.update({"exit_code": client.finish(), "passed": True, "ready": ready,
                       "synthetic_saved_file": digest(save), "requests": client.requests,
                       "rpc_count": len(client.requests),
                       "interleaved_events": client.events,
                       "atomic_rollback": True, "single_undo_redo": True, "dirty_close_protected": True})
    except (OSError, ValueError, KeyError, TypeError, TimeoutError, subprocess.SubprocessError) as exc:
        report["error"] = str(exc)
        raise
    finally:
        report["forced_termination"] = client.cleanup()
        if log_dir is not None:
            for name, data in (("stdout.private.jsonl", client.stdout_log), ("stdin.private.jsonl", client.stdin_log),
                               ("stderr.private.log", client.stderr)):
                (log_dir / name).write_bytes(data)
            report["private_logs"] = {name: digest(log_dir / name) for name in
                                      ("stdout.private.jsonl", "stdin.private.jsonl", "stderr.private.log")}
            write_json(log_dir / "session.local.json", report)
    return report


def verify_payload(package):
    require((package / "NOT_FOR_DISTRIBUTION.txt").is_file(), "Not a marked local evaluation")
    manifest = json.loads((package / "FILES.sha256.json").read_text(encoding="utf-8"))
    expected = set(manifest) | {"FILES.sha256.json"}
    actual = {p.relative_to(package).as_posix() for p in package.rglob("*") if p.is_file()}
    require(actual == expected, "Payload file allowlist mismatch")
    for name, record in manifest.items():
        path = checked(package / name)
        require(path.is_relative_to(package.resolve()), "Manifest path escapes package")
        require(digest(path) == record, "Payload hash mismatch: " + name)
    source = json.loads((package / "SOURCE.json").read_text(encoding="utf-8"))
    require(source.get("mode") == "LOCAL EVALUATION" and source.get("public_approved") is False, "Not a local-only source manifest")


def smoke(evaluation, timeout=15, *, fixture=None):
    require(fixture in (None, "synthetic"), "Only synthetic fixtures are supported")
    evaluation = confined(evaluation)
    require((evaluation / "COMPLETE.txt").is_file(), "Assembly is incomplete")
    package = checked(evaluation / "package")
    verify_payload(package)
    source = json.loads((package / "SOURCE.json").read_text(encoding="utf-8"))
    report_path = evaluation / "smoke.local.json"
    require(not report_path.exists(), "Smoke report exists; preserve evidence and use a fresh evaluation")
    report = {"schema": 1, "mode": "LOCAL EVALUATION", "public_approved": False, "passed": False,
              "scope": "Python subprocess wire/state compatibility; actual Neo native hosted client NOT executed",
              "clean_install_verified": False, "neo_executed": False, "gui_capture_microphone": False,
              "fixture": fixture or "basic", "host_tested": False, "model_executed": False,
              "mock_host": "Python synthetic replies only; not Neo or a model" if fixture else None,
              "payload_limit": source.get("payload_scope", "Limited artifacts; no complete Neo speech/resources/models/Bash/ONNX payload"),
                            "local_resources_included": source.get("local_resources_included", False),
                            "resource_execution_validated": False, "bash_executed": False,
              "private_logs_only": True, "executables": {}, "sessions": [],
              "script": digest(Path(__file__)), "neo_exe": digest(package / "neo.exe"),
              "package_manifest": digest(package / "FILES.sha256.json"),
              "source_manifest": digest(package / "SOURCE.json"),
              "native_tts_removed": source.get("native_tts_removed", "unknown"),
              "native_validation_scope": "Carried from hash-verified package; smoke does not validate native TTS removal or legal approval",
              "legal_approved": False, "reproducible_build_verified": False,
              "runtime_build": source.get("runtime_build"), "rpc_count": 0}
    try:
        for app in ("drawing", "blackboard"):
            exe = checked(package / "apps" / app / f"neo-{app}.exe")
            report["executables"][app] = digest(exe)
            with tempfile.TemporaryDirectory(prefix="synthetic-", dir=evaluation) as temp:
                temporary = Path(temp)
                cli = {}
                for flag in ("--version", "--help"):
                    result = subprocess.run([str(exe), flag], cwd=temporary, capture_output=True, timeout=timeout)
                    require(result.returncode == 0 and result.stdout == b"" and result.stderr.strip(), "Help/version must exit successfully with stderr only")
                    require(len(result.stderr) <= MAX_LINE, "Excessive help/version output")
                    cli[flag] = result.stderr.decode("utf-8").strip()
                session = run_session([str(exe), "--headless"], app, temporary, timeout,
                                      fixture=fixture, log_dir=evaluation / (app + "-synthetic-private"))
                session["cli_stderr"] = cli
                report["sessions"].append(session)
                report["rpc_count"] += session["rpc_count"]
        verify_payload(package)
        report["payload_verified_after_smoke"] = True
        report["passed"] = True
    except (OSError, ValueError, KeyError, TypeError, TimeoutError, subprocess.SubprocessError) as exc:
        report["error"] = str(exc)
    write_json(report_path, report)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--evaluation", required=True, type=Path)
    parser.add_argument("--fixture", choices=["synthetic"], help="Opt in to offline mock-host Agent/resource/cancellation scenarios; no real data/model")
    parser.add_argument("--timeout", type=float, default=15, help="Per CLI/RPC timeout, 0 < seconds <= 30")
    args = parser.parse_args(argv)
    if not 0 < args.timeout <= 30:
        parser.error("timeout must be between 0 and 30 seconds")
    try:
        report = smoke(args.evaluation, args.timeout, fixture=args.fixture)
        print("Headless local evaluation: " + ("PASS" if report["passed"] else "FAIL: " + report.get("error", "unknown")))
        return 0 if report["passed"] else 1
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        print(str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
