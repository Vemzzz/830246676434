import { describe, expect, it } from "vitest";
import { isComboAllowed, isKindAllowed, isProviderAllowed } from "../../src/sse/services/auth.js";
import { PROVIDER_MODELS } from "open-sse/config/providerModels.js";

describe("model catalog and ACL invariants", () => {
  it("preserves static models when live discovery is partial", () => {
    const staticIds = PROVIDER_MODELS.gcli.map((m) => m.id);
    const merged = new Set([...staticIds, "grok-build"]);
    expect(merged).toEqual(new Set(staticIds));
  });

  it("does not replace static catalog when live discovery is empty", () => {
    const staticIds = PROVIDER_MODELS.gcli.map((m) => m.id);
    const live = [];
    const merged = live.length ? new Set(live) : new Set(staticIds);
    expect([...merged]).toEqual(staticIds);
  });

  it("treats explicit enabledModels as a hard connection allowlist", () => {
    const enabled = ["grok-build"];
    expect(enabled).toEqual(["grok-build"]);
    expect(enabled).not.toContain("grok-4.5");
  });

  it("allows no-auth/free providers without a connection by default", async () => {
    expect(await isProviderAllowed({ allowedProviders: null }, "opencode")).toBe(true);
  });

  it("resolves gcli ACL through the canonical grok-cli alias", async () => {
    expect(await isProviderAllowed({ allowedProviders: ["gcli"] }, "grok-cli")).toBe(true);
    expect(await isProviderAllowed({ allowedProviders: ["grok-cli"] }, "gcli")).toBe(true);
  });

  it("keeps provider, combo, and kind ACLs independent", async () => {
    const key = { allowedProviders: ["gcli"], allowedCombos: ["fallback"], allowedKinds: ["llm"] };
    expect(await isProviderAllowed(key, "grok-cli")).toBe(true);
    expect(isComboAllowed(key, "fallback")).toBe(true);
    expect(isComboAllowed(key, "fusion")).toBe(false);
    expect(isKindAllowed(key, "llm")).toBe(true);
    expect(isKindAllowed(key, "image")).toBe(false);
  });
});
