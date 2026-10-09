const apiKeyLimitsMigration = {
  version: 9,
  name: "add-api-key-limits",
  up(db) {
    const columns = new Set(db.all("PRAGMA table_info(apiKeys)").map((row) => row.name));
    for (const [name, type] of [
      ["maxInputTokens", "INTEGER"],
      ["maxOutputTokens", "INTEGER"],
      ["requestsPerMinute", "INTEGER"],
      ["ipWhitelist", "TEXT"],
    ]) {
      if (!columns.has(name)) db.exec(`ALTER TABLE apiKeys ADD COLUMN ${name} ${type}`);
    }
    db.exec(`CREATE TABLE IF NOT EXISTS apiKeyRateLimits (
      apiKeyId TEXT PRIMARY KEY,
      windowStart INTEGER NOT NULL,
      requestCount INTEGER NOT NULL
    )`);
  },
};

export default apiKeyLimitsMigration;
