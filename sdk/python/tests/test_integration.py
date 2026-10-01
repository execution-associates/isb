"""Integration tests against a real incusd. Skipped unless ISB_INTEGRATION=1.

    ISB_INTEGRATION=1 ISB_BIN=/path/to/isb PYTHONPATH=sdk/python/src \
        python3 -m unittest discover -s sdk/python/tests -v

Needs a local image with a `dev` user (uid 1000) and python3
(`ISB_TEST_IMAGE`, default `dev-base`). Everything created is named
`isb-test-py-<pid>-...`, labeled `isb-test=py`, and removed afterwards, pass
or fail. Nothing else is touched.
"""

from __future__ import annotations

import asyncio
import os
import random
import tempfile
import textwrap
import time
import unittest

import isb

ENABLED = os.environ.get("ISB_INTEGRATION") == "1"
IMAGE = os.environ.get("ISB_TEST_IMAGE", "dev-base")
PREFIX = f"isb-test-py-{os.getpid()}-"
RUN_LABEL = "isb-test-py-run"
LABELS: dict[str, str] = {"isb-test": "py", RUN_LABEL: str(os.getpid())}
MAIN = PREFIX + "main"


async def _sweep() -> None:
    """Remove anything this run created (by label, and by name prefix for volumes)."""
    async with isb.Client() as c:
        for info in await isb.Sandbox.list_with({RUN_LABEL: str(os.getpid())}, client=c):
            if info.name.startswith(PREFIX):
                try:
                    await isb.Sandbox.remove(info.name, force=True, client=c)
                except isb.NotFoundError:
                    pass
        for v in await isb.volumes.list(client=c):
            if v.name.startswith(PREFIX):
                try:
                    await isb.volumes.remove(v.name, v.pool, client=c)
                except isb.IsbError:
                    pass


