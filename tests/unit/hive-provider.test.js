import { describe, expect, it } from "vitest";
import REGISTRY from "../../open-sse/providers/registry/index.js";
import { PROVIDERS, PROVIDER_MODELS } from "../../open-sse/config/providers.js";
import { resolveProviderAlias } from "../../open-sse/services/model.js";
import { resolveProviderId } from "@/shared/constants/providers.js";
import { getCapabilitiesForModel } from "../../open-sse/providers/capabilities.js";
import { deriveValidateUrl } from "../../open-sse/providers/schema.js";
import { getModelUpstreamId } from "../../open-sse/config/providerModels.js";

describe("Hive AI provider", () => {
  const hive = REGISTRY.find((e) => e.id === "hive");

  it("is registered in REGISTRY and into runtime PROVIDERS", () => {
    expect(hive).toBeDefined();
    expect(hive.category).toBe("apikey");
    expect(hive.alias).toBe("hive");
    expect(hive.aliases).toContain("hive-ai");
    expect(hive.transport.baseUrl).toBe("https://api.thehive.ai/api/v3/chat/completions");

    expect(PROVIDERS.hive).toBeDefined();
    expect(PROVIDERS.hive.format).toBe("openai");
    expect(PROVIDERS.hive.baseUrl).toBe("https://api.thehive.ai/api/v3/chat/completions");
  });

  // Hive serves no GET-able models endpoint (404), so pinning validateUrl at the
  // chat path would only mislead the model-list and connection-test probes.
  it("declares no validateUrl, leaving deriveValidateUrl to build /models", () => {
    expect(hive.transport.validateUrl).toBeUndefined();
    expect(deriveValidateUrl(PROVIDERS.hive)).toBe("https://api.thehive.ai/api/v3/models");
  });

  it("resolves aliases 'hive' and 'hive-ai'", () => {
    expect(resolveProviderAlias("hive")).toBe("hive");
    expect(resolveProviderAlias("hive-ai")).toBe("hive");
    expect(resolveProviderId("hive")).toBe("hive");
    expect(resolveProviderId("hive-ai")).toBe("hive");
  });

  it("exposes the upstream keys plus short aliases that rewrite to them", () => {
    const models = Object.fromEntries((PROVIDER_MODELS.hive || []).map((m) => [m.id, m]));
    expect(Object.keys(models)).toEqual([
      "deepseek-ai/deepseek-v4.1-flash",
      "zai-org/glm-5.3-flash",
      "deepseek-v4.1-flash",
      "glm-5.3-flash",
    ]);
    expect(models["deepseek-v4.1-flash"].upstreamModelId).toBe("deepseek-ai/deepseek-v4.1-flash");
    expect(models["glm-5.3-flash"].upstreamModelId).toBe("zai-org/glm-5.3-flash");
    expect(models["deepseek-ai/deepseek-v4.1-flash"].upstreamModelId).toBeUndefined();
    // The short alias must resolve to the vendor id before it reaches the upstream
    // request body: Hive rejects the bare form with 400 "Invalid Model Name".
    expect(getModelUpstreamId("hive", "glm-5.3-flash")).toBe("zai-org/glm-5.3-flash");
    expect(getModelUpstreamId("hive", "deepseek-v4.1-flash")).toBe("deepseek-ai/deepseek-v4.1-flash");
  });

  it("reports 1M context, thinking format, and multimodal input for both models", () => {
    for (const [model, videoInput] of [
      ["deepseek-ai/deepseek-v4.1-flash", false],
      ["zai-org/glm-5.3-flash", true],
    ]) {
      const caps = getCapabilitiesForModel("hive", model);
      expect(caps.contextWindow, model).toBe(1000000);
      expect(caps.vision, model).toBe(true);
      expect(caps.videoInput, model).toBe(videoInput);
      expect(caps.reasoning, model).toBe(true);
      // Genuine config deltas vs DEFAULT_CAPABILITIES (thinkingFormat null,
      // thinkingCanDisable true) — these catch a Hive config regression, unlike
      // `tools`, whose default is already true for every provider.
      expect(caps.thinkingFormat, model).toBe("openai");
      // Reasoning streams on every turn regardless of request params, so a client
      // must not be told it can switch thinking off.
      expect(caps.thinkingCanDisable, model).toBe(false);
      // Live 2026-10-01: Hive's GLM stream closes the tool call after
      // function.name with no arguments (completion_tokens: 1), while the same
      // route keeps them for DeepSeek — so GLM must not advertise tool calling.
      expect(caps.tools, model).toBe(model.endsWith("glm-5.3-flash") ? false : true);
    }
  });

  it("supports passthrough models", () => {
    expect(hive.passthroughModels).toBe(true);
  });
});
