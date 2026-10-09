import { randomUUID } from "node:crypto";
import { getAdapter } from "../driver.js";

// Parse a JSON TEXT column with null=all / []=none semantics.
// DB NULL → null (all allowed). DB "[]" → [] (none). DB "[x]" → [x].
function parsePermList(raw) {
  if (raw === null || raw === undefined) return null;
  try { return JSON.parse(raw); } catch { return null; }
}

// Serialize back: null → null (DB NULL), [] → "[]", [x] → "[x]"
function serializePermList(val) {
  if (val === null || val === undefined) return null;
  return JSON.stringify(Array.isArray(val) ? val : []);
}

function rowToKey(row) {
  if (!row) return null;
  return {
    id: row.id,
    key: row.key,
    name: row.name,
    machineId: row.machineId,
    isActive: row.isActive === 1 || row.isActive === true,
    createdAt: row.createdAt,
    allowedProviders: parsePermList(row.allowedProviders),
    allowedCombos: parsePermList(row.allowedCombos),
    allowedKinds: parsePermList(row.allowedKinds),
    maxInputTokens: row.maxInputTokens ?? null,
    maxOutputTokens: row.maxOutputTokens ?? null,
    requestsPerMinute: row.requestsPerMinute ?? null,
    ipWhitelist: parsePermList(row.ipWhitelist),
  };
}

export async function getApiKeys() {
  const db = await getAdapter();
  const rows = db.all(`SELECT * FROM apiKeys ORDER BY createdAt ASC`);
  return rows.map(rowToKey);
}

export async function getApiKeyById(id) {
  const db = await getAdapter();
  const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
  return rowToKey(row);
}

export async function createApiKey(name, machineId, limits = {}) {
  if (!machineId) throw new Error("machineId is required");
  const [db, { generateApiKeyWithMachine }] = await Promise.all([
    getAdapter(),
    import("@/shared/utils/apiKey"),
  ]);
  const result = generateApiKeyWithMachine(machineId);
  const apiKey = {
    id: randomUUID(),
    name,
    key: result.key,
    machineId,
    isActive: true,
    createdAt: new Date().toISOString(),
    allowedProviders: null,
    allowedCombos: null,
    allowedKinds: null,
    ...limits,
  };
  db.run(
    `INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt, allowedProviders, allowedCombos, allowedKinds, maxInputTokens, maxOutputTokens, requestsPerMinute, ipWhitelist) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    [
      apiKey.id, apiKey.key, apiKey.name, apiKey.machineId, 1, apiKey.createdAt,
      null, null, null, apiKey.maxInputTokens, apiKey.maxOutputTokens,
      apiKey.requestsPerMinute, serializePermList(apiKey.ipWhitelist),
    ]
  );
  return apiKey;
}

export async function updateApiKey(id, data) {
  const db = await getAdapter();
  let result = null;
  db.transaction(() => {
    const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
    if (!row) return;
    const current = rowToKey(row);
    // Merge: only override fields explicitly present in data
    const merged = { ...current };
    if (data.isActive !== undefined) merged.isActive = data.isActive;
    if (data.name !== undefined) merged.name = data.name;
    if ("allowedProviders" in data) merged.allowedProviders = data.allowedProviders;
    if ("allowedCombos" in data) merged.allowedCombos = data.allowedCombos;
    if ("allowedKinds" in data) merged.allowedKinds = data.allowedKinds;
    if ("maxInputTokens" in data) merged.maxInputTokens = data.maxInputTokens;
    if ("maxOutputTokens" in data) merged.maxOutputTokens = data.maxOutputTokens;
    if ("requestsPerMinute" in data) merged.requestsPerMinute = data.requestsPerMinute;
    if ("ipWhitelist" in data) merged.ipWhitelist = data.ipWhitelist;
    db.run(
      `UPDATE apiKeys SET key = ?, name = ?, machineId = ?, isActive = ?, allowedProviders = ?, allowedCombos = ?, allowedKinds = ?, maxInputTokens = ?, maxOutputTokens = ?, requestsPerMinute = ?, ipWhitelist = ? WHERE id = ?`,
      [
        merged.key,
        merged.name,
        merged.machineId,
        merged.isActive ? 1 : 0,
        serializePermList(merged.allowedProviders),
        serializePermList(merged.allowedCombos),
        serializePermList(merged.allowedKinds),
        merged.maxInputTokens,
        merged.maxOutputTokens,
        merged.requestsPerMinute,
        serializePermList(merged.ipWhitelist),
        id,
      ]
    );
    result = merged;
  });
  return result;
}

export async function deleteApiKey(id) {
  const db = await getAdapter();
  const res = db.transaction(() => {
    db.run(`DELETE FROM apiKeyRateLimits WHERE apiKeyId = ?`, [id]);
    return db.run(`DELETE FROM apiKeys WHERE id = ?`, [id]);
  });
  return (res?.changes ?? 0) > 0;
}

export async function consumeApiKeyRequest(id, limit, now = Date.now()) {
  const db = await getAdapter();
  const windowStart = Math.floor(now / 60000) * 60000;
  const result = db.run(
    `INSERT INTO apiKeyRateLimits(apiKeyId, windowStart, requestCount) VALUES(?, ?, 1)
     ON CONFLICT(apiKeyId) DO UPDATE SET
       requestCount = CASE
         WHEN apiKeyRateLimits.windowStart = excluded.windowStart THEN apiKeyRateLimits.requestCount + 1
         ELSE 1
       END,
       windowStart = excluded.windowStart
     WHERE apiKeyRateLimits.windowStart != excluded.windowStart
       OR apiKeyRateLimits.requestCount < ?`,
    [id, windowStart, limit]
  );
  return {
    allowed: (result?.changes ?? 0) > 0,
    retryAfterSeconds: (result?.changes ?? 0) > 0
      ? 0
      : Math.max(1, Math.ceil((windowStart + 60000 - now) / 1000)),
  };
}

export async function validateApiKey(key) {
  const db = await getAdapter();
  const row = db.get(`SELECT * FROM apiKeys WHERE key = ?`, [key]);
  if (!row || row.isActive !== 1 && row.isActive !== true) return null;
  return rowToKey(row);
}
