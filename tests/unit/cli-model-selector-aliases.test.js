import { describe, expect, it } from "vitest";

import { buildActiveAliases } from "../../cli/src/cli/utils/modelSelector.js";

// /v1/models stamps `owned_by` with the provider's alias (getProviderAlias), while
// the CLI used to resolve id→alias from a hand-written table. That table had
// drifted from the registry, so a connected provider whose alias it got wrong was
// filtered out of the picker entirely. The alias set now comes from the
// /api/providers payload instead.
describe("cli model selector alias set", () => {
  const payload = {
    connections: [
      { provider: "cline", isActive: true, alias: "cl" },
      { provider: "zed", isActive: true, alias: "zd" },
      { provider: "qoder-cn", isActive: true, alias: "qdcn" },
      { provider: "codebuddy-cn", isActive: true, alias: "cbcn" },
      { provider: "grok-cli", isActive: true, alias: "gcli" },
      { provider: "claude", isActive: true, alias: "cc" },
      { provider: "custom-node-1", isActive: true, providerSpecificData: { prefix: "customprefix" } },
      { provider: "openai", isActive: false },
    ],
    aliasMap: { cl: "cline", zd: "zed", qdcn: "qoder-cn", cbcn: "codebuddy-cn", gcli: "grok-cli", cc: "claude" },
    providers: [
      { id: "opencode", alias: "oc", aliases: [], noAuth: true },
      { id: "searxng", alias: null, aliases: [], noAuth: true },
      { id: "claude", alias: "cc", aliases: [], noAuth: false },
    ],
  };

  it("admits every connected provider's alias, including ones no literal map listed", () => {
    const active = buildActiveAliases(payload);
    for (const alias of ["cl", "zd", "qdcn", "cbcn", "gcli", "cc"]) {
      expect(active.has(alias), `${alias} must survive the filter`).toBe(true);
    }
    expect(active.has("customprefix")).toBe(true);
  });

  it("admits no-auth providers from the payload even without a connection", () => {
    const active = buildActiveAliases(payload);
    expect(active.has("opencode")).toBe(true);
    expect(active.has("oc")).toBe(true);
    expect(active.has("searxng")).toBe(true);
  });

  it("ignores inactive connections and tolerates an empty payload", () => {
    const active = buildActiveAliases(payload);
    expect(active.has("openai")).toBe(false);
    expect([...buildActiveAliases()].length).toBe(0);
    expect([...buildActiveAliases({ connections: [null], providers: [null] })].length).toBe(0);
  });
});
