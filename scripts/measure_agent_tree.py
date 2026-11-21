#!/usr/bin/env python3
"""Reproduce the offline daemon/root/two-child resource measurement."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time


class Client:
    def __init__(self, path, daemon):
        deadline = time.monotonic() + 30
        while True:
            self.sock = socket.socket(socket.AF_UNIX)
            try:
                self.sock.connect(str(path))
                break
            except (FileNotFoundError, ConnectionRefusedError):
                self.sock.close()
                if daemon.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("daemon failed to become ready")
                threading.Event().wait(0.025)
        self.sock.settimeout(180)
        self.reader = self.sock.makefile("rb")
        assert json.loads(self.reader.readline())["type"] == "daemon_hello"
        self.counter = 0

    def call(self, command, **payload):
        self.counter += 1
        ident = str(self.counter)
        wire = {"type": "command", "id": ident,
                "protocol": {"name": "prime-agent.daemon", "version": 7},
                "command": {"type": command, **payload}}
        self.sock.sendall((json.dumps(wire) + "\n").encode())
        while line := self.reader.readline():
            response = json.loads(line)
            if response.get("type") == "response" and response.get("id") == ident:
                assert response.get("success"), response
                return response.get("data")
        raise RuntimeError("daemon closed connection")

    def close(self):
        self.reader.close()
        self.sock.close()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        messages = body["messages"]
        users = [(i, row) for i, row in enumerate(messages) if row["role"] == "user"]
        index, user = users[-1]
        text = json.dumps(user["content"])
        requested = next((tag for tag in ("SPAWN_ONE", "SPAWN_TWO", "CPU_TOOL") if tag in text), None)
        already_called = any(row["role"] == "tool" for row in messages[index + 1:])
        if requested and not already_called:
            if requested.startswith("SPAWN"):
                number = "one" if requested == "SPAWN_ONE" else "two"
                code = f"child_{number} = await rlm.spawn('CHILD_READY', name='measure-{number}')\nprint('SPAWN_DONE=' + child_{number}.rlm_child_id)"
            else:
                code = ("import time\nmeasurement_payload = bytearray(32 * 1024 * 1024)\n"
                        "measurement_deadline = time.process_time() + 0.6\n"
                        "while time.process_time() < measurement_deadline:\n"
                        "    measurement_sum = sum(range(10000))\n"
                        "del measurement_payload\nprint('CPU_TOOL_DONE')")
            delta = {"tool_calls": [{"index": 0, "id": f"measure_call_{len(messages)}", "type": "function",
                     "function": {"name": "python_repl", "arguments": json.dumps({"code": code})}}]}
            reason = "tool_calls"
        else:
            if requested:
                results = [row for row in messages[index + 1:] if row["role"] == "tool"]
                expected = "CPU_TOOL_DONE" if requested == "CPU_TOOL" else "SPAWN_DONE="
                self.server.tool_results.append({"tag": requested, "results": results})
                if expected not in json.dumps(results):
                    self.server.failures.append({"tag": requested, "results": results})
            delta, reason = {"content": "Measurement turn completed."}, "stop"
        self.server.requests.append({"tag": requested or "plain", "tool": reason == "tool_calls",
                                     "last_user": user, "following_roles": [row["role"] for row in messages[index + 1:]]})
        rows = [{"id": "measure", "object": "chat.completion.chunk", "created": 1,
                 "model": "measure-1", "choices": [{"index": 0, "delta": change,
                 "finish_reason": finish}]} for change, finish in
                (({"role": "assistant", **delta}, None), ({}, reason))]
        data = ("".join("data: " + json.dumps(row) + "\n\n" for row in rows)
                + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--kernel-python", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    for name in ("home", "agent", "tmp", "scratch", "cache", "config", "data", "state"):
        (root / name).mkdir()
    package = Path(__file__).resolve().parents[1]
    env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "LD_LIBRARY_PATH") if key in os.environ}
    env.update(HOME=str(root / "home"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
               XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
               UV_CACHE_DIR=str(root / "cache/uv"), PA_TEST_SCRATCH_ROOT=str(root / "scratch"),
               PRIME_AGENT_CODING_AGENT_DIR=str(root / "agent"), PI_CODING_AGENT_DIR=str(root / "agent"),
               PI_PACKAGE_DIR=str(package), PRIME_AGENT_KERNEL_PYTHON=str(args.kernel_python.absolute()),
               PYTHONPATH=os.pathsep.join(str(path) for path in sorted((package / "skills").glob("*/src"))),
               PI_OFFLINE="1", DO_NOT_TRACK="1", TERM="xterm-256color")
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    provider.requests = []
    provider.tool_results = []
    provider.failures = []
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    models = {"providers": {"measurement-local": {"api": "openai-completions", "apiKey": "fixture-only",
              "baseUrl": f"http://127.0.0.1:{provider.server_port}/v1", "models": [{"id": "measure-1",
              "name": "Measurement", "reasoning": False, "contextWindow": 128000, "maxTokens": 1024}]}}}
    (root / "agent/models.json").write_text(json.dumps(models))
    (root / "agent/settings.json").write_text(json.dumps({"prewarmPythonKernel": False,
        "defaultProvider": "measurement-local", "defaultModel": "measure-1", "onboardingCompleted": True}))
    phase = root / "phase"
    phase.write_text("daemon_only")
    log = (root / "daemon.log").open("wb")
    daemon = subprocess.Popen([str(binary), "--mode", "daemon", "--daemon-socket", str(root / "d.sock")],
                              cwd=root, env=env, stdout=log, stderr=log)
    sampler = subprocess.Popen([sys.executable, str(package / "scripts/sample_agent_resources.py"),
                               "--pid", str(daemon.pid), "--interval", "0.05", "--duration", "180",
                               "--phase-file", str(phase), "--output", str(root / "samples.csv")], env=env)
    command_shapes = {}
    observer_stop = threading.Event()
    def observe_commands():
        from sample_agent_resources import descendants, read_proc
        while not observer_stop.is_set():
            for process in descendants(read_proc(), daemon.pid):
                if process.identity in command_shapes:
                    continue
                try:
                    argv = (Path("/proc") / str(process.pid) / "cmdline").read_bytes().split(b"\0")
                except (FileNotFoundError, ProcessLookupError):
                    continue
                if len(argv) > 2 and argv[1:3] == [b"-m", b"rlm.repl"]:
                    shape = "python -m rlm.repl"
                elif len(argv) > 1 and argv[1] == b"-c":
                    shape = "python -c (probe; source omitted)"
                else:
                    shape = process.name
                command_shapes[process.identity] = {"pid": process.pid, "start_id": process.start_id,
                                                     "command_shape": shape}
            observer_stop.wait(0.025)
    observer = threading.Thread(target=observe_commands, daemon=True)
    observer.start()
    client = None
    receipt = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "daemon_pid": daemon.pid,
               "turns": [], "provider": "loopback fake SSE; no external inference"}
    def mark(label):
        phase.write_text(label)
    def observe(label):
        mark(label)
        threading.Event().wait(0.5)
    def turn(active, message, label):
        from sample_agent_resources import descendants, read_proc
        mark(label)
        before = {p.identity: p.cpu_seconds for p in descendants(read_proc(), daemon.pid)}
        result_count = len(provider.tool_results)
        start = time.monotonic()
        client.call("prompt_and_wait", activeSessionId=active, message=message)
        assert not provider.failures, provider.failures
        if message in ("CPU_TOOL", "SPAWN_ONE", "SPAWN_TWO"):
            assert any(row["tag"] == message for row in provider.tool_results[result_count:]), message
        elapsed = time.monotonic() - start
        after = descendants(read_proc(), daemon.pid)
        receipt["turns"].append({"phase": label, "wall_seconds": elapsed,
            "tree_cpu_seconds": sum(p.cpu_seconds - before.get(p.identity, 0.0) for p in after),
            "before_identities": [list(identity) for identity in before],
            "after_identities": [list(p.identity) for p in after]})
    try:
        client = Client(root / "d.sock", daemon)
        observe("daemon_only")
        config = {"cwd": str(root), "agentDir": str(root / "agent"), "provider": "measurement-local",
                  "model": "measure-1", "noExtensions": True, "noContextFiles": True, "telemetryDisabled": True}
        created = client.call("create", name="measure-root", config=config)
        active = created.get("activeSessionId") or created["id"]
        client.call("attach", activeSessionId=active)
        turn(active, "PLAIN_ROOT", "root_plain")
        observe("root_idle")
        turn(active, "CPU_TOOL", "root_tool_cold")
        observe("root_after_first_tool_idle")
        turn(active, "SPAWN_ONE", "spawn_one")
        one = next(row for row in client.call("list", all=True)["sessions"] if row.get("sessionName") == "measure-one")
        first = one.get("activeSessionId") or one["id"]
        client.call("wait_for_idle", activeSessionId=first)
        observe("one_child_idle")
        turn(active, "SPAWN_TWO", "spawn_two")
        two = next(row for row in client.call("list", all=True)["sessions"] if row.get("sessionName") == "measure-two")
        second = two.get("activeSessionId") or two["id"]
        client.call("wait_for_idle", activeSessionId=second)
        observe("two_children_idle")
        for ident, name in ((active, "root"), (first, "child_one"), (second, "child_two")):
            turn(ident, "CPU_TOOL", name + "_tool")
            observe(name + "_warm_idle")
            turn(ident, "PLAIN_FOLLOWUP", name + "_followup")
        receipt["sessions"] = client.call("list", all=True)["sessions"]
        receipt["requests"] = provider.requests
        receipt["tool_results"] = provider.tool_results
        observe("final_idle")
    finally:
        from sample_agent_resources import descendants, read_proc
        owned = descendants(read_proc(), daemon.pid)
        mark("shutdown")
        try:
            if client:
                client.call("shutdown", force=True)
                client.close()
            daemon.wait(timeout=30)
        finally:
            observer_stop.set()
            observer.join(timeout=5)
            receipt["process_commands"] = list(command_shapes.values())
            sampler.terminate()
            sampler.wait(timeout=5)
            log.close()
            provider.shutdown()
            provider.server_close()
            current = read_proc()
            remaining = [p for p in owned if p.pid in current and current[p.pid].identity == p.identity]
            receipt["remaining_owned_pids"] = [p.pid for p in remaining]
            for process in remaining:
                os.kill(process.pid, 15)
            receipt["requests"] = provider.requests
            receipt["tool_results"] = provider.tool_results
            receipt["daemon_exit"] = daemon.poll()
            (root / "receipt.json").write_text(json.dumps(receipt, indent=2))
    assert daemon.returncode == 0 and not remaining, receipt


if __name__ == "__main__":
    main()
