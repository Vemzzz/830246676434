import { describe, expect, it } from "vitest";
import { PROVIDERS } from "../../open-sse/providers/index.js";
import { FORMATS } from "../../open-sse/translator/formats.js";
import { shouldDefaultClaudeToolType } from "../../open-sse/translator/concerns/toolCall.js";
import {
  anchorClaudeCache,
  normalizeClaudePassthrough,
  prepareClaudeRequest,
} from "../../open-sse/translator/formats/claude.js";
import { claudeToOpenAIRequest } from "../../open-sse/translator/request/claude-to-openai.js";

const text = (value, extra = {}) => ({ type: "text", text: value, ...extra });
const tool = (name, extra = {}) => ({ name, input_schema: { type: "object" }, ...extra });

function markerCount(body) {
  return [
    ...(body.system || []),
    ...(body.tools || []),
    ...(body.messages || []).flatMap((message) => message.content || []),
  ].filter((block) => block.cache_control).length;
}

describe("upstream Claude/DeepSeek tool and cache fixes", () => {
  it("defaults Claude tool type only for declaring gateways", () => {
    expect(shouldDefaultClaudeToolType("minimax", FORMATS.CLAUDE, [tool("x")], PROVIDERS)).toBe(true);
    expect(shouldDefaultClaudeToolType("minimax-cn", FORMATS.CLAUDE, [tool("x")], PROVIDERS)).toBe(true);
    expect(shouldDefaultClaudeToolType("deepseek", FORMATS.CLAUDE, [tool("x")], PROVIDERS)).toBe(false);
    expect(shouldDefaultClaudeToolType("minimax", FORMATS.OPENAI, [tool("x")], PROVIDERS)).toBe(false);
  });

  it("keeps DeepSeek web search types and drops custom types", () => {
    expect(PROVIDERS.deepseek.quirks.claudeSupportedToolTypes).toEqual([
      "web_search_20250305",
      "web_search_20260209",
    ]);
    const body = prepareClaudeRequest({
      model: "deepseek-v4-pro",
      max_tokens: 100,
      messages: [{ role: "user", content: "q" }],
      tools: [
        tool("Read", { type: "custom" }),
        tool("search", { type: "web_search_20250305" }),
      ],
    }, "deepseek");
    expect(body.tools).toHaveLength(1);
    expect(body.tools[0]).toMatchObject({ name: "search", type: "web_search_20250305" });
  });

  it("normalizes bare content objects and preserves them through Claude to OpenAI", () => {
    const body = { messages: [{ role: "assistant", content: text("answer") }] };
    normalizeClaudePassthrough(body);
    expect(body.messages[0].content).toEqual([text("answer")]);

    const out = claudeToOpenAIRequest("m", {
      messages: [{ role: "user", content: text("question") }],
    }, false);
    expect(out.messages).toContainEqual({ role: "user", content: "question" });
  });

  it("caps cache markers at four while retaining system and tool head anchors", () => {
    const body = anchorClaudeCache({
      system: [text("system", { cache_control: { type: "ephemeral" } })],
      tools: [tool("search", { cache_control: { type: "ephemeral" } })],
      messages: [
        { role: "user", content: [text("old", { cache_control: { type: "ephemeral" } })] },
        { role: "assistant", content: [text("reply", { cache_control: { type: "ephemeral" } })] },
        { role: "user", content: [text("new")] },
      ],
    });
    expect(markerCount(body)).toBeLessThanOrEqual(4);
    expect(body.system[0].cache_control.ttl).toBe("1h");
    expect(body.tools[0].cache_control.ttl).toBe("1h");
  });
});

it("does not alter existing thinking placeholder behavior for DeepSeek", () => {
  const body = prepareClaudeRequest({
    model: "deepseek-v4-pro",
    thinking: { type: "enabled" },
    messages: [
      { role: "user", content: [text("q")] },
      { role: "assistant", content: [{ type: "tool_use", id: "toolu_1", name: "Read", input: {} }] },
      { role: "user", content: [{ type: "tool_result", tool_use_id: "toolu_1", content: "ok" }] },
      { role: "user", content: [text("continue")] },
    ],
  }, "deepseek");
  expect(body.messages[1].content[0]).toEqual({ type: "thinking", thinking: "." });
});