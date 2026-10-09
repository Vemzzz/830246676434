#!/usr/bin/env node
// Shrink already-stored requestDetails rows, then reclaim the SQLite file.
//
// Rows written before the maxJsonSize clamp was fixed can hold whole request /
// response bodies (avg ~1 MB/row). This script re-applies the same per-field
// truncation the write path now uses, UPDATEs those rows in place (no row is
// deleted), and VACUUMs to return the freed pages to the filesystem.
//
// Usage:
//   node scripts/compact-request-details.cjs           # dry run, prints plan
//   node scripts/compact-request-details.cjs --yes     # take a backup, then compact
//
// The DB path comes from src/lib/dataDir.js (DATA_DIR env, else ~/.9router/db),
// the same rule the app uses.
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");

const BATCH_SIZE = 25;

const srcUrl = (rel) => pathToFileURL(path.join(__dirname, "..", "src", "lib", rel)).href;

async function openAdapter(file) {
  try {
    const { createBetterSqliteAdapter } = await import(srcUrl("db/adapters/betterSqliteAdapter.js"));
    return createBetterSqliteAdapter(file);
  } catch (e) {
    console.warn(`[compact] better-sqlite3 unavailable (${e.message}); falling back to node:sqlite`);
  }
  const { createNodeSqliteAdapter } = await import(srcUrl("db/adapters/nodeSqliteAdapter.js"));
  return await createNodeSqliteAdapter(file);
}

function fmtBytes(n) {
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

async function main() {
  const apply = process.argv.includes("--yes");

  const { DATA_DIR } = await import(srcUrl("dataDir.js"));
  const dbPath = path.join(DATA_DIR, "db", "data.sqlite");

  if (!fs.existsSync(dbPath)) {
    console.error(`[compact] database not found: ${dbPath}`);
    return 2;
  }

  const { truncateField, resolveMaxJsonSize } = await import(srcUrl("db/repos/requestDetailsRepo.js"));

  const fileSizeBefore = fs.statSync(dbPath).size;

  let db;
  try {
    db = await openAdapter(dbPath);
  } catch (e) {
    if (/locked|busy/i.test(e.message)) {
      console.error(
        "[compact] database is locked — another process (the app) holds it. " +
        "Stop the app (pm2 stop) and re-run this script."
      );
      return 3;
    }
    throw e;
  }

  let exitCode = 0;
  try {
    const settingsRow = db.get(`SELECT data FROM settings WHERE id = 1`);
    let settings = {};
    try { settings = JSON.parse(settingsRow?.data || "{}"); } catch {}
    const cap = resolveMaxJsonSize(settings);

    const before = db.get(`SELECT COUNT(*) AS rows, COALESCE(SUM(LENGTH(data)), 0) AS bytes FROM requestDetails`);
    const oversized = db.get(
      `SELECT COUNT(*) AS rows, COALESCE(SUM(LENGTH(data)), 0) AS bytes FROM requestDetails WHERE LENGTH(data) > ?`,
      [cap]
    );

    console.log(`[compact] db            : ${dbPath}`);
    console.log(`[compact] cap per field : ${cap} bytes (${cap / 1024} KB)`);
    console.log(`[compact] rows          : ${before.rows} (${fmtBytes(before.bytes)} of JSON)`);
    console.log(`[compact] oversized rows: ${oversized.rows} (${fmtBytes(oversized.bytes)})`);
    console.log(`[compact] file size     : ${fmtBytes(fileSizeBefore)}`);

    if (oversized.rows === 0) {
      console.log("[compact] nothing to compact.");
      return 0;
    }

    if (!apply) {
      console.log("[compact] dry run — re-run with --yes to back up and compact.");
      return 0;
    }

    // Full-file copy first; abort without writing if it fails.
    const backupPath = `${dbPath}.bak-${new Date().toISOString().replace(/[:.]/g, "-")}`;
    try {
      fs.copyFileSync(dbPath, backupPath);
      for (const suffix of ["-wal", "-shm"]) {
        if (fs.existsSync(dbPath + suffix)) fs.copyFileSync(dbPath + suffix, backupPath + suffix);
      }
      console.log(`[compact] backup        : ${backupPath}`);
    } catch (e) {
      console.error(`[compact] backup FAILED, aborting: ${e.message}`);
      return 1;
    }

    let cursor = "";
    let updated = 0;
    for (;;) {
      const batch = db.all(
        `SELECT id, data FROM requestDetails WHERE LENGTH(data) > ? AND id > ? ORDER BY id ASC LIMIT ?`,
        [cap, cursor, BATCH_SIZE]
      );
      if (batch.length === 0) break;
      cursor = batch[batch.length - 1].id;

      db.transaction(() => {
        for (const row of batch) {
          let record;
          try { record = JSON.parse(row.data); } catch { continue; }
          if (!record || typeof record !== "object") continue;
          for (const field of ["request", "providerRequest", "providerResponse", "response"]) {
            if (record[field] == null) continue;
            record[field] = truncateField(record[field], cap);
          }
          db.run(`UPDATE requestDetails SET data = ? WHERE id = ?`, [JSON.stringify(record), row.id]);
          updated++;
        }
      });
    }

    const after = db.get(`SELECT COUNT(*) AS rows, COALESCE(SUM(LENGTH(data)), 0) AS bytes FROM requestDetails`);
    console.log(`[compact] updated rows  : ${updated}`);
    console.log(`[compact] rows          : ${after.rows} (${fmtBytes(after.bytes)} of JSON)`);
    console.log(`[compact] file size     : ${fmtBytes(fs.statSync(dbPath).size)} (pre-VACUUM)`);

    try {
      db.exec("VACUUM");
      console.log(`[compact] file size     : ${fmtBytes(fs.statSync(dbPath).size)} (post-VACUUM)`);
    } catch (e) {
      if (/locked|busy/i.test(e.message)) {
        console.error(
          "[compact] VACUUM failed: database is locked. Rows were truncated, but the file was not " +
          "reclaimed. Stop the app (pm2 stop) and re-run this script to VACUUM."
        );
      } else {
        console.error(`[compact] VACUUM failed: ${e.message}`);
      }
      exitCode = 3;
    }
  } finally {
    try { db.close(); } catch {}
  }
  return exitCode;
}

main()
  .then((code) => { process.exitCode = code; })
  .catch((e) => { console.error(`[compact] fatal: ${e.stack || e.message}`); process.exitCode = 1; });
