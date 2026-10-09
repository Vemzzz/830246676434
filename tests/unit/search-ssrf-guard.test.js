import { describe, expect, it } from "vitest";
import { resolveBaseUrl } from "../../open-sse/handlers/search/callers.js";

const CONFIG = { id: "searxng", baseUrl: "https://searxng.example.com" };

describe("resolveBaseUrl SSRF guard", () => {
  it("returns the configured provider URL without validating admin config", async () => {
    await expect(resolveBaseUrl(CONFIG, {})).resolves.toBe("https://searxng.example.com");
  });

  it.each([
    "http://127.0.0.1:18999",
    "http://10.0.0.1",
    "http://169.254.169.254/latest/meta-data",
    "http://localhost:8080",
    "file:///etc/passwd",
  ])("rejects client-controlled target %s", async (baseUrl) => {
    await expect(
      resolveBaseUrl(CONFIG, { providerOptions: { baseUrl } }),
    ).rejects.toThrow(/Blocked URL|Invalid baseUrl/);
  });
});
