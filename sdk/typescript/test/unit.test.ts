// Unit tests: no incus needed. They run the real `isb rpc` ($ISB_BIN, else
// isb on PATH) against a socket that does not exist, plus small fake servers.
//
//   ISB_BIN=/path/to/isb bun test test/unit.test.ts

import { afterAll, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execParams, toArgv } from "../src/exec.js";
import {
  Client,
  ClientClosedError,
  ConnectError,
  errorFromRpc,
  findIsb,
  InvalidError,
  IsbError,
  IsbTimeoutError,
  NamedVolumeMode,
  NotFoundError,
  PortBinding,
  ProcessExitedError,
  Project,
  ProtocolError,
  planHasChanges,
  Sandbox,
  Volume,
  volumes,
} from "../src/index.js";
import { b64decode, b64encode, durationParam } from "../src/util.js";

const BOGUS = "/nonexistent/isb-sdk-ts-test.socket";
const tmp = mkdtempSync(join(tmpdir(), "isb-sdk-ts-unit-"));
const clients: Client[] = [];

function client(extra: ConstructorParameters<typeof Client>[0] = {}): Client {
  const c = new Client({ socket: BOGUS, ...extra });
  clients.push(c);
  return c;
}

/** A fake `isb` that runs the given shell script body. */
function fake(name: string, body: string): string {
  const p = join(tmp, name);
  writeFileSync(p, `#!/bin/sh\n${body}\n`);
  chmodSync(p, 0o755);
  return p;
}

afterAll(async () => {
  await Promise.all(clients.map((c) => c.close().catch(() => {})));
  rmSync(tmp, { recursive: true, force: true });
});

