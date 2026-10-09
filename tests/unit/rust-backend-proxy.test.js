import { beforeEach, describe, expect, it, vi } from "vitest";

const { resetComboRotation, invalidateAllowedModelsCache } = vi.hoisted(() => ({
  resetComboRotation: vi.fn(),
  invalidateAllowedModelsCache: vi.fn(),
}));

vi.mock("open-sse/services/combo.js", () => ({ resetComboRotation }));
vi.mock("@/sse/services/allowedModels.js", () => ({ invalidateAllowedModelsCache }));

import {
  isRustBackendEnabled,
  isRustChatModelSupported,
  proxyRustAdminRequest,
  proxyRustAuthRequest,
  proxyRustChatRequest,
  proxyRustModelInfoRequest,
  proxyRustModelsRequest,
} from "@/lib/rustBackendProxy.js";

describe("proxyRustAdminRequest combo cache invalidation", () => {
  beforeEach(() => {
    vi.stubEnv("NEXT_PUBLIC_RUST_API_URL", "https://rust.example/");
    vi.stubGlobal("fetch", vi.fn(async () => new Response('{"success":true}', { status: 200 })));
    resetComboRotation.mockClear();
    invalidateAllowedModelsCache.mockClear();
  });

  describe("proxyRustAuthRequest", () => {
    beforeEach(() => {
      vi.stubEnv("NEXT_PUBLIC_RUST_API_URL", "https://rust.example/");
      vi.stubGlobal("fetch", vi.fn(async () => new Response('{"success":true}', {
        status: 200,
        headers: [
          ["content-type", "application/json"],
          ["set-cookie", "auth_token=signed-token; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400; Secure"],
        ],
      })));
    });

    it("uses one RUST_BE flag to enable Rust routing", () => {
      vi.stubEnv("RUST_BE", "true");
      expect(isRustBackendEnabled()).toBe(true);
      vi.stubEnv("RUST_BE", "false");
      expect(isRustBackendEnabled()).toBe(false);
    });

    it("routes only chat models Rust currently supports", () => {
      expect(isRustChatModelSupported("openai/gpt-4.1")).toBe(true);
      expect(isRustChatModelSupported("anthropic/claude-sonnet")).toBe(false);
      expect(isRustChatModelSupported("gpt-4.1")).toBe(false);
    });

    it("maps the Next chat API path to Rust's OpenAI chat endpoint", async () => {
      vi.stubEnv("NEXT_PUBLIC_RUST_API_URL", "https://rust.example/");
      const request = new Request("https://dashboard.example/api/v1/chat/completions?trace=1", {
        method: "POST",
        body: JSON.stringify({ model: "openai/gpt-4.1" }),
      });

      const response = await proxyRustChatRequest(request);

      expect(response.status).toBe(200);
      expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/v1/chat/completions?trace=1");
    });

    it("maps the dashboard OpenAI models route to Rust's public models endpoint", async () => {
      const request = new Request("https://dashboard.example/api/v1/models?kind=llm", {
        headers: { authorization: "Bearer api-key" },
      });

      const response = await proxyRustModelsRequest(request);
      const [, options] = fetch.mock.calls.at(-1);

      expect(response.status).toBe(200);
      expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/v1/models?kind=llm");
      expect(options.headers.get("authorization")).toBe("Bearer api-key");
    });

    it("maps the model info API to Rust and forwards query parameters", async () => {
      const request = new Request(
        "https://dashboard.example/api/v1/models/info?id=openai%2Fgpt-4.1&kind=llm",
      );

      const response = await proxyRustModelInfoRequest(request);

      expect(response.status).toBe(200);
      expect(fetch.mock.calls.at(-1)[0]).toBe(
        "https://rust.example/v1/models/info?id=openai%2Fgpt-4.1&kind=llm",
      );
    });

    it.each([
      [
        "/api/providers/suggested-models?type=openrouter-free&url=https%3A%2F%2Fmodels.example%2Flist",
        "https://rust.example/api/providers/suggested-models?type=openrouter-free&url=https%3A%2F%2Fmodels.example%2Flist",
      ],
      [
        "/api/providers/kilo/free-models",
        "https://rust.example/api/providers/kilo/free-models",
      ],
      [
        "/api/providers/client?provider=codex&page=2",
        "https://rust.example/api/providers/client?provider=codex&page=2",
      ],
    ])("maps provider discovery endpoint %s to Rust", async (path, expectedUrl) => {
      const response = await proxyRustAdminRequest(
        new Request(`https://dashboard.example${path}`),
      );

      expect(response.status).toBe(200);
      expect(fetch.mock.calls.at(-1)[0]).toBe(expectedUrl);
    });

    it("forwards request cookies and preserves Rust session cookies", async () => {
      const request = new Request("https://dashboard.example/api/auth/login", {
        method: "POST",
        headers: { cookie: "password_change=bootstrap", "content-type": "application/json" },
        body: JSON.stringify({ password: "correct-horse" }),
      });

      const response = await proxyRustAuthRequest(request);
      const [, options] = fetch.mock.calls[0];

      expect(response.status).toBe(200);
      expect(options.headers.get("cookie")).toBe("password_change=bootstrap");
      expect(response.headers.get("set-cookie")).toContain("auth_token=signed-token");
    });

    it("preserves Rust OIDC redirects through the Next.js proxy", async () => {
      vi.stubGlobal("fetch", vi.fn(async () => new Response(null, {
        status: 302,
        headers: { location: "https://issuer.example/authorize?state=opaque" },
      })));
      const request = new Request("https://dashboard.example/api/auth/oidc/start", {
        headers: { "sec-fetch-mode": "navigate", "sec-fetch-dest": "document" },
      });

      const response = await proxyRustAuthRequest(request);

      expect(response.status).toBe(302);
      expect(response.headers.get("location")).toBe("https://issuer.example/authorize?state=opaque");
    });
  });

  it.each([
    ["POST", "/api/combos"],
    ["PUT", "/api/combos/combo-id"],
    ["DELETE", "/api/combos/combo-id"],
  ])("invalidates local combo state after successful %s %s", async (method, path) => {
    const request = new Request(`https://dashboard.example${path}?source=test`, {
      method,
      headers: { cookie: "auth_token=session", "x-9r-cli-token": "cli-token" },
      ...(method === "DELETE" ? {} : { body: "{}" }),
    });

    const response = await proxyRustAdminRequest(request);

    expect(response.status).toBe(200);
    expect(resetComboRotation).toHaveBeenCalledOnce();
    expect(invalidateAllowedModelsCache).toHaveBeenCalledOnce();
    const [url, options] = fetch.mock.calls[0];
    expect(url).toBe(`https://rust.example${path}?source=test`);
    expect(options.headers.get("cookie")).toBe("auth_token=session");
    expect(options.headers.get("x-9r-cli-token")).toBe("cli-token");
  });

  it("does not invalidate local caches when Rust rejects a mutation", async () => {
    fetch.mockResolvedValueOnce(new Response('{"error":"invalid"}', { status: 400 }));
    const response = await proxyRustAdminRequest(new Request("https://dashboard.example/api/combos", {
      method: "POST",
      body: "{}",
    }));

    expect(response.status).toBe(400);
    expect(resetComboRotation).not.toHaveBeenCalled();
    expect(invalidateAllowedModelsCache).not.toHaveBeenCalled();
  });

  it("does not invalidate local caches for reads or unrelated routes", async () => {
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/combos"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/models"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/settings/require-login"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/usage/providers"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/usage/request-details?page=2"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/usage/logs"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/usage/request-logs"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/models/availability"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/ready"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/init"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/pricing"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/version"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/health"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/locale", {
      method: "POST",
      body: JSON.stringify({ locale: "id" }),
    }));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/keys?active=true"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/providers", {
      method: "POST",
      body: "{}",
    }));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/providers/openai/overrides"));
    await proxyRustAdminRequest(new Request("https://dashboard.example/api/provider-nodes", {
      method: "POST",
      body: "{}",
    }));

    expect(resetComboRotation).not.toHaveBeenCalled();
    expect(invalidateAllowedModelsCache).not.toHaveBeenCalled();
  });

  it("proxies dashboard login-requirement reads to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/settings/require-login",
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe(
      "https://rust.example/api/settings/require-login",
    );
  });

  it("proxies usage-provider reads to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/usage/providers",
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe(
      "https://rust.example/api/usage/providers",
    );
  });

  it("proxies request-detail collection and resource paths to Rust", async () => {
    const listResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/usage/request-details?page=2&pageSize=10&provider=openai",
    ));
    const detailResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/usage/request-details/detail-id",
    ));

    expect(listResponse.status).toBe(200);
    expect(detailResponse.status).toBe(200);
    expect(fetch.mock.calls.at(-2)[0]).toBe(
      "https://rust.example/api/usage/request-details?page=2&pageSize=10&provider=openai",
    );
    expect(fetch.mock.calls.at(-1)[0]).toBe(
      "https://rust.example/api/usage/request-details/detail-id",
    );
  });

  it("proxies usage logs and legacy request logs to Rust", async () => {
    const logsResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/usage/logs",
    ));
    const legacyResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/usage/request-logs",
    ));

    expect(logsResponse.status).toBe(200);
    expect(legacyResponse.status).toBe(200);
    expect(fetch.mock.calls.at(-2)[0]).toBe("https://rust.example/api/usage/logs");
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/usage/request-logs");
  });

  it("proxies model availability reads and cooldown resets to Rust", async () => {
    const getResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/models/availability",
      { headers: { cookie: "auth_token=session" } },
    ));
    const postResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/models/availability",
      {
        method: "POST",
        headers: { cookie: "auth_token=session", "content-type": "application/json" },
        body: JSON.stringify({ action: "clearCooldown", provider: "openai", model: "gpt-4.1" }),
      },
    ));

    expect(getResponse.status).toBe(200);
    expect(postResponse.status).toBe(200);
    expect(fetch.mock.calls.at(-2)[0]).toBe("https://rust.example/api/models/availability");
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/models/availability");
    expect(fetch.mock.calls.at(-1)[1].method).toBe("POST");
  });

  it("proxies provider header override reads and writes to Rust", async () => {
    const getResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/providers/openai/overrides",
    ));
    const putResponse = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/providers/openai/overrides",
      {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ headers: { "X-Custom": "value" } }),
      },
    ));

    expect(getResponse.status).toBe(200);
    expect(putResponse.status).toBe(200);
    expect(fetch.mock.calls.at(-2)[0]).toBe(
      "https://rust.example/api/providers/openai/overrides",
    );
    expect(fetch.mock.calls.at(-1)[0]).toBe(
      "https://rust.example/api/providers/openai/overrides",
    );
    expect(fetch.mock.calls.at(-1)[1].method).toBe("PUT");
  });

  it("proxies database readiness checks to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/ready",
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/ready");
  });

  it("proxies pricing reads and mutations to Rust", async () => {
    for (const [method, body, query] of [
      ["GET", undefined, ""],
      ["PATCH", JSON.stringify({ openai: { model: { input: 1 } } }), ""],
      ["DELETE", undefined, "?provider=openai&model=model"],
    ]) {
      const response = await proxyRustAdminRequest(new Request(
        `https://dashboard.example/api/pricing${query}`,
        { method, ...(body ? { body } : {}) },
      ));

      expect(response.status).toBe(200);
      expect(fetch.mock.calls.at(-1)[0]).toBe(
        `https://rust.example/api/pricing${query}`,
      );
    }
  });

  it("proxies the initialization probe to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/init",
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/init");
  });

  it("proxies version requests to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/version",
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/version");
  });

  it("preserves CORS headers from Rust health responses", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(
      JSON.stringify({ ok: true }),
      {
        status: 200,
        headers: {
          "content-type": "application/json",
          "access-control-allow-origin": "*",
        },
      },
    )));
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/health",
    ));

    expect(response.status).toBe(200);
    expect(response.headers.get("access-control-allow-origin")).toBe("*");
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/health");
  });

  it("proxies locale updates to Rust", async () => {
    const response = await proxyRustAdminRequest(new Request(
      "https://dashboard.example/api/locale",
      { method: "POST", body: JSON.stringify({ locale: "id" }) },
    ));

    expect(response.status).toBe(200);
    expect(fetch.mock.calls.at(-1)[0]).toBe("https://rust.example/api/locale");
  });

  it.each([
    ["PUT", "/api/models"],
    ["PUT", "/api/models/alias"],
    ["POST", "/api/models/custom"],
    ["DELETE", "/api/models/disabled?providerAlias=openai&id=model-a"],
  ])("invalidates allowed-model cache after successful %s %s", async (method, path) => {
    const response = await proxyRustAdminRequest(new Request(`https://dashboard.example${path}`, {
      method,
      ...(method === "DELETE" ? {} : { body: "{}" }),
    }));

    expect(response.status).toBe(200);
    expect(invalidateAllowedModelsCache).toHaveBeenCalledOnce();
    expect(resetComboRotation).not.toHaveBeenCalled();
  });
});
