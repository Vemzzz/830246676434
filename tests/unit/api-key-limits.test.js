import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("../../src/lib/localDb.js", () => ({ consumeApiKeyRequest: vi.fn() }));
vi.mock("../../src/lib/auth/loginLimiter.js", () => ({ getClientIp: vi.fn() }));

import {
  estimateInputTokens,
  normalizeApiKeyLimits,
  normalizeIpAddress,
} from "../../src/lib/apiKeyLimitUtils.js";
import {
  applyApiKeyOutputLimit,
  enforceApiKeyInputLimit,
  enforceApiKeyRequest,
} from "../../src/sse/services/apiKeyLimits.js";
import { consumeApiKeyRequest } from "../../src/lib/localDb.js";
import { getClientIp } from "../../src/lib/auth/loginLimiter.js";

beforeEach(() => {
  vi.clearAllMocks();
  getClientIp.mockReturnValue("192.0.2.4");
  consumeApiKeyRequest.mockResolvedValue({ allowed: true, retryAfterSeconds: 0 });
});

describe("API key limit configuration", () => {
  it("normalizes optional positive limits and a list of literal IP addresses", () => {
    expect(normalizeApiKeyLimits({
      maxInputTokens: "4000",
      maxOutputTokens: "",
      requestsPerMinute: 60,
      ipWhitelist: "192.0.2.4\n2001:db8::1, 192.0.2.4",
    })).toEqual({
      value: {
        maxInputTokens: 4000,
        maxOutputTokens: null,
        requestsPerMinute: 60,
        ipWhitelist: ["192.0.2.4", "2001:db8::1"],
      },
    });
  });

  it("rejects non-positive limits and invalid IP addresses", () => {
    expect(normalizeApiKeyLimits({ requestsPerMinute: 0 }).error).toMatch(/positive whole number/);
    expect(normalizeApiKeyLimits({ ipWhitelist: "example.com" }).error).toMatch(/Invalid IP address/);
  });

  it("normalizes IPv4-mapped IPv6 addresses", () => {
    expect(normalizeIpAddress("::ffff:192.0.2.4")).toBe("192.0.2.4");
  });
});

describe("API key request limits", () => {
  it("uses an approximate JSON-byte input estimate and rejects requests above the cap", () => {
    const body = { messages: [{ role: "user", content: "12345678" }] };
    expect(estimateInputTokens(body)).toBe(Math.ceil(new TextEncoder().encode(JSON.stringify(body)).byteLength / 4));
    expect(enforceApiKeyInputLimit({ maxInputTokens: 1 }, body).status).toBe(400);
    expect(enforceApiKeyInputLimit({ maxInputTokens: 1000 }, body)).toBeNull();
  });

  it("applies an output cap without increasing a lower requested cap", () => {
    const chatBody = { max_tokens: 800 };
    applyApiKeyOutputLimit(new Request("https://example.test/v1/chat/completions"), chatBody, { maxOutputTokens: 200 });
    expect(chatBody.max_tokens).toBe(200);

    const responsesBody = { input: "hello", max_output_tokens: 100 };
    applyApiKeyOutputLimit(new Request("https://example.test/v1/responses"), responsesBody, { maxOutputTokens: 200 });
    expect(responsesBody.max_output_tokens).toBe(100);
  });

  it("uses the configured output cap when the request has no output token field", () => {
    const body = { messages: [{ role: "user", content: "hello" }] };
    applyApiKeyOutputLimit(new Request("https://example.test/v1/chat/completions"), body, { maxOutputTokens: 200 });
    expect(body.max_tokens).toBe(200);
  });

  it("rejects requests outside the whitelist before consuming RPM", async () => {
    getClientIp.mockReturnValue("198.51.100.8");
    const response = await enforceApiKeyRequest(
      new Request("https://example.test/v1/chat/completions"),
      { id: "key-1", ipWhitelist: ["192.0.2.4"], requestsPerMinute: 20 }
    );
    expect(response.status).toBe(403);
    expect(consumeApiKeyRequest).not.toHaveBeenCalled();
  });

  it("enforces RPM and returns a Retry-After header", async () => {
    consumeApiKeyRequest.mockResolvedValue({ allowed: false, retryAfterSeconds: 17 });
    const response = await enforceApiKeyRequest(
      new Request("https://example.test/v1/chat/completions"),
      { id: "key-1", ipWhitelist: null, requestsPerMinute: 20 }
    );
    expect(response.status).toBe(429);
    expect(response.headers.get("Retry-After")).toBe("17");
  });
});
