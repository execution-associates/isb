import { describe, expect, it } from "vitest";
import { defaultBackupName } from "@/data/backup-dialogs";
import { hookTimeoutProblem, originLabel, snapshotNameProblem, stampDate } from "./api";

describe("volume helpers", () => {
  it("reads restore stamps", () => {
    expect(stampDate("20261003T090912Z")?.toISOString()).toBe("2026-10-03T09:09:12.000Z");
    expect(stampDate("2026-10-03")).toBeNull();
  });

  it("names where a restore came from", () => {
    expect(originLabel("snapshot:auto-20261003T090000Z")).toBe("snapshot auto-20261003T090000Z");
    expect(originLabel("backup:isb/acme/home/ws_home-20261003T090912Z.volume.tar.gz")).toBe("backup ws_home-20261003T090912Z.volume.tar.gz");
  });

  it("checks snapshot names like the daemon", () => {
    expect(snapshotNameProblem("")).toBeNull();
    expect(snapshotNameProblem("before-upgrade")).toBeNull();
    expect(snapshotNameProblem("auto-1")).toMatch(/isb's own/);
    expect(snapshotNameProblem("isb-backup-x")).toMatch(/isb's own/);
    expect(snapshotNameProblem("-x")).not.toBeNull();
    expect(snapshotNameProblem("a/b")).not.toBeNull();
  });

  it("bounds the hook timeout", () => {
    expect(hookTimeoutProblem("")).toBeNull();
    expect(hookTimeoutProblem("30s")).toBeNull();
    expect(hookTimeoutProblem("1h")).toBeNull();
    expect(hookTimeoutProblem("2h")).not.toBeNull();
    expect(hookTimeoutProblem("0s")).not.toBeNull();
    expect(hookTimeoutProblem("soon")).not.toBeNull();
  });

  it("names a volume's backup from the volume", () => {
    expect(defaultBackupName("pg")).toBe("pg-daily");
    expect(defaultBackupName("acme_workspace_home")).toBe("acme-workspace-home-daily");
    expect(defaultBackupName("Shop-Production_PG_Data_with_a_long_tail")).toBe("shop-production-pg-dat-daily");
  });
});
