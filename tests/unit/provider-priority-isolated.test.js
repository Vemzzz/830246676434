import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

// DATA_DIR must be set before src/lib/db is imported, otherwise the adapter opens
// the developer's live ~/.9router database — the defect upstream's 57c04f00
// removed the old file for (it had seeded ~787 rows into the live DB). This file
// keeps the same regression coverage on a throwaway directory.
const dataDir = mkdtempSync(join(tmpdir(), "vr-priority-"));
process.env.DATA_DIR = dataDir;

let createProviderConnection;
let getProviderConnections;
let deleteProviderConnection;
let updateProviderConnection;
let resetDb;

beforeAll(async () => {
  const db = await import("../../src/lib/db/index.js");
  ({ createProviderConnection, getProviderConnections, deleteProviderConnection, updateProviderConnection } = db);
  resetDb = db.resetDbForTests;
});

afterAll(async () => {
  try {
    if (typeof resetDb === "function") await resetDb();
  } catch {
    // best effort — the directory is removed either way
  }
  rmSync(dataDir, { recursive: true, force: true });
});

const seed = async (provider, n, priority) => {
  for (let i = 0; i < n; i++) {
    await createProviderConnection({
      provider,
      authType: "apikey",
      name: `seed-${i}`,
      apiKey: `k${i}`,
      ...(priority === undefined ? {} : { priority: 1 }),
    });
  }
};

describe("provider insert priority (#4311 regression, isolated DATA_DIR)", () => {
  it("assigns sequential priorities when the caller omits priority", async () => {
    const P = `iso-seq-${Date.now()}`;
    await seed(P, 3);
    const list = await getProviderConnections({ provider: P });
    expect(list.map((c) => c.priority)).toEqual([1, 2, 3]);
  });

  it("reindexes when the caller sends the dashboard's explicit priority 1", async () => {
    const P = `iso-prio1-${Date.now()}`;
    await seed(P, 3, 1);
    const list = await getProviderConnections({ provider: P });
    expect(list.map((c) => c.priority)).toEqual([1, 2, 3]);
    expect(new Set(list.map((c) => c.name)).size).toBe(3);
  });

  it("renumbers after a delete so gaps do not accumulate", async () => {
    const P = `iso-del-${Date.now()}`;
    await seed(P, 4);
    const before = await getProviderConnections({ provider: P });
    await deleteProviderConnection(before[0].id);
    const after = await getProviderConnections({ provider: P });
    expect(after.map((c) => c.priority)).toEqual([1, 2, 3]);
  });

  it("renumbers on an explicit priority update", async () => {
    const P = `iso-upd-${Date.now()}`;
    await seed(P, 4);
    const list = await getProviderConnections({ provider: P });
    await updateProviderConnection(list[3].id, { priority: 1 });
    const after = await getProviderConnections({ provider: P });
    expect(after.map((c) => c.priority)).toEqual([1, 2, 3, 4]);
  });
});
