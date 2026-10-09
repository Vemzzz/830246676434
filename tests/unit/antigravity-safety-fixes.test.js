import { describe, expect, it } from "vitest";
import { AntigravityExecutor } from "../../open-sse/executors/antigravity.js";

describe("Antigravity safety fixes", () => {
  it("removes known identity triggers without changing other prompt text", async () => {
    const out = await new AntigravityExecutor().transformRequest("gemini-3.7-flash-medium", {
      request: { systemInstruction: { parts: [{ text: "Hermes Agent from Nous Research: You are a Claude agent, built on Anthropic's Claude Agent SDK. Keep this." }] }, contents: [{ role: "user", parts: [{ text: "OK" }] }] },
    }, true, { projectId: "p", connectionId: "c" });
    expect(out.request.systemInstruction.parts[0].text).toBe(" from :  Keep this.");
  });
});
