import { describe, expect, it } from "vitest";
import { fitToolName, TOOL_NAME_MAX_LENGTH } from "../../open-sse/translator/concerns/toolCall.js";

// Built to length rather than eyeballed, so these cases cannot silently stop
// being over the limit when someone edits a string.
const repeatTo = (seed, len) => (seed + "x".repeat(len)).slice(0, len);

describe("fitToolName", () => {
  it("leaves a name that already fits untouched", () => {
    expect(fitToolName("get_weather")).toBe("get_weather");
    const exact = repeatTo("a", TOOL_NAME_MAX_LENGTH);
    expect(fitToolName(exact)).toBe(exact);
  });

  it("shortens the over-long name from issue #148", () => {
    const name = repeatTo("mcp__github__create_pull_request_review_", 68);
    expect(name).toHaveLength(68);

    const fitted = fitToolName(name);
    expect(fitted.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
    expect(fitted).not.toBe(name);
  });

  it("keeps two tools distinct when a plain slice would merge them", () => {
    // A plain slice(0, 64) collapses both of these onto the same name, which
    // would let the model silently pick the wrong tool.
    const shared = repeatTo("mcp__github__create_pull_request_review_", TOOL_NAME_MAX_LENGTH);
    const a = `${shared}alpha`;
    const b = `${shared}beta`;
    expect(a.slice(0, TOOL_NAME_MAX_LENGTH)).toBe(b.slice(0, TOOL_NAME_MAX_LENGTH));

    const taken = new Set();
    const fittedA = fitToolName(a, TOOL_NAME_MAX_LENGTH, taken);
    taken.add(fittedA);
    const fittedB = fitToolName(b, TOOL_NAME_MAX_LENGTH, taken);

    expect(fittedB).not.toBe(fittedA);
    expect(fittedA.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
    expect(fittedB.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
  });

  it("is deterministic, so a retried request keeps the same name", () => {
    const name = repeatTo("mcp__x__tool_", 90);
    expect(fitToolName(name)).toBe(fitToolName(name));
  });

  it("passes through input it must not invent names for", () => {
    expect(fitToolName("")).toBe("");
    expect(fitToolName(undefined)).toBe(undefined);
  });
});