describe("client", () => {
  test("hello and version", async () => {
    const c = client();
    const hello = await c.hello();
    expect(hello.protocol).toBe(1);
    expect(typeof hello.isb).toBe("string");
    expect(await c.version()).toEqual(hello);
    expect(c.pid).toBeGreaterThan(0);
  });

  test("Bun.spawn spawner", async () => {
    const c = client({ spawner: "bun" });
    expect((await c.version()).protocol).toBe(1);
    const e = await Sandbox.get("x", { client: c }).catch((e) => e);
    expect(e).toBeInstanceOf(ConnectError);
    await c.close();
  });

  test("argv puts global flags before rpc", () => {
    const c = new Client({
      isbBin: "/x/isb",
      socket: "/s",
      project: "p",
      createTimeout: 600,
    });
    expect(c.argv()).toEqual([
      "/x/isb",
      "--socket",
      "/s",
      "--project",
      "p",
      "--create-timeout",
      "600",
      "rpc",
    ]);
  });

  test("schema has the spec definitions", async () => {
    const s = await client().schema();
    expect(Object.keys(s.$defs ?? {})).toContain("SandboxSpec");
  });

  test("unreachable incusd is ConnectError", async () => {
    const c = client();
    const e = await Sandbox.get("x", { client: c }).catch((e) => e);
    expect(e).toBeInstanceOf(ConnectError);
    expect(e).toBeInstanceOf(IsbError);
    expect(e.code).toBe("connect");
    expect(e.data.socket).toBe(BOGUS);
  });

  test("unknown method is ProtocolError", async () => {
    const e = await client()
      .request<null>("no.such.method")
      .catch((e) => e);
    expect(e).toBeInstanceOf(ProtocolError);
    expect(e.code).toBe("protocol");
  });

  test("bad params are InvalidError", async () => {
    const e = await client()
      .request("sandbox.get", { nam: "x" })
      .catch((e) => e);
    expect(e).toBeInstanceOf(InvalidError);
  });

  test("spec with an unknown field is InvalidError", async () => {
    const c = client();
    const spec = { name: "x", image: "dev-base", cpu: 2 } as unknown as Parameters<
      typeof Sandbox.plan
    >[0];
    const e = await Sandbox.plan(spec, { client: c }).catch((e) => e);
    expect(e).toBeInstanceOf(InvalidError);
    expect(e.message).toContain("cpu");
  });

  test("exec control on a finished exec is NotFoundError", async () => {
    const e = await client()
      .request("exec.signal", { exec: 424242, signal: 15 })
      .catch((e) => e);
    expect(e).toBeInstanceOf(NotFoundError);
  });

  test("concurrent requests are matched by id", async () => {
    const c = client();
    const calls: Promise<unknown>[] = [];
    for (let i = 0; i < 30; i++) {
      if (i % 3 === 0) calls.push(c.version().then((v) => ["version", v.protocol]));
      else if (i % 3 === 1)
        calls.push(Sandbox.get(`s${i}`, { client: c }).catch((e) => ["get", e.code]));
      else calls.push(c.request("nope").catch((e) => ["nope", e.code]));
    }
    const got = await Promise.all(calls);
    got.forEach((g, i) => {
      if (i % 3 === 0) expect(g).toEqual(["version", 1]);
      else if (i % 3 === 1) expect(g).toEqual(["get", "connect"]);
      else expect(g).toEqual(["nope", "protocol"]);
    });
  });

  test("replies out of order are matched by id", async () => {
    // Answers request 1 after request 2.
    const bin = fake(
      "reorder",
      [
        `echo '{"isb":"0","protocol":1}'`,
        "read a; read b",
        `echo '{"id":2,"event":"progress","data":"two"}'`,
        `echo '{"id":2,"result":"second"}'`,
        `echo '{"id":1,"result":"first"}'`,
        "read c",
      ].join("\n"),
    );
    const c = client({ isbBin: bin });
    const seen: unknown[] = [];
    const [a, b] = await Promise.all([
      c.request("a"),
      c.request("b", {}, (ev, d) => seen.push([ev, d])),
    ]);
    expect([a, b]).toEqual(["first", "second"]);
    expect(seen).toEqual([["progress", "two"]]);
  });

  test("an unknown protocol version is refused", async () => {
    const bin = fake("proto2", `echo '{"isb":"9.9.9","protocol":2}'\nread x`);
    const e = await client({ isbBin: bin })
      .version()
      .catch((e) => e);
    expect(e).toBeInstanceOf(ProtocolError);
    expect(e.message).toContain("protocol 2");
  });

  test("a binary without rpc fails clearly", async () => {
    const bin = fake("norpc", `echo "error: unrecognized subcommand 'rpc'" >&2\nexit 2`);
    const e = await client({ isbBin: bin })
      .version()
      .catch((e) => e);
    expect(e).toBeInstanceOf(ProcessExitedError);
    expect(e.message).toContain("unrecognized subcommand");
    expect(e.data.exit_code).toBe(2);
  });

  test("a missing binary fails clearly", async () => {
    const e = await client({ isbBin: join(tmp, "does-not-exist") })
      .version()
      .catch((e) => e);
    expect(e).toBeInstanceOf(ProcessExitedError);
  });

  test("subprocess death rejects pending requests, then restarts", async () => {
    const bin = fake("dies", `echo '{"isb":"0","protocol":1}'\nread line\necho "boom" >&2\nexit 3`);
    const c = client({ isbBin: bin });
    const e = await c.request<null>("sandbox.get", { name: "x" }).catch((e) => e);
    expect(e).toBeInstanceOf(ProcessExitedError);
    expect(e.message).toContain("status 3");
    expect(e.message).toContain("boom");
    // The next request starts a fresh server.
    const e2 = await c.request("x").catch((e) => e);
    expect(e2).toBeInstanceOf(ProcessExitedError);
  });

  test("killing the real server rejects a pending request", async () => {
    // A server whose request never returns: isb rpc running `sleep` would need
    // incus, so kill it while a request is in flight instead.
    const bin = fake("hang", `echo '{"isb":"0","protocol":1}'\nread line\nsleep 30`);
    const c = client({ isbBin: bin });
    const p = c.request<null>("anything").catch((e) => e);
    await new Promise((r) => setTimeout(r, 100));
    process.kill(c.pid as number, "SIGKILL");
    const e = await p;
    expect(e).toBeInstanceOf(ProcessExitedError);
    expect(e.message).toContain("SIGKILL");
  });

  test("close, then requests fail with ClientClosedError", async () => {
    const c = client();
    await c.version();
    await c.close();
    expect(c.closed).toBe(true);
    const e = await c.version().catch((e) => e);
    expect(e).toBeInstanceOf(ClientClosedError);
  });

  test("asyncDispose closes", async () => {
    let c: Client | undefined;
    {
      await using d = new Client({ socket: BOGUS });
      c = d;
      await d.version();
    }
    expect(c.closed).toBe(true);
  });

  test("findIsb prefers the explicit path, then ISB_BIN", () => {
    expect(findIsb("/explicit/isb")).toBe("/explicit/isb");
    const old = process.env.ISB_BIN;
    process.env.ISB_BIN = "/env/isb";
    try {
      expect(findIsb()).toBe("/env/isb");
    } finally {
      if (old === undefined) delete process.env.ISB_BIN;
      else process.env.ISB_BIN = old;
    }
  });
});

