// requestDetails payload sizing — regression guard for the observabilityMaxJsonSize
// unit bug: the setting is KB. A garbage stored value (1024, i.e. 1 MB) is out of the
// accepted 1–64 KB band and must fall back to the 5 KB default, not clamp to the
// 64 KB ceiling — a ceiling-sized cap still let big bodies through and made the list
// endpoint read hundreds of MB from SQLite.
import { describe, it, expect } from "vitest";
import { truncateField, resolveMaxJsonSize } from "@/lib/db/repos/requestDetailsRepo.js";

const KB = 1024;
const big = (mb) => ({ blob: "x".repeat(mb * 1024 * 1024) });

describe("resolveMaxJsonSize — KB setting, out-of-range falls back to the 5 KB default", () => {
  it("interprets the setting as KB", () => {
    expect(resolveMaxJsonSize({ observabilityMaxJsonSize: 5 }, {})).toBe(5 * KB);
  });

  it("honours a sane stored value (32 KB) so a user can raise it deliberately", () => {
    expect(resolveMaxJsonSize({ observabilityMaxJsonSize: 32 }, {})).toBe(32 * KB);
  });

  it("falls back to the 5 KB default for a stored 1024 (out of range), not the 64 KB ceiling", () => {
    expect(resolveMaxJsonSize({ observabilityMaxJsonSize: 1024 }, {})).toBe(5 * KB);
  });

  it("falls back to the 5 KB default for values below the 1 KB floor", () => {
    expect(resolveMaxJsonSize({ observabilityMaxJsonSize: 0 }, {})).toBe(5 * KB);
  });

  it("falls back to the 5 KB default for values above the 64 KB ceiling", () => {
    expect(resolveMaxJsonSize({ observabilityMaxJsonSize: 65 }, {})).toBe(5 * KB);
  });

  it("falls back to the env value when the setting is absent", () => {
    expect(resolveMaxJsonSize({}, { OBSERVABILITY_MAX_JSON_SIZE: "8" })).toBe(8 * KB);
  });

  it("falls back to the 5 KB default when the env value is also out of range", () => {
    expect(resolveMaxJsonSize({}, { OBSERVABILITY_MAX_JSON_SIZE: "1024" })).toBe(5 * KB);
  });
});

describe("truncateField — bounded stored payload", () => {
  it("passes a small payload through untruncated", () => {
    const small = { messages: [{ role: "user", content: "hi" }] };
    expect(truncateField(small, 5 * KB)).toEqual(small);
  });

  it("truncates a payload over the configured cap and keeps the record small", () => {
    const truncated = truncateField(big(1), 5 * KB);
    expect(truncated._truncated).toBe(true);
    const serialized = JSON.stringify({ request: truncated, response: {} });
    expect(Buffer.byteLength(serialized)).toBeLessThan(200 * KB);
  });

  it("regression: cap derived from a stored 1024 is the 5 KB default and still truncates a 2 MB payload", () => {
    const cap = resolveMaxJsonSize({ observabilityMaxJsonSize: 1024 }, {});
    expect(cap).toBe(5 * KB);

    const record = {
      id: "regression-1",
      request: truncateField(big(2), cap),
      providerRequest: truncateField(big(2), cap),
      providerResponse: truncateField(big(2), cap),
      response: truncateField(big(2), cap),
    };
    for (const field of ["request", "providerRequest", "providerResponse", "response"]) {
      expect(record[field]._truncated).toBe(true);
      expect(record[field]._originalSize).toBeGreaterThan(2 * 1024 * 1024);
    }
    const serialized = JSON.stringify(record);
    for (const field of ["request", "providerRequest", "providerResponse", "response"]) {
      expect(record[field]._preview.length).toBeLessThanOrEqual(200);
    }
    expect(Buffer.byteLength(serialized)).toBeLessThan(200 * KB);
  });

  it("hard-clamps even when a caller passes an oversized maxSize", () => {
    const truncated = truncateField(big(1), 100 * 1024 * 1024);
    expect(truncated._truncated).toBe(true);
  });

  it("measures the cap in UTF-8 bytes, not UTF-16 code units", () => {
    const cap = 5 * KB;
    // CJK chars are 1 UTF-16 code unit but 3 UTF-8 bytes: 2048 chars = 2048
    // code units (< cap) but 6144 bytes (> cap).
    const payload = { text: "\u4e2d".repeat(2048) };
    const str = JSON.stringify(payload);
    expect(str.length).toBeLessThan(cap);
    expect(Buffer.byteLength(str, "utf8")).toBeGreaterThan(cap);

    const truncated = truncateField(payload, cap);
    expect(truncated._truncated).toBe(true);
    expect(truncated._originalSize).toBe(Buffer.byteLength(str, "utf8"));
  });
});
