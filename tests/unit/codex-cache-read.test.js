import { describe, it, expect } from "vitest";
import { resolveSessionId } from "../../open-sse/utils/sessionManager.js";
import { extractUsageFromResponse } from "../../open-sse/handlers/chatCore/requestDetail.js";
import { convertResponsesStreamToJson } from "../../open-sse/transformer/streamToJsonConverter.js";

describe("Codex Prompt Caching & Cache Read Extraction", () => {
  it("generates stable session ID across multi-turn tool calling turns", () => {
    const turn1Body = {
      messages: [
        { role: "user", content: "Check auth logic in auth.js" },
        {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "call_1",
              type: "function",
              function: { name: "grep", arguments: JSON.stringify({ pattern: "isAuthorized", path: "src/auth.js" }) }
            }
          ]
        },
        { role: "tool", tool_call_id: "call_1", content: "function isAuthorized() { return true; }" },
      ]
    };

    const turn2Body = {
      messages: [
        ...turn1Body.messages,
        { role: "user", content: "Now check permissions.js" }
      ]
    };

    const session1 = resolveSessionId({ body: turn1Body, connectionId: "conn_1", scope: "codex" });
    const session2 = resolveSessionId({ body: turn2Body, connectionId: "conn_1", scope: "codex" });

    expect(session1).toBeTruthy();
    expect(session2).toBe(session1);
  });

  it("extracts cached_tokens from OpenAI Responses API usage format in requestDetail", () => {
    const responsesUsage = {
      input_tokens: 2048,
      output_tokens: 128,
      input_tokens_details: {
        cached_tokens: 1536
      }
    };

    const extracted = extractUsageFromResponse({ usage: responsesUsage });
    expect(extracted).toBeTruthy();
    expect(extracted.prompt_tokens).toBe(2048);
    expect(extracted.completion_tokens).toBe(128);
    expect(extracted.cached_tokens).toBe(1536);
  });

  it("preserves input_tokens_details in convertResponsesStreamToJson", async () => {
    const sseEvent = [
      'event: response.created\n',
      'data: {"type":"response.created","response":{"id":"resp_123","created_at":1700000000}}\n\n',
      'event: response.completed\n',
      'data: {"type":"response.completed","response":{"id":"resp_123","status":"completed","usage":{"input_tokens":3000,"output_tokens":250,"input_tokens_details":{"cached_tokens":2500}}}}\n\n'
    ].join("");

    const encoder = new TextEncoder();
    const stream = new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode(sseEvent));
        controller.close();
      }
    });

    const json = await convertResponsesStreamToJson(stream);
    expect(json.status).toBe("completed");
    expect(json.usage.input_tokens).toBe(3000);
    expect(json.usage.output_tokens).toBe(250);
    expect(json.usage.input_tokens_details).toEqual({ cached_tokens: 2500 });
  });
});
