// Integration tests against the real incusd. Skipped unless ISB_INTEGRATION=1.
// They need the incus socket (root-equivalent): run them on the host, never
// in a sandbox that should not have it.
//
//   ISB_INTEGRATION=1 ISB_BIN=/path/to/isb bun test test/integration.test.ts
//
// Everything they create is named isb-test-ts-<pid>-... and is removed in
// afterAll, pass or fail. The image is $ISB_TEST_IMAGE (default dev-base: a
// local image with user `dev`, uid 1000).

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  AlreadyExistsError,
  Client,
  type ExecEvent,
  IsbTimeoutError,
  NotFoundError,
  PortBinding,
  Project,
  planHasChanges,
  Sandbox,
  type SandboxSpec,
  Volume,
  volumes,
} from "../src/index.js";

const enabled = process.env.ISB_INTEGRATION === "1";
const IMAGE = process.env.ISB_TEST_IMAGE ?? "dev-base";
const PREFIX = `isb-test-ts-${process.pid}`;
const T = 180_000; // per-test timeout: creates take ~5-10s, more on a busy host

const text = (b: Uint8Array) => new TextDecoder().decode(b);

describe.skipIf(!enabled)("integration", () => {
  let client: Client;
  let tmp: string;
  const spec: SandboxSpec = {
    name: `${PREFIX}-a`,
    image: IMAGE,
    labels: { "isb-test": "ts", "isb-test-run": `${process.pid}` },
    env: { FROM_SPEC: "yes" },
    ready: ["running", "default_route", { user_exists: "dev" }],
    ready_timeout: "90s",
  };

  beforeAll(() => {
    client = new Client();
    tmp = mkdtempSync(join(tmpdir(), "isb-sdk-ts-it-"));
  });

  afterAll(async () => {
    const c = new Client();
    try {
      for (const s of await Sandbox.list({ client: c })) {
        if (s.name.startsWith(`${PREFIX}-`)) {
          await Sandbox.remove(s.name, { force: true, client: c }).catch((e) =>
            console.error(`cleanup ${s.name}: ${e}`),
          );
        }
      }
      for (const v of await volumes.list({ client: c })) {
        if (v.name.startsWith(`${PREFIX}-`)) {
          await volumes
            .remove(v.name, { pool: v.pool, client: c })
            .catch((e) => console.error(`cleanup volume ${v.name}: ${e}`));
        }
      }
    } finally {
      await c.close();
      await client?.close();
      rmSync(tmp, { recursive: true, force: true });
    }
  }, T);

  let sb: Sandbox;

  test(
    "create, then create again is AlreadyExistsError",
    async () => {
      const lines: string[] = [];
      sb = await Sandbox.create(spec, { client, onProgress: (l) => lines.push(l) });
      expect(sb.name).toBe(spec.name);
      expect(lines.length).toBeGreaterThan(0);
      const info = await sb.info();
      expect(info.status).toBe("Running");
      expect(info.type).toBe("container");
      const e = await Sandbox.create(spec, { client }).catch((e) => e);
      expect(e).toBeInstanceOf(AlreadyExistsError);
    },
    T,
  );

  test(
    "connectOrCreate on a correct sandbox changes nothing",
    async () => {
      const again = await Sandbox.connectOrCreate(spec, { client });
      const r = again.lastReport;
      expect(r?.created).toBe(false);
      expect(r?.applied.filter((a) => a.action !== "note")).toEqual([]);
      const plan = await Sandbox.plan(spec, { client });
      expect(plan.status).toBe("Running");
      expect(planHasChanges(plan)).toBe(false);
    },
    T,
  );

  test("get, listWith labels, labels()", async () => {
    const got = await Sandbox.get(spec.name, { client });
    expect((await got.labels())["isb-test"]).toBe("ts");
    const byMap = await Sandbox.listWith(
      { labels: { "isb-test-run": `${process.pid}`, "isb-test": null } },
      { client },
    );
    expect(byMap.map((i) => i.name)).toEqual([spec.name]);
    const byList = await Sandbox.listWith({ labels: [`isb-test-run=${process.pid}`] }, { client });
    expect(byList.map((i) => i.name)).toEqual([spec.name]);
    const none = await Sandbox.listWith({ labels: { "isb-test-run": "no-such-run" } }, { client });
    expect(none).toEqual([]);
  });

  test("exec: argv is preserved, exit code and streams split", async () => {
    const out = await sb.exec("printf", ["[%s]", "a b", "$HOME", ""]);
    expect(out.success).toBe(true);
    expect(out.stdoutText).toBe("[a b][$HOME][]");
    const r = await sb.exec(["sh", "-c", "echo out; echo err >&2; exit 3"]);
    expect(r.exitCode).toBe(3);
    expect(r.success).toBe(false);
    expect(r.stdoutText).toBe("out\n");
    expect(r.stderrText).toBe("err\n");
    const env = await sb.exec(["sh", "-c", "echo $FROM_SPEC"]);
    expect(env.stdoutText).toBe("yes\n");
  });

  test("exec as dev with cwd and env", async () => {
    const r = await sb.exec(["sh", "-c", 'id -un; pwd; echo "$FOO"; echo "$HOME"'], {
      user: "dev",
      cwd: "/tmp",
      env: { FOO: "bar baz" },
    });
    expect(r.stdoutText).toBe("dev\n/tmp\nbar baz\n/home/dev\n");
    const uid = await sb.exec("id", ["-u"], { user: 1000 });
    expect(uid.stdoutText.trim()).toBe("1000");
  });

  test("exec with stdin", async () => {
    const r = await sb.exec("cat", { stdin: "hello\nworld" });
    expect(r.stdoutText).toBe("hello\nworld");
    const bytes = new Uint8Array([0, 1, 2, 255]);
    const b = await sb.exec("cat", { stdin: bytes });
    expect(b.stdout).toEqual(bytes);
  });

  test("execStream: the first chunk arrives before the process exits", async () => {
    const p = await sb.execStream(["sh", "-c", "echo first; sleep 2; echo second >&2"]);
    const events: ExecEvent[] = [];
    let doneAtFirst: boolean | undefined;
    for await (const ev of p) {
      if (doneAtFirst === undefined) doneAtFirst = p.done;
      events.push(ev);
    }
    expect(doneAtFirst).toBe(false);
    expect(await p.wait()).toBe(0);
    expect(events.map((e) => [e.kind, text(e.data)])).toEqual([
      ["stdout", "first\n"],
      ["stderr", "second\n"],
    ]);
  });

  test("execStream: piped stdin, write and closeStdin", async () => {
    const p = await sb.execStream("cat", { stdin: "piped" });
    await p.write("abc");
    await p.write(new Uint8Array([10]));
    await p.closeStdin();
    const out = await p.output();
    expect(out.exitCode).toBe(0);
    expect(out.stdoutText).toBe("abc\n");
  });

  test("execStream: signal 15 to a trapping shell gives its exit code", async () => {
    const p = await sb.execStream([
      "sh",
      "-c",
      'trap "exit 42" TERM; echo ready; while :; do sleep 0.1; done',
    ]);
    for await (const ev of p) {
      if (text(ev.data).includes("ready")) break;
    }
    await p.signal(15);
    expect(await p.wait()).toBe(42);
  });

  test("exec timeout is IsbTimeoutError", async () => {
    const t0 = Date.now();
    const e = await sb.exec("sleep", ["30"], { timeout: 1 }).catch((e) => e);
    expect(e).toBeInstanceOf(IsbTimeoutError);
    expect(e.code).toBe("exec_timeout");
    expect(Date.now() - t0).toBeLessThan(15_000);
    const s = await sb.execStream("sleep", ["30"], { timeout: "1s" });
    const e2 = await s.wait().catch((e) => e);
    expect(e2).toBeInstanceOf(IsbTimeoutError);
  });

  test("tty exec shows /dev/pts", async () => {
    const r = await sb.exec("tty", { tty: true });
    expect(r.exitCode).toBe(0);
    expect(r.stdoutText).toContain("/dev/pts/");
    const p = await sb.execStream(["sh", "-c", "sleep 0.5; stty size"], {
      tty: true,
      width: 80,
      height: 24,
    });
    await p.resize(100, 30);
    const out = await p.output();
    expect(out.stdoutText.trim()).toBe("30 100");
  });

  test("addPort with search steps past a taken port; removeDevice", async () => {
    const server: Server = createServer();
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const taken = (server.address() as { port: number }).port;
    try {
      const listen = await sb.addPort(
        PortBinding.host(`tcp:127.0.0.1:${taken}`, "tcp:127.0.0.1:80", {
          name: "web",
          search: 20,
        }),
      );
      expect(listen).toMatch(/^tcp:127\.0\.0\.1:\d+$/);
      const port = Number(listen.split(":")[2]);
      expect(port).toBeGreaterThan(taken);
      expect(port).toBeLessThanOrEqual(taken + 20);
      expect((await sb.info()).devices.web?.listen).toBe(listen);
      expect(await sb.removeDevice("web")).toBe(true);
      expect(await sb.removeDevice("web")).toBe(false);
    } finally {
      server.close();
    }
  });

  test(
    "named volume with owner via Volume.named (reconciled onto the sandbox)",
    async () => {
      const vol = `${PREFIX}-cache`;
      const withVol: SandboxSpec = {
        ...spec,
        volumes: { "/home/dev/.cache/sdk": Volume.named(vol, { owner: "dev" }) },
      };
      const plan = await Sandbox.plan(withVol, { client });
      expect(plan.actions.map((a) => a.action)).toContain("create_volume");
      const again = await Sandbox.connectOrCreate(withVol, { client });
      const kinds = again.lastReport?.applied.map((a) => a.action) ?? [];
      expect(kinds).toContain("add_device");
      expect(kinds).toContain("fix_owner");
      const r = await sb.exec("stat", ["-c", "%U", "/home/dev/.cache/sdk"]);
      expect(r.stdoutText.trim()).toBe("dev");
      const info = await volumes.get(vol, { client });
      expect(info.name).toBe(vol);
      expect(info.used_by.join(" ")).toContain(spec.name);
      // In use: refused.
      expect(volumes.remove(vol, { client })).rejects.toBeDefined();
      // Detach it again (prune the device), then remove the volume.
      const pruned = await Sandbox.connectOrCreate(spec, { client, pruneDevices: true });
      expect(pruned.lastReport?.applied.map((a) => a.action)).toContain("remove_device");
      await volumes.remove(vol, { pool: info.pool, client });
      expect(volumes.get(vol, { client })).rejects.toBeInstanceOf(NotFoundError);
    },
    T,
  );

  test(
    "waitReady, stop, start, restart",
    async () => {
      await sb.waitReady();
      await sb.waitReady({ ready: ["running", { path_writable: "/tmp" }], readyTimeout: 30 });
      await sb.stop({ force: true });
      expect((await sb.info()).status).toBe("Stopped");
      await sb.start();
      await sb.waitReady();
      await sb.restart();
      expect((await sb.info()).status).toBe("Running");
    },
    T,
  );

  test(
    "Project: load, plan, up, exec with service defaults, down",
    async () => {
      const dir = join(tmp, "proj");
      mkdirSync(join(dir, "src"), { recursive: true });
      chmodSync(dir, 0o755);
      chmodSync(join(dir, "src"), 0o755);
      writeFileSync(join(dir, "src", "marker"), "from-host\n");
      const file = join(dir, "isb.yaml");
      writeFileSync(
        file,
        [
          "sandboxes:",
          "  web:",
          `    image: ${IMAGE}`,
          "    labels: {isb-test: ts}",
          '    env: {GREETING: "${GREETING}"}',
          "    volumes:",
          "      /home/dev/src: {bind: ./src, device: src, readonly: true}",
          "    ready: [running, {user_exists: dev}]",
          "    exec: {user: dev, cwd: /home/dev/src}",
          "",
        ].join("\n"),
      );
      const proj = await Project.load({
        files: [file],
        projectName: `${PREFIX}-proj`,
        vars: { GREETING: "hi there" },
        client,
      });
      expect(proj.services).toEqual(["web"]);
      expect(proj.spec("web").name).toBe(`${PREFIX}-proj-web`);

      const plans = await proj.plan();
      expect(plans).toHaveLength(1);
      expect(plans[0]?.status).toBeNull();
      expect(plans[0]?.actions.map((a) => a.action)).toContain("create_instance");

      const lines: string[] = [];
      const up = await proj.up({ onProgress: (l) => lines.push(l) });
      expect(up.map((u) => u.service)).toEqual(["web"]);
      expect(up[0]?.report.created).toBe(true);
      expect(lines.length).toBeGreaterThan(0);

      const web = proj.sandbox("web");
      const r = await web.exec(["sh", "-c", 'id -un; pwd; cat marker; echo "$GREETING"']);
      expect(r.stdoutText).toBe("dev\n/home/dev/src\nfrom-host\nhi there\n");
      // Per-call options override the service defaults.
      const root = await web.exec("id", ["-un"], { user: "root" });
      expect(root.stdoutText).toBe("root\n");

      const replan = await proj.plan();
      expect(planHasChanges(replan[0] as (typeof replan)[number])).toBe(false);
      const again = await proj.up({ services: ["web"] });
      expect(again[0]?.report.created).toBe(false);

      await proj.down();
      expect(Sandbox.get(web.name, { client })).rejects.toBeInstanceOf(NotFoundError);
    },
    T,
  );

  test("NotFoundError for a missing sandbox", async () => {
    const e = await Sandbox.get(`${PREFIX}-missing`, { client }).catch((e) => e);
    expect(e).toBeInstanceOf(NotFoundError);
    expect(e.message).toContain(`${PREFIX}-missing`);
    const x = new Sandbox(`${PREFIX}-missing`, client);
    expect(x.exec("true")).rejects.toBeInstanceOf(NotFoundError);
  });

  test(
    "remove",
    async () => {
      const e = await sb.remove().catch((e) => e);
      expect(e).toBeDefined(); // running: needs force
      await sb.remove({ force: true });
      expect(Sandbox.get(sb.name, { client })).rejects.toBeInstanceOf(NotFoundError);
    },
    T,
  );
});