describe("compose", () => {
  test("compose.load with vars", async () => {
    const dir = mkdtempSync(join(tmp, "compose-"));
    const f = join(dir, "isb.yaml");
    writeFileSync(
      f,
      [
        "sandboxes:",
        "  web:",
        '    image: "${IMG}"',
        "    cpus: 2",
        "    exec: {user: dev, cwd: /home/dev}",
        "  Worker_1:",
        "    image: dev-base",
        "",
      ].join("\n"),
    );
    const c = client();
    const p = await Project.load({
      files: [f],
      vars: { IMG: "dev-base" },
      projectName: "demo",
      client: c,
    });
    expect(p.name).toBe("demo");
    expect(p.services.sort()).toEqual(["Worker_1", "web"]);
    expect(p.file.sandboxes?.web?.image).toBe("dev-base");
    expect(p.spec("Worker_1").name).toBe("demo-worker-1");
    const sb = p.sandbox("web");
    expect(sb.name).toBe("demo-web");
    expect(sb.execDefaults).toEqual({ user: "dev", cwd: "/home/dev" });
    expect(() => p.sandbox("nope")).toThrow(NotFoundError);
  });

  test("unset variable is InvalidError", async () => {
    const dir = mkdtempSync(join(tmp, "compose-"));
    const f = join(dir, "isb.yaml");
    writeFileSync(f, 'sandboxes:\n  web: {image: "${ISB_SDK_TS_SURELY_UNSET}"}\n');
    const e = await Project.load({ files: [f], client: client() }).catch((e) => e);
    expect(e).toBeInstanceOf(InvalidError);
    expect(e.message).toContain("ISB_SDK_TS_SURELY_UNSET");
  });

  test("missing file is InvalidError (parse)", async () => {
    const e = await Project.load({ files: [join(tmp, "nope.yaml")], client: client() }).catch(
      (e) => e,
    );
    expect(e).toBeInstanceOf(InvalidError);
  });
});

