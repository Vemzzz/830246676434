import { beforeEach, describe, expect, it, vi } from "vitest";

const { handleChat, proxyRustChatRequest, initTranslators } = vi.hoisted(() => ({
  handleChat: vi.fn(),
  proxyRustChatRequest: vi.fn(),
  initTranslators: vi.fn(),
}));

vi.mock("@/sse/handlers/chat.js", () => ({ handleChat }));
vi.mock("open-sse/translator/index.js", () => ({ initTranslators }));
vi.mock("@/lib/rustBackendProxy", () => ({
  isRustBackendEnabled: () => process.env.RUST_BE === "true",
  isRustChatModelSupported: (model) => typeof model === "string"
    && (model.startsWith("openai/") || model.startsWith("combo/")),
  proxyRustChatRequest,
}));

const { POST } = await import("@/app/api/v1/chat/completions/route.js");

function request(model) {
  return new Request("https://router.example/v1/chat/completions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ model, messages: [] }),
  });
}

describe("Rust chat routing", () => {
  beforeEach(() => {
    vi.stubEnv("RUST_BE", "true");
    vi.clearAllMocks();
    handleChat.mockImplementation(async () => new Response("node"));
    proxyRustChatRequest.mockImplementation(async () => new Response("rust"));
  });

  it("routes explicitly supported OpenAI models to Rust", async () => {
    const response = await POST(request("openai/gpt-4.1"));

    expect(await response.text()).toBe("rust");
    expect(proxyRustChatRequest).toHaveBeenCalledOnce();
    expect(handleChat).not.toHaveBeenCalled();
  });

  it("routes configured combos to Rust for native fallback", async () => {
    const response = await POST(request("combo/coding-stack"));

    expect(await response.text()).toBe("rust");
    expect(proxyRustChatRequest).toHaveBeenCalledOnce();
    expect(handleChat).not.toHaveBeenCalled();
  });

  it("keeps unported provider models on Node", async () => {
    const response = await POST(request("anthropic/claude-sonnet"));

    expect(await response.text()).toBe("node");
    expect(proxyRustChatRequest).not.toHaveBeenCalled();
    expect(handleChat).toHaveBeenCalledOnce();
  });

  it("falls back to Node when Rust cannot enforce an API-key IP whitelist", async () => {
    proxyRustChatRequest.mockResolvedValueOnce(new Response(JSON.stringify({
      error: {
        message: "Rust chat IP-whitelisted keys require a trusted client-IP proxy configuration",
      },
    }), { status: 403 }));

    const response = await POST(request("openai/gpt-4.1"));

    expect(await response.text()).toBe("node");
    expect(handleChat).toHaveBeenCalledOnce();
  });

  it("does not hide unrelated Rust authorization failures", async () => {
    proxyRustChatRequest.mockResolvedValueOnce(new Response(JSON.stringify({
      error: { message: "API key is not allowed" },
    }), { status: 403 }));

    const response = await POST(request("openai/gpt-4.1"));

    expect(response.status).toBe(403);
    expect(handleChat).not.toHaveBeenCalled();
  });
});
