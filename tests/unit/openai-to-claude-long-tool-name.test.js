/**
 * Regression for issue #148: a custom OpenAI-compatible node pointed at an
 * Anthropic-compatible gateway rejected every request with
 *
 *   `name` must be at most 64 characters, got 68
 *
 * The name came from the client and VansRouter forwarded it verbatim, so the
 * whole request failed on any client using MCP tools (mcp__<server>__<action>
 * runs long). These tests pin the three things that have to hold together:
 * the name is shortened, the client still gets its own name back, and a forced
 * tool_choice points at the name that was actually declared.
 */

import { describe, it, expect } from "vitest";
import { openaiToClaudeRequest } from "../../open-sse/translator/request/openai-to-claude.js";
import { restoreToolName, TOOL_NAME_MAX_LENGTH } from "../../open-sse/translator/concerns/toolCall.js";

const repeatTo = (seed, len) => (seed + "x".repeat(len)).slice(0, len);
const longName = repeatTo("mcp__github__create_pull_request_review_", 68);
const tool = (name) => ({ type: "function", function: { name, parameters: { type: "object" } } });
const body = (extra) => ({ messages: [{ role: "user", content: "hi" }], ...extra });

describe("openai→claude: over-long tool names (issue #148)", () => {
  it("never sends a name past the limit upstream", () => {
    expect(longName).toHaveLength(68);
    const out = openaiToClaudeRequest("claude-sonnet-4.5", body({ tools: [tool(longName)] }), false);

    expect(out.tools[0].name.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
  });

  it("hands the caller's own name back on the way out", () => {
    const out = openaiToClaudeRequest("claude-sonnet-4.5", body({ tools: [tool(longName)] }), false);
    expect(restoreToolName(out, out.tools[0].name)).toBe(longName);
  });

  it("points a forced tool_choice at the declared name, not the original", () => {
    const out = openaiToClaudeRequest("claude-sonnet-4.5", body({
      tools: [tool(longName)],
      tool_choice: { type: "function", function: { name: longName } },
    }), false);

    expect(out.tool_choice).toEqual({ type: "tool", name: out.tools[0].name });
  });

  it("leaves a short name and a string tool_choice exactly as sent", () => {
    const out = openaiToClaudeRequest("claude-sonnet-4.5", body({
      tools: [tool("get_weather")],
      tool_choice: "required",
    }), false);

    expect(out.tools[0].name).toBe("get_weather");
    expect(out.tool_choice).toEqual({ type: "any" });
  });

  it("keeps two prefix-sharing long tools as two distinct tools", () => {
    const shared = repeatTo("mcp__github__create_pull_request_review_", TOOL_NAME_MAX_LENGTH);
    const out = openaiToClaudeRequest("claude-sonnet-4.5", body({
      tools: [tool(`${shared}alpha`), tool(`${shared}beta`)],
    }), false);

    expect(out.tools[0].name).not.toBe(out.tools[1].name);
    expect(restoreToolName(out, out.tools[0].name)).toBe(`${shared}alpha`);
    expect(restoreToolName(out, out.tools[1].name)).toBe(`${shared}beta`);
  });
});
