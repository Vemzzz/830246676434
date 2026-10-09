import { describe, expect, it, vi } from "vitest";

// The POST handler wrote every `selections[].role` straight into `new RegExp(...)`
// (auxRoleRe) and into a YAML key, so a role of "(" threw an uncaught
// SyntaxError (500) and a crafted role/model could inject sibling YAML keys.
// Validation now rejects those before the config file is touched, which is why
// these cases never reach the filesystem.
vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal();
  return {
    ...actual,
    mkdir: vi.fn().mockResolvedValue(undefined),
    readFile: vi.fn().mockResolvedValue(""),
    writeFile: vi.fn().mockResolvedValue(undefined),
  };
});

const { POST } = await import("../../src/app/api/cli-tools/hermes-settings/route.js");

const post = (body) =>
  POST(
    new Request("http://127.0.0.1/api/cli-tools/hermes-settings", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
  );

describe("hermes-settings POST input validation", () => {
  const base = { baseUrl: "http://127.0.0.1:20128" };

  it("rejects a role that would break the RegExp instead of throwing", async () => {
    const res = await post({ ...base, selections: [{ role: "(", model: "gpt-5" }] });
    expect(res.status).toBe(400);
    expect((await res.json()).error).toMatch(/Role must be/);
  });

  it("rejects a role that would inject a sibling YAML key", async () => {
    const res = await post({ ...base, selections: [{ role: "vision:\n    x", model: "gpt-5" }] });
    expect(res.status).toBe(400);
  });

  it("rejects a model that would break out of the quoted scalar", async () => {
    const res = await post({ ...base, selections: [{ role: "vision", model: 'gpt-5"\n    injected: "1' }] });
    expect(res.status).toBe(400);
  });

  it("rejects a baseUrl containing a quote or newline", async () => {
    const res = await post({
      baseUrl: 'http://x"\n    injected: "1',
      selections: [{ role: "default", model: "gpt-5" }],
    });
    expect(res.status).toBe(400);
  });

  it("still rejects a payload with no default role", async () => {
    const res = await post({ ...base, selections: [{ role: "vision", model: "gpt-5" }] });
    expect(res.status).toBe(400);
    expect((await res.json()).error).toMatch(/baseUrl and model are required/);
  });
});
