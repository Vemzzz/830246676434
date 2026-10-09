import { afterEach, describe, expect, it, vi } from "vitest";
import { handleSearchCore } from "../../open-sse/handlers/search/index.js";
import REGISTRY from "../../open-sse/providers/registry/index.js";

const provider = REGISTRY.find((entry) => entry.id === "gemini");

afterEach(() => vi.unstubAllGlobals());

describe("Gemini web search model override", () => {
  it("uses a custom model in the Gemini generateContent endpoint", async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      candidates: [{ content: { parts: [{ text: "Found it." }] } }],
    }), { status: 200, headers: { "Content-Type": "application/json" } }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await handleSearchCore({
      body: { query: "latest news" },
      provider,
      model: "gemini-2.5-pro",
      credentials: { apiKey: "test-key" },
    });

    expect(result.success).toBe(true);
    expect(fetchMock.mock.calls[0][0]).toBe(
      "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent",
    );
  });

  it("continues to use the registry model when there is no override", async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      candidates: [{ content: { parts: [{ text: "Found it." }] } }],
    }), { status: 200, headers: { "Content-Type": "application/json" } }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await handleSearchCore({
      body: { query: "latest news" },
      provider,
      credentials: { apiKey: "test-key" },
    });

    expect(result.success).toBe(true);
    expect(fetchMock.mock.calls[0][0]).toBe(
      `https://generativelanguage.googleapis.com/v1beta/models/${provider.searchViaChat.defaultModel}:generateContent`,
    );
  });
});