describe("builders", () => {
  test("Volume.bind", () => {
    expect(Volume.bind("./src")).toEqual({ bind: "./src" });
    expect(
      Volume.bind("/srv/ref", { readonly: true, device: "ref", options: { shift: "true" } }),
    ).toEqual({ bind: "/srv/ref", readonly: true, device: "ref", options: { shift: "true" } });
  });

  test("Volume.named", () => {
    expect(Volume.named("cache")).toEqual({ named: "cache" });
    expect(Volume.named("cache", { owner: "dev", pool: "default", device: "c" })).toEqual({
      named: "cache",
      owner: "dev",
      pool: "default",
      device: "c",
    });
    expect(Volume.named("data", { mode: NamedVolumeMode.Existing, readonly: true })).toEqual({
      named: "data",
      readonly: true,
      external: true,
    });
    expect(Volume.named("x", { mode: NamedVolumeMode.EnsureExists })).toEqual({ named: "x" });
  });

  test("PortBinding", () => {
    expect(PortBinding.host("tcp:127.0.0.1:5173", "tcp:127.0.0.1:5173", { search: 50 })).toEqual({
      bind: "host",
      listen: "tcp:127.0.0.1:5173",
      connect: "tcp:127.0.0.1:5173",
      search: 50,
    });
    expect(
      PortBinding.guest("tcp:127.0.0.1:8190", "tcp:127.0.0.1:8080", { name: "backend" }),
    ).toEqual({
      bind: "guest",
      listen: "tcp:127.0.0.1:8190",
      connect: "tcp:127.0.0.1:8080",
      name: "backend",
    });
  });

  test("builders produce specs isb accepts", async () => {
    // plan validates the spec before it needs incus; a valid spec reaches
    // the connect error, an invalid one would be InvalidError.
    const spec = {
      name: "isb-sdk-ts-unit",
      image: "dev-base",
      volumes: {
        "/home/dev/src": Volume.bind("/", { device: "src" }),
        "/home/dev/.cache": Volume.named("c", { owner: "dev" }),
      },
      ports: [PortBinding.host("tcp:127.0.0.1:5173", "tcp:127.0.0.1:5173", { search: 5 })],
    };
    const e = await Sandbox.plan(spec, { client: client() }).catch((e) => e);
    expect(e).toBeInstanceOf(ConnectError);
  });
});

describe("helpers", () => {
  test("argv forms", () => {
    expect(toArgv("ls")).toEqual(["ls"]);
    expect(toArgv("ls", ["-l", "a b"])).toEqual(["ls", "-l", "a b"]);
    expect(toArgv(["sh", "-c", "echo hi"])).toEqual(["sh", "-c", "echo hi"]);
    expect(() => toArgv([])).toThrow();
  });

  test("exec params", () => {
    expect(
      execParams(
        "web",
        ["cat"],
        { user: "dev" },
        { stdin: "hi", timeout: 5, user: 1000, env: { A: "1" }, tty: true },
        false,
      ),
    ).toEqual({
      name: "web",
      argv: ["cat"],
      defaults: { user: "dev" },
      user: "1000",
      env: { A: "1" },
      tty: true,
      timeout: "5",
      stdin: { data: "aGk=" },
    });
    expect(execParams("web", ["cat"], undefined, { stdin: "piped" }, true)).toEqual({
      name: "web",
      argv: ["cat"],
      stdin: "piped",
      stream: true,
    });
  });

  test("base64 and durations", () => {
    const bytes = new Uint8Array([0, 255, 128, 7]);
    expect(b64decode(b64encode(bytes))).toEqual(bytes);
    expect(b64encode("foobar")).toBe("Zm9vYmFy");
    expect(durationParam(90)).toBe("90");
    expect(durationParam("5m")).toBe("5m");
    expect(durationParam(undefined)).toBeUndefined();
    expect(() => durationParam(-1)).toThrow();
  });

  test("error mapping", () => {
    expect(errorFromRpc({ code: "exec_timeout", message: "m" })).toBeInstanceOf(IsbTimeoutError);
    expect(errorFromRpc({ code: "operation_timeout", message: "m" })).toBeInstanceOf(
      IsbTimeoutError,
    );
    expect(errorFromRpc({ code: "parse", message: "m" })).toBeInstanceOf(InvalidError);
    const e = errorFromRpc({ code: "api", message: "m", data: { status: 500 } });
    expect(e.constructor).toBe(IsbError);
    expect(e.data).toEqual({ status: 500 });
    expect(e.name).toBe("IsbError");
  });

  test("planHasChanges", () => {
    expect(planHasChanges({ name: "x", status: null, actions: [] })).toBe(false);
    expect(
      planHasChanges({ name: "x", status: "Running", actions: [{ action: "note", message: "m" }] }),
    ).toBe(false);
    expect(
      planHasChanges({ name: "x", status: null, actions: [{ action: "start_instance" }] }),
    ).toBe(true);
  });

  test("volume helpers reach the server", async () => {
    const e = await volumes.list({ client: client() }).catch((e) => e);
    expect(e).toBeInstanceOf(ConnectError);
  });
});
