import Database from "better-sqlite3";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../src/lib/db/driver.js", () => ({ getAdapter: vi.fn() }));

import { getAdapter } from "../../src/lib/db/driver.js";
import { consumeApiKeyRequest } from "../../src/lib/db/repos/apiKeysRepo.js";

let db;

beforeEach(() => {
  db = new Database(":memory:");
  db.exec(`CREATE TABLE apiKeyRateLimits (
    apiKeyId TEXT PRIMARY KEY,
    windowStart INTEGER NOT NULL,
    requestCount INTEGER NOT NULL
  )`);
  getAdapter.mockResolvedValue({
    run: (sql, params) => db.prepare(sql).run(...params),
  });
});

afterEach(() => {
  db.close();
  vi.clearAllMocks();
});

describe("API key RPM persistence", () => {
  it("allows up to the configured limit per fixed minute and resets on the next minute", async () => {
    expect(await consumeApiKeyRequest("key-1", 2, 10_000)).toEqual({ allowed: true, retryAfterSeconds: 0 });
    expect(await consumeApiKeyRequest("key-1", 2, 20_000)).toEqual({ allowed: true, retryAfterSeconds: 0 });
    expect(await consumeApiKeyRequest("key-1", 2, 30_000)).toEqual({ allowed: false, retryAfterSeconds: 30 });
    expect(await consumeApiKeyRequest("key-1", 2, 60_000)).toEqual({ allowed: true, retryAfterSeconds: 0 });
  });

  it("tracks limits independently for each API key", async () => {
    await consumeApiKeyRequest("key-1", 1, 10_000);
    expect(await consumeApiKeyRequest("key-1", 1, 20_000)).toMatchObject({ allowed: false });
    expect(await consumeApiKeyRequest("key-2", 1, 20_000)).toMatchObject({ allowed: true });
  });
});