@unittest.skipUnless(ENABLED, "set ISB_INTEGRATION=1 (needs incus)")
class IntegrationTests(unittest.IsolatedAsyncioTestCase):
    """Most tests share one container, created once."""

    @classmethod
    def setUpClass(cls) -> None:
        async def create() -> None:
            async with isb.Client() as c:
                await isb.Sandbox.create(
                    MAIN,
                    image=IMAGE,
                    client=c,
                    labels=LABELS,
                    ready=["running", "default_route", {"user_exists": "dev"}],
                )

        try:
            asyncio.run(create())
        except BaseException:
            asyncio.run(_sweep())
            raise

    @classmethod
    def tearDownClass(cls) -> None:
        asyncio.run(_sweep())

    async def asyncSetUp(self) -> None:
        self.client = isb.Client()
        await self.client.start()
        self.sb = isb.Sandbox(MAIN, client=self.client)

    async def asyncTearDown(self) -> None:
        await self.client.close()

    # -- lifecycle ---------------------------------------------------------

    async def test_create_already_exists(self) -> None:
        with self.assertRaises(isb.AlreadyExistsError):
            await isb.Sandbox.create(MAIN, image=IMAGE, client=self.client, labels=LABELS)

    async def test_connect_or_create_noop(self) -> None:
        progress: list[str] = []
        sb = await isb.Sandbox.connect_or_create(
            MAIN,
            image=IMAGE,
            client=self.client,
            labels=LABELS,
            ready=["running", "default_route", {"user_exists": "dev"}],
            on_progress=progress.append,
        )
        r = sb.last_report
        assert r is not None
        self.assertEqual(r.name, MAIN)
        self.assertFalse(r.created)
        self.assertFalse(r.changed, r.applied)
        self.assertEqual(r.restart_needed, [])
        plan = await isb.Sandbox.plan(MAIN, image=IMAGE, client=self.client, labels=LABELS)
        self.assertTrue(plan.is_noop, plan.actions)
        self.assertEqual((plan.status or "").lower(), "running")

    async def test_get_and_list_with(self) -> None:
        sb = await isb.Sandbox.get(MAIN, client=self.client)
        info = await sb.info()
        self.assertTrue(info.running)
        self.assertEqual(info.type, "container")
        self.assertEqual(info.labels, LABELS)
        self.assertEqual(await sb.labels(), LABELS)
        names = [i.name for i in await isb.Sandbox.list_with({RUN_LABEL: str(os.getpid())}, client=self.client)]
        self.assertIn(MAIN, names)
        names = [
            i.name for i in await isb.Sandbox.list_with([f"{RUN_LABEL}={os.getpid()}", "isb-test"], client=self.client)
        ]
        self.assertIn(MAIN, names)
        self.assertEqual(await isb.Sandbox.list_with({RUN_LABEL: "no-such-run"}, client=self.client), [])
        self.assertIn(MAIN, [i.name for i in await isb.Sandbox.list(client=self.client)])

    async def test_not_found(self) -> None:
        with self.assertRaises(isb.NotFoundError):
            await isb.Sandbox.get(PREFIX + "missing", client=self.client)
        with self.assertRaises(isb.NotFoundError):
            await isb.Sandbox(PREFIX + "missing", client=self.client).exec("true")
        with self.assertRaises(isb.NotFoundError):
            await isb.volumes.get(PREFIX + "missing-vol", client=self.client)

    async def test_wait_ready(self) -> None:
        await self.sb.wait_ready(["running", "default_route", {"user_exists": "dev"}], "30s")
        with self.assertRaises(isb.NotReadyError):
            await self.sb.wait_ready([{"user_exists": "no-such-user-xyz"}], 1)

    # -- exec --------------------------------------------------------------

    async def test_exec_captured(self) -> None:
        out = await self.sb.exec(["sh", "-c", "echo out; echo err >&2; exit 3"])
        self.assertEqual((out.exit_code, out.stdout, out.stderr), (3, b"out\n", b"err\n"))
        self.assertFalse(out.success)
        # argv is never joined into a shell string
        out = await self.sb.exec("printf", ["[%s]", "a b", "$HOME", "'q'"])
        self.assertEqual(out.stdout_text, "[a b][$HOME]['q']")
        self.assertTrue(out.success)

    async def test_exec_user_cwd_env(self) -> None:
        out = await self.sb.exec(
            ["sh", "-c", 'id -un; pwd; echo "$FOO"; echo "$HOME"'],
            user="dev",
            cwd="/tmp",
            env={"FOO": "bar baz"},
        )
        self.assertEqual(out.stdout_text.splitlines(), ["dev", "/tmp", "bar baz", "/home/dev"])
        # exec defaults on the handle, overridden per call
        sb = isb.Sandbox(MAIN, client=self.client, exec_defaults={"user": "dev", "cwd": "/home/dev", "env": {"A": "1"}})
        out = await sb.exec(["sh", "-c", 'id -un; pwd; echo "$A$B"'], env={"B": "2"})
        self.assertEqual(out.stdout_text.splitlines(), ["dev", "/home/dev", "12"])
        out = await sb.exec(["id", "-un"], user="root")
        self.assertEqual(out.stdout_text.strip(), "root")

    async def test_exec_stdin(self) -> None:
        data = bytes(range(256)) * 64
        out = await self.sb.exec(["sha256sum"], stdin=data)
        import hashlib

        self.assertEqual(out.stdout_text.split()[0], hashlib.sha256(data).hexdigest())
        out = await self.sb.exec("cat", stdin="héllo")
        self.assertEqual(out.stdout, "héllo".encode())

    async def test_exec_timeout(self) -> None:
        t = time.monotonic()
        with self.assertRaises(isb.IsbTimeoutError) as cm:
            await self.sb.exec(["sleep", "30"], timeout=1)
        self.assertEqual(cm.exception.code, "exec_timeout")
        self.assertLess(time.monotonic() - t, 20)

    async def test_exec_tty(self) -> None:
        out = await self.sb.exec(["tty"], tty=True)
        self.assertTrue(out.stdout_text.startswith("/dev/pts/"), out)
        out = await self.sb.exec(["tty"])
        self.assertNotEqual(out.exit_code, 0)

    async def test_exec_stream_first_chunk_before_exit(self) -> None:
        async with await self.sb.exec_stream(["sh", "-c", "echo first; sleep 3; echo second >&2"]) as p:
            t = time.monotonic()
            first = await asyncio.wait_for(p.__anext__(), 10)
            self.assertEqual(first, isb.ExecEvent("stdout", b"first\n"))
            self.assertFalse(p.done)
            self.assertLess(time.monotonic() - t, 2.5)
            rest = [ev async for ev in p]
            self.assertEqual(rest, [isb.ExecEvent("stderr", b"second\n")])
            self.assertEqual(await p.wait(), 0)

    async def test_exec_stream_piped_stdin(self) -> None:
        p = await self.sb.exec_stream(["cat"], stdin="piped")
        await p.write(b"hello ")
        await p.write("world\n")
        await p.close_stdin()
        out = await asyncio.wait_for(p.collect(), 30)
        self.assertEqual((out.exit_code, out.stdout, out.stderr), (0, b"hello world\n", b""))

    async def test_exec_stream_signal(self) -> None:
        p = await self.sb.exec_stream(["sh", "-c", "trap 'exit 42' TERM; echo ready; while :; do sleep 0.1; done"])
        async for ev in p:
            self.assertEqual(ev.data, b"ready\n")
            break
        await p.signal(15)
        self.assertEqual(await asyncio.wait_for(p.wait(), 30), 42)

    async def test_exec_stream_context_kills(self) -> None:
        async with await self.sb.exec_stream(["sleep", "300"]) as p:
            pass
        self.assertTrue(p.done)

    async def test_concurrent_execs(self) -> None:
        outs = await asyncio.gather(*(self.sb.exec(["sh", "-c", f"sleep 0.5; echo {i}"]) for i in range(8)))
        self.assertEqual([o.stdout_text.strip() for o in outs], [str(i) for i in range(8)])

    # -- devices -----------------------------------------------------------

    async def test_add_port_and_remove_device(self) -> None:
        base = random.randint(20000, 60000)
        dev = "isb-test-py-port"
        try:
            listen = await self.sb.add_port(
                isb.PortBinding.host(f"tcp:127.0.0.1:{base}", "tcp:127.0.0.1:80", name=dev, search=20)
            )
            self.assertTrue(listen.startswith("tcp:127.0.0.1:"), listen)
            port = int(listen.rsplit(":", 1)[1])
            self.assertTrue(base <= port <= base + 20)
            info = await self.sb.info()
            self.assertEqual(info.devices[dev]["listen"], listen)
            # adding it again leaves the correct device alone
            again = await self.sb.add_port(
                isb.PortBinding.host(f"tcp:127.0.0.1:{base}", "tcp:127.0.0.1:80", name=dev, search=20)
            )
            self.assertEqual(again, listen)
        finally:
            removed = await self.sb.remove_device(dev)
        self.assertTrue(removed)
        self.assertFalse(await self.sb.remove_device(dev))
        self.assertNotIn(dev, (await self.sb.info()).devices)


