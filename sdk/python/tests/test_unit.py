"""Unit tests: the protocol client against the real `isb rpc` with a bogus
socket, plus fake servers for the failure paths. No incus needed.

    PYTHONPATH=sdk/python/src ISB_BIN=/path/to/isb python3 -m unittest discover -s sdk/python/tests
"""

from __future__ import annotations

import asyncio
import os
import stat
import tempfile
import textwrap
import unittest
from typing import Any, Optional

import isb
from isb._errors import error_from_json
from isb._util import duration

BOGUS_SOCKET = "/nonexistent/isb-sdk-py-test.socket"


def _have_binary() -> Optional[str]:
    try:
        return isb.find_binary()
    except isb.BinaryNotFoundError:
        return None


def fake_server(tmp: str, body: str) -> str:
    """A shell script standing in for the isb binary."""
    path = os.path.join(tmp, "fake-isb")
    with open(path, "w") as f:
        f.write("#!/bin/sh\n" + textwrap.dedent(body))
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
    return path


class PureTests(unittest.TestCase):
    def test_volume_builders(self) -> None:
        self.assertEqual(isb.Volume.bind("./src"), {"bind": "./src"})
        self.assertEqual(
            isb.Volume.bind("/srv", readonly=True, device="src", options={"shift": True}),
            {"bind": "/srv", "readonly": True, "device": "src", "options": {"shift": True}},
        )
        self.assertEqual(isb.Volume.named("cache"), {"named": "cache"})
        self.assertEqual(
            isb.Volume.named("cache", mode=isb.NamedVolumeMode.EXISTING, owner="dev", pool="p", readonly=True),
            {"named": "cache", "external": True, "owner": "dev", "readonly": True, "pool": "p"},
        )

    def test_port_builders(self) -> None:
        self.assertEqual(
            isb.PortBinding.host("tcp:127.0.0.1:5173", "tcp:127.0.0.1:5173", name="vite", search=50),
            {
                "bind": "host",
                "listen": "tcp:127.0.0.1:5173",
                "connect": "tcp:127.0.0.1:5173",
                "name": "vite",
                "search": 50,
            },
        )
        self.assertEqual(
            isb.PortBinding.guest("tcp:127.0.0.1:8190", "tcp:127.0.0.1:8080"),
            {"bind": "guest", "listen": "tcp:127.0.0.1:8190", "connect": "tcp:127.0.0.1:8080"},
        )

    def test_duration(self) -> None:
        self.assertIsNone(duration(None))
        self.assertEqual(duration(1.5), "1500ms")
        self.assertEqual(duration(2), "2000ms")
        self.assertEqual(duration("90s"), "90s")
        with self.assertRaises(ValueError):
            duration(-1)

    def test_error_mapping(self) -> None:
        cases = {
            "not_found": isb.NotFoundError,
            "already_exists": isb.AlreadyExistsError,
            "not_ready": isb.NotReadyError,
            "request_timeout": isb.IsbTimeoutError,
            "operation_timeout": isb.IsbTimeoutError,
            "exec_timeout": isb.IsbTimeoutError,
            "invalid": isb.InvalidError,
            "interpolation": isb.InvalidError,
            "parse": isb.InvalidError,
            "connect": isb.ConnectError,
            "api": isb.ApiError,
            "protocol": isb.ProtocolError,
            "bad_request": isb.ProtocolError,
            "websocket": isb.IsbError,
        }
        for code, cls in cases.items():
            e = error_from_json({"code": code, "message": "m", "data": {"k": 1}})
            self.assertIs(type(e), cls, code)
            self.assertEqual((e.code, e.message, e.data), (code, "m", {"k": 1}))
        self.assertFalse(issubclass(isb.IsbTimeoutError, TimeoutError))

    def test_exec_output(self) -> None:
        o = isb.ExecOutput(0, b"hi\n", b"\xff")
        self.assertTrue(o.success)
        self.assertEqual(o.stdout_text, "hi\n")
        self.assertEqual(o.stderr_text, "�")
        self.assertFalse(isb.ExecOutput(1, b"", b"").success)

    def test_find_binary_order(self) -> None:
        old = os.environ.get("ISB_BIN")
        try:
            os.environ["ISB_BIN"] = "/from/env"
            self.assertEqual(isb.find_binary("/explicit"), "/explicit")
            self.assertEqual(isb.find_binary(), "/from/env")
        finally:
            if old is None:
                os.environ.pop("ISB_BIN", None)
            else:
                os.environ["ISB_BIN"] = old


class FakeServerTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    async def test_refuses_other_protocol(self) -> None:
        path = fake_server(
            self.tmp.name,
            """\
            echo '{"isb":"9.9.9","protocol":2}'
            cat >/dev/null
            """,
        )
        c = isb.Client(isb_bin=path)
        with self.assertRaises(isb.ProtocolError) as cm:
            await c.start()
        self.assertIn("protocol 2", str(cm.exception))
        await c.close()

    async def test_binary_without_rpc(self) -> None:
        path = fake_server(
            self.tmp.name,
            """\
            echo "error: unrecognized subcommand 'rpc'" >&2
            exit 2
            """,
        )
        c = isb.Client(isb_bin=path)
        with self.assertRaises(isb.ProcessError) as cm:
            await c.call("version")
        self.assertIn("unrecognized subcommand", str(cm.exception))
        self.assertEqual(cm.exception.data and cm.exception.data["exit_code"], 2)
        await c.close()

    async def test_missing_binary(self) -> None:
        c = isb.Client(isb_bin=os.path.join(self.tmp.name, "does-not-exist"))
        with self.assertRaises(isb.ProcessError):
            await c.call("version")
        await c.close()

    async def test_death_fails_pending_requests(self) -> None:
        # Says hello, reads one request, dies without answering.
        path = fake_server(
            self.tmp.name,
            """\
            echo '{"isb":"0.4.0","protocol":1}'
            read line
            echo "boom" >&2
            exit 3
            """,
        )
        c = isb.Client(isb_bin=path)
        with self.assertRaises(isb.ProcessError) as cm:
            await asyncio.wait_for(c.call("version"), 10)
        self.assertIn("exited (status 3)", str(cm.exception))
        self.assertIn("boom", str(cm.exception))
        # Later calls fail the same way instead of hanging.
        with self.assertRaises(isb.ProcessError):
            await asyncio.wait_for(c.call("version"), 10)
        self.assertFalse(c.running)
        await c.close()

    async def test_close_fails_pending(self) -> None:
        path = fake_server(
            self.tmp.name,
            """\
            echo '{"isb":"0.4.0","protocol":1}'
            cat >/dev/null
            """,
        )
        c = isb.Client(isb_bin=path)
        task = asyncio.ensure_future(c.call("version"))
        await asyncio.sleep(0.2)
        await c.close()
        with self.assertRaises(isb.ProcessError):
            await asyncio.wait_for(task, 10)
        with self.assertRaises(isb.ProcessError):
            await c.call("version")

    async def test_events_and_out_of_order_replies(self) -> None:
        # Answers two requests in reverse order, with an event and a stray line.
        path = fake_server(
            self.tmp.name,
            """\
            echo '{"isb":"0.4.0","protocol":1}'
            read a
            read b
            echo 'not json'
            echo '{"id":null,"error":{"code":"bad_request","message":"x"}}'
            echo '{"id":2,"event":"progress","data":"two: working"}'
            echo '{"id":2,"result":"second"}'
            echo '{"id":1,"error":{"code":"not_found","message":"sandbox x not found"}}'
            cat >/dev/null
            """,
        )
        async with isb.Client(isb_bin=path) as c:
            events: list[tuple[str, Any]] = []
            t1 = asyncio.ensure_future(c.call("sandbox.get", {"name": "x"}))
            await asyncio.sleep(0.05)
            t2 = asyncio.ensure_future(c.call("version", on_event=lambda e, d: events.append((e, d))))
            self.assertEqual(await asyncio.wait_for(t2, 10), "second")
            with self.assertRaises(isb.NotFoundError):
                await asyncio.wait_for(t1, 10)
            self.assertEqual(events, [("progress", "two: working")])


