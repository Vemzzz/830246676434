/**
 * Issue #148: All providers and custom OpenAI-compatible nodes must cap tool names
 * at 64 characters so that upstream providers (such as kiosapi.com, OneAPI,
 * OpenCode Responses API) do not reject requests with:
 *
 *   Error from provider (Console): `name` must be at most 64 characters, got 68
 *
 * The original name must be transparently restored in response streams and objects.
 */

import { describe, it, expect } from "vitest";
import { translateRequest, translateResponse } from "../../open-sse/translator/index.js";
import { FORMATS } from "../../open-sse/translator/formats.js";
import { TOOL_NAME_MAX_LENGTH, restoreToolName } from "../../open-sse/translator/concerns/toolCall.js";

const repeatTo = (seed, len) => (seed + "x".repeat(len)).slice(0, len);
const longName = repeatTo("mcp__github__create_pull_request_review_comments_for_commit_", 68);

const makeOpenAIBody = (extra = {}) => ({
  messages: [{ role: "user", content: "list prs" }],
  tools: [{
    type: "function",
    function: {
      name: longName,
      description: "Creates pull request review comments",
      parameters: { type: "object", properties: { body: { type: "string" } } }
    }
  }],
  tool_choice: { type: "function", function: { name: longName } },
  ...extra
});

describe("Issue #148 — Universal tool name fitting across all provider formats", () => {
  it("fits tool name to <= 64 chars in OpenAI -> OpenAI format (custom nodes like kiosapi.com)", () => {
    expect(longName.length).toBe(68);
    const body = makeOpenAIBody();
    const translated = translateRequest(FORMATS.OPENAI, FORMATS.OPENAI, "gpt-4o", body, true);

    const fittedName = translated.tools[0].function.name;
    expect(fittedName.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
    expect(fittedName).not.toBe(longName);
    expect(translated.tool_choice.function.name).toBe(fittedName);

    // Verify reverse mapping attached to request
    expect(translated._toolNameMap).toBeDefined();
    expect(translated._toolNameMap.get(fittedName)).toBe(longName);

    // Verify response restoration
    const streamChunk = {
      choices: [{
        delta: {
          tool_calls: [{
            index: 0,
            function: { name: fittedName, arguments: '{"body":"ok"}' }
          }]
        }
      }]
    };
    const [restored] = translateResponse(FORMATS.OPENAI, FORMATS.OPENAI, streamChunk, {
      toolNameMap: translated._toolNameMap
    });
    expect(restored.choices[0].delta.tool_calls[0].function.name).toBe(longName);
  });

  it("fits tool name to <= 64 chars in OpenAI -> OpenAI Responses format (muse-spark-1.3-contributor)", () => {
    const body = makeOpenAIBody();
    const translated = translateRequest(FORMATS.OPENAI, FORMATS.OPENAI_RESPONSES, "muse-spark-1.3-contributor", body, true);

    const fittedName = translated.tools[0].name;
    expect(fittedName.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
    expect(fittedName).not.toBe(longName);

    // Verify reverse mapping attached to request
    expect(translated._toolNameMap).toBeDefined();
    expect(translated._toolNameMap.get(fittedName)).toBe(longName);

    // Verify response restoration
    const responsesChunk = {
      type: "response.output_item.added",
      item: {
        type: "function_call",
        name: fittedName,
        call_id: "call_123"
      }
    };
    const [restored] = translateResponse(FORMATS.OPENAI_RESPONSES, FORMATS.OPENAI, responsesChunk, {
      toolNameMap: translated._toolNameMap
    });
    expect(restored.choices[0].delta.tool_calls[0].function.name).toBe(longName);
  });

  it("fits tool name in assistant message history", () => {
    const body = makeOpenAIBody({
      messages: [
        { role: "user", content: "list prs" },
        {
          role: "assistant",
          tool_calls: [{
            id: "call_123",
            type: "function",
            function: { name: longName, arguments: "{}" }
          }]
        },
        { role: "tool", tool_call_id: "call_123", content: "[]" },
        { role: "user", content: "summarize" }
      ]
    });

    const translated = translateRequest(FORMATS.OPENAI, FORMATS.OPENAI, "gpt-4o", body, true);
    const fittedName = translated.tools[0].function.name;

    expect(translated.messages[1].tool_calls[0].function.name).toBe(fittedName);
    expect(translated.messages[1].tool_calls[0].function.name.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
  });

  it("keeps prefix-sharing tools distinct across calls", () => {
    const sharedPrefix = repeatTo("mcp__github__create_pull_request_review_comments_for_commit_", 64);
    const toolA = `${sharedPrefix}_a`;
    const toolB = `${sharedPrefix}_b`;

    const body = {
      messages: [{ role: "user", content: "hi" }],
      tools: [
        { type: "function", function: { name: toolA, parameters: { type: "object" } } },
        { type: "function", function: { name: toolB, parameters: { type: "object" } } }
      ]
    };

    const translated = translateRequest(FORMATS.OPENAI, FORMATS.OPENAI, "gpt-4o", body, true);
    const nameA = translated.tools[0].function.name;
    const nameB = translated.tools[1].function.name;

    expect(nameA).not.toBe(nameB);
    expect(nameA.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);
    expect(nameB.length).toBeLessThanOrEqual(TOOL_NAME_MAX_LENGTH);

    expect(translated._toolNameMap.get(nameA)).toBe(toolA);
    expect(translated._toolNameMap.get(nameB)).toBe(toolB);
  });
});