@unittest.skipUnless(ENABLED, "set ISB_INTEGRATION=1 (needs incus)")
class OwnSandboxTests(unittest.IsolatedAsyncioTestCase):
    """Tests that need their own sandboxes."""

    async def asyncSetUp(self) -> None:
        self.client = isb.Client()
        await self.client.start()
        self.addAsyncCleanup(self.client.close)

    async def _remove(self, name: str) -> None:
        try:
            await isb.Sandbox.remove(name, force=True, client=self.client)
        except isb.NotFoundError:
            pass

    async def test_named_volume_owner_lifecycle_remove(self) -> None:
        name = PREFIX + "vol"
        vol = PREFIX + "cache"
        self.addAsyncCleanup(self._remove_volume, vol)
        self.addAsyncCleanup(self._remove, name)
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "hello.txt"), "w") as f:
                f.write("hi")
            sb = await isb.Sandbox.create(
                name,
                image=IMAGE,
                client=self.client,
                labels=LABELS,
                idmap="auto",
                volumes=[
                    isb.Volume.named(vol, "/home/dev/.cache/thing", owner="dev"),
                    isb.Volume.bind(d, "/mnt/ro", read_only=True, device="ro"),
                ],
                user="dev",
                ready=["running", {"path_writable": "/home/dev/.cache/thing"}],
            )
            script = "stat -c %U /home/dev/.cache/thing /home/dev/.cache; touch /home/dev/.cache/thing/x && echo ok"
            out = await sb.exec(["sh", "-c", script + "; cat /mnt/ro/hello.txt"])
            self.assertEqual(out.stdout_text.splitlines(), ["dev", "dev", "ok", "hi"], out)
            v = await isb.volumes.get(vol, client=self.client)
            self.assertEqual(v.name, vol)
            self.assertTrue(any(name in u for u in v.used_by), v.used_by)

            await sb.stop(force=True, timeout=10)
            self.assertFalse((await sb.info()).running)
            await sb.start()
            self.assertTrue((await sb.info()).running)
            await sb.restart()
            await sb.wait_ready()  # the spec's checks, as exec user dev

            await sb.remove(force=True)
        with self.assertRaises(isb.NotFoundError):
            await isb.Sandbox.get(name, client=self.client)

    async def _remove_volume(self, vol: str) -> None:
        try:
            await isb.volumes.remove(vol, client=self.client)
        except isb.NotFoundError:
            pass

    async def test_volumes_api(self) -> None:
        vol = PREFIX + "plain"
        self.addAsyncCleanup(self._remove_volume, vol)
        r = await isb.volumes.create(vol, config={"size": "64MiB"}, client=self.client)
        self.assertTrue(r["created"])
        r2 = await isb.Volumes(self.client).create(vol)
        self.assertFalse(r2["created"])
        self.assertIn(vol, [v.name for v in await isb.volumes.list(r["pool"], client=self.client)])
        await isb.volumes.remove(vol, client=self.client)
        with self.assertRaises(isb.NotFoundError):
            await isb.volumes.get(vol, client=self.client)

    async def test_project_up_plan_down(self) -> None:
        pname = PREFIX + "proj"
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "src"))
            with open(os.path.join(d, "src", "marker"), "w") as f:
                f.write("from host")
            compose = os.path.join(d, "isb.yaml")
            with open(compose, "w") as fh:
                fh.write(
                    textwrap.dedent("""\
                    services:
                      web:
                        image: "${IMG}"
                        idmap: auto
                        labels: [isb-test=py, "isb-test-py-run=${RUN}"]
                        volumes:
                          - ./src:/home/dev/src:device=src
                        ready: [running, {user_exists: dev}]
                        user: dev
                        working_dir: /home/dev/src
                        exec: {env: {GREETING: "${GREETING:-hello}"}}
                    """)
                )
            project = await isb.Project.load(
                compose, vars={"IMG": IMAGE, "RUN": str(os.getpid())}, project_name=pname, client=self.client
            )
            web_name = project.file["services"]["web"]["container_name"]
            self.assertEqual(web_name, f"{pname}-web")
            self.addAsyncCleanup(self._remove, str(web_name))

            plans = await project.plan()
            self.assertEqual(len(plans), 1)
            self.assertIsNone(plans[0].status)
            self.assertFalse(plans[0].is_noop)

            progress: list[str] = []
            reports = await project.up(on_progress=progress.append)
            self.assertEqual([s for s, _ in reports], ["web"])
            self.assertTrue(reports[0][1].created)
            self.assertTrue(any("creating" in p for p in progress), progress)

            web = project.sandbox("web")
            out = await web.exec(["sh", "-c", 'id -un; pwd; cat marker; echo " $GREETING"'])
            self.assertEqual(out.stdout_text, "dev\n/home/dev/src\nfrom host hello\n")

            again = await project.up(["web"])
            self.assertFalse(again[0][1].created)
            self.assertFalse(again[0][1].changed, again[0][1].applied)
            self.assertTrue((await project.plan(["web"]))[0].is_noop)

            await project.down(on_progress=progress.append)
            with self.assertRaises(isb.NotFoundError):
                await web.info()


if __name__ == "__main__":
    unittest.main()