@unittest.skipUnless(_have_binary(), "no isb binary (set ISB_BIN)")
class RpcTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.client = isb.Client(socket=BOGUS_SOCKET)
        await self.client.start()

    async def asyncTearDown(self) -> None:
        await self.client.close()

    async def test_hello_and_version(self) -> None:
        self.assertTrue(self.client.running)
        v = await self.client.version()
        self.assertEqual(v["protocol"], isb.PROTOCOL)
        self.assertEqual(v["isb"], self.client.server_version)
        schema = await self.client.schema()
        self.assertIn("SandboxSpec", schema["$defs"])

    async def test_argv_flags(self) -> None:
        c = isb.Client(isb_bin="/x/isb", socket="/s", project="p", create_timeout=90)
        self.assertEqual(c.argv(), ["/x/isb", "--socket", "/s", "--project", "p", "--create-timeout", "90000ms", "rpc"])

    async def test_connect_error(self) -> None:
        with self.assertRaises(isb.ConnectError) as cm:
            await isb.Sandbox.get("web", client=self.client)
        self.assertEqual(cm.exception.code, "connect")
        self.assertEqual(cm.exception.data, {"socket": BOGUS_SOCKET})

    async def test_unknown_method(self) -> None:
        with self.assertRaises(isb.ProtocolError) as cm:
            await self.client.call("no.such.method")
        self.assertEqual(cm.exception.code, "protocol")

    async def test_bad_params(self) -> None:
        with self.assertRaises(isb.InvalidError):
            await self.client.call("sandbox.get", {"nam": "x"})

    async def test_concurrent_requests(self) -> None:
        calls = []
        for i in range(60):
            if i % 3 == 0:
                calls.append(self.client.call("version"))
            elif i % 3 == 1:
                calls.append(self.client.call("sandbox.get", {"name": f"s{i}"}))
            else:
                calls.append(self.client.call("no.such"))
        results = await asyncio.gather(*calls, return_exceptions=True)
        for i, r in enumerate(results):
            if i % 3 == 0:
                self.assertEqual(r["protocol"], 1)  # type: ignore[index]
            elif i % 3 == 1:
                self.assertIsInstance(r, isb.ConnectError)
            else:
                self.assertIsInstance(r, isb.ProtocolError)

    async def test_spec_unknown_field_is_invalid(self) -> None:
        with self.assertRaises(isb.InvalidError) as cm:
            await isb.Sandbox.plan("x", image="dev-base", client=self.client, **{"cpu": 2})  # type: ignore[arg-type]
        self.assertIn("cpu", str(cm.exception))
        with self.assertRaises(isb.InvalidError):
            await isb.Sandbox.create(
                "x",
                image="dev-base",
                client=self.client,
                type="bogus",  # type: ignore[arg-type]
            )

    async def test_exec_needs_connection(self) -> None:
        sb = isb.Sandbox("web", client=self.client, exec_defaults={"user": "dev"})
        with self.assertRaises(isb.ConnectError):
            await sb.exec(["true"], timeout=5, env={"A": 1})
        p = await sb.exec_stream("true")
        with self.assertRaises(isb.ConnectError):
            async for _ in p:
                pass
        with self.assertRaises(TypeError):
            await sb.exec(["a"], ["b"])

    async def test_subprocess_killed(self) -> None:
        proc = self.client._proc
        assert proc is not None
        proc.kill()
        await proc.wait()
        with self.assertRaises(isb.ProcessError):
            await asyncio.wait_for(self.client.call("version"), 10)

    async def test_compose_load_with_vars(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "src"))
            f = os.path.join(d, "isb.yaml")
            with open(f, "w") as fh:
                fh.write(
                    textwrap.dedent("""\
                    sandboxes:
                      web:
                        image: "${IMG}"
                        cpus: 2
                        volumes:
                          /src: {bind: ./src}
                        exec: {user: dev, cwd: /src}
                      Worker_1:
                        image: "${IMG}"
                    """)
                )
            p = await isb.Project.load(f, vars={"IMG": "dev-base"}, project_name="Demo App", client=self.client)
            self.assertEqual(p.name, "demo-app")
            self.assertEqual(sorted(p.services), ["Worker_1", "web"])
            web = p.file["sandboxes"]["web"]
            self.assertEqual(web["image"], "dev-base")
            self.assertEqual(web["name"], "demo-app-web")
            sb = p.sandbox("web")
            self.assertEqual(sb.name, "demo-app-web")
            defaults = sb.exec_defaults or {}
            self.assertEqual((defaults.get("user"), defaults.get("cwd")), ("dev", "/src"))
            self.assertEqual(p.sandbox("Worker_1").name, "demo-app-worker-1")
            with self.assertRaises(isb.NotFoundError):
                p.sandbox("nope")
            with self.assertRaises(isb.InvalidError) as cm:
                await isb.Project.load(f, client=self.client)
            # The server reports it as `parse` (naming the file) wrapping the interpolation error.
            self.assertIn(cm.exception.code, ("interpolation", "parse"))
            self.assertIn("IMG", str(cm.exception))
            with self.assertRaises(isb.InvalidError) as cm:
                await isb.Project.load(os.path.join(d, "missing.yaml"), client=self.client)

    async def test_default_client(self) -> None:
        c1 = isb.default_client()
        self.assertIs(c1, isb.default_client())
        try:
            v = await isb.Sandbox.list_with({"x": None}, client=None)
        except isb.ConnectError:
            pass  # no incus here (or no access): still went through the default client
        else:
            self.assertIsInstance(v, list)
        self.assertTrue(c1.running)
        await c1.close()
        self.assertIsNot(c1, isb.default_client())


if __name__ == "__main__":
    unittest.main()
