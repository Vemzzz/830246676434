import { describe, expect, it } from "vitest";

import REGISTRY from "../../open-sse/providers/registry/index.js";
import { FREE_REFRESH_VALUES, listFreeTiers } from "open-sse/providers/freeTiers.js";

const ORDER = ["daily", "weekly", "monthly", "rolling", "one-time"];
const rank = (v) => (v === null ? ORDER.length : ORDER.indexOf(v));
const rowsFor = (id) => listFreeTiers().filter((t) => t.providerId === id);

describe("freeTiers.listFreeTiers", () => {
  it("returns a non-empty catalogue with an evidence-backed daily provider row", () => {
    const tiers = listFreeTiers();
    expect(tiers.length).toBeGreaterThan(0);
    expect(tiers.every((t) => !("hasFree" in t))).toBe(true);

    const openrouter = rowsFor("openrouter");
    expect(openrouter.length).toBeGreaterThan(0);
    const providerRow = openrouter.find((t) => t.modelId === null);
    expect(providerRow).toBeDefined();
    expect(providerRow.freeRefresh).toBe("daily");
  });

  it("emits exactly one provider-level row per free provider, never fanning out to paid models", () => {
    // gemini is `category: "freeTier"` — it has a free allowance, it is not all-free.
    const gemini = rowsFor("gemini");
    expect(gemini).toHaveLength(1);
    expect(gemini[0].modelId).toBeNull();
    expect(gemini[0].modelName).toBeNull();
    expect(gemini[0].kind).toBeNull();

    // claude and codex are subscription-gated paid offerings, not free tiers.
    expect(rowsFor("claude")).toHaveLength(0);
    expect(rowsFor("codex")).toHaveLength(0);
  });

  it("emits model rows for a `category: free` provider, inheriting the provider cadence (opencode)", () => {
    const rows = rowsFor("opencode");
    expect(rows.length).toBeGreaterThan(1);
    for (const row of rows) expect(row.freeRefresh).toBeNull();
  });

  it("emits only explicitly-marked free models on otherwise-mixed providers (openrouter)", () => {
    const rows = rowsFor("openrouter");
    expect(rows.find((t) => t.modelId === null)).toBeDefined();

    const modelRows = rows.filter((t) => t.modelId !== null);
    expect(modelRows.length).toBeGreaterThan(0);
    for (const row of modelRows) expect(row.modelId.endsWith(":free")).toBe(true);

    // A paid OpenRouter model must never be labelled free.
    expect(rows.some((t) => t.modelId === "black-forest-labs/FLUX.1-schnell")).toBe(false);
    expect(rows.some((t) => t.modelId === "bytedance/seedance-2.0")).toBe(false);
  });

  it("emits only null or allowlisted freeRefresh values", () => {
    for (const tier of listFreeTiers()) {
      expect(tier.freeRefresh === null || FREE_REFRESH_VALUES.includes(tier.freeRefresh)).toBe(true);
    }
  });

  it("keeps the total row count well under the fan-out ceiling", () => {
    expect(listFreeTiers().length).toBeLessThan(200);
  });

  it("sorts deterministically: cadence groups monotonic, unknown last, idempotent", () => {
    const first = listFreeTiers();
    const second = listFreeTiers();
    expect(second).toEqual(first);

    let lastRank = -1;
    for (const tier of first) {
      const r = rank(tier.freeRefresh);
      expect(r).toBeGreaterThanOrEqual(lastRank);
      lastRank = r;
    }
    expect(first[0].freeRefresh).toBe("daily");
    expect(rank(first.at(-1).freeRefresh)).toBe(ORDER.length);
  });

  it("emits no duplicate (providerId, modelId) rows", () => {
    const seen = new Set();
    for (const tier of listFreeTiers()) {
      const key = `${tier.providerId}::${tier.modelId}`;
      expect(seen.has(key)).toBe(false);
      seen.add(key);
    }
  });

  it("lets a model-level freeRefresh override the provider-level value", () => {
    // `category: "free"` is the case where model rows are emitted for every model,
    // so this is the only place the provider cadence can be inherited/overridden.
    const probe = {
      id: "__free-tier-probe__",
      category: "free",
      hasFree: true,
      freeRefresh: "monthly",
      alias: "probe",
      display: { name: "Free Tier Probe" },
      models: [
        { id: "inherit", name: "Inherit" },
        { id: "override", name: "Override", hasFree: true, freeRefresh: "weekly" },
      ],
    };
    REGISTRY.push(probe);
    try {
      const rows = listFreeTiers().filter((t) => t.providerId === "__free-tier-probe__");
      const byModel = Object.fromEntries(rows.map((r) => [r.modelId, r.freeRefresh]));
      expect(byModel[null]).toBe("monthly");
      expect(byModel.inherit).toBe("monthly");
      expect(byModel.override).toBe("weekly");
    } finally {
      REGISTRY.splice(REGISTRY.indexOf(probe), 1);
    }
  });

  it("throws on a freeRefresh typo instead of shipping it", () => {
    const probe = {
      id: "__free-tier-bad-probe__",
      category: "freeTier",
      hasFree: true,
      freeRefresh: "fortnightly",
      display: { name: "Bad Probe" },
      models: [],
    };
    REGISTRY.push(probe);
    try {
      expect(() => listFreeTiers()).toThrow(/Invalid freeRefresh/);
    } finally {
      REGISTRY.splice(REGISTRY.indexOf(probe), 1);
    }
  });
});
