/**
 * Regression tests for Issue #150:
 * "Claude code CBAI (Codebuddy) Provide Error"
 *
 * Upstream error:
 *   API Error: 400 [400]: {"code":11128,"msg":"Illegal API invocation from an unapproved channel",...}
 *
 * Root Cause:
 *   Tencent CodeBuddy (both codebuddy.ai and copilot.tencent.com) inspects chat body
 *   system and assistant messages for competitor CLI identities (especially Claude Code:
 *   "You are Claude Code, Anthropic's official CLI for Claude...").
 *   When detected, upstream WAF blocks the request with code 11128.
 *
 * Fix:
 *   Neutralize competitor identity markers in system/assistant messages before sending,
 *   compact oversized tools (>64KB), harvest rotated tokens, and parse code 11128 as a
 *   content-filter error rather than an account failure.
 */

import { describe, expect, it } from "vitest";
import { CodeBuddyIntlExecutor } from "../../open-sse/executors/codebuddy-intl.js";
import { CodeBuddyExecutor } from "../../open-sse/executors/codebuddy-cn.js";
import {
  neutralizeCodeBuddyChannelIdentity,
  compactOversizedTools,
  captureRotatedToken,
  parseCodeBuddyError
} from "../../open-sse/executors/codebuddyShared.js";
import { checkFallbackError } from "../../open-sse/services/accountFallback.js";

const CLAUDE_CODE_SYSTEM =
  "You are Claude Code, Anthropic's official CLI for Claude. You are an interactive CLI tool that helps users with software engineering tasks.";

describe("Issue #150: CodeBuddy Channel Identity & 11128 WAF Neutralization", () => {
  const intlExecutor = new CodeBuddyIntlExecutor();
  const cnExecutor = new CodeBuddyExecutor();

  it("neutralizes Claude Code system prompt in CodeBuddyIntlExecutor (CBAI)", () => {
    const body = {
      messages: [
        { role: "system", content: CLAUDE_CODE_SYSTEM },
        { role: "user", content: "Implement a feature" }
      ]
    };

    const out = intlExecutor.transformRequest("glm-5.2", body, false, {});

    // Must not contain the forbidden competitor identity phrase
    const serialized = JSON.stringify(out.messages);
    expect(serialized).not.toContain("You are Claude Code, Anthropic's official CLI for Claude");
    expect(serialized).toContain("You are CodeBuddy Code");

    // User message must remain intact
    const userMsg = out.messages.find(m => m.role === "user");
    expect(userMsg.content).toEqual([{ type: "text", text: "Implement a feature" }]);
  });

  it("neutralizes Claude Code system prompt in CodeBuddyExecutor (CN)", () => {
    const body = {
      messages: [
        { role: "system", content: CLAUDE_CODE_SYSTEM },
        { role: "user", content: "Fix bug" }
      ]
    };

    const out = cnExecutor.transformRequest("glm-5.2", body, false, {});

    const serialized = JSON.stringify(out.messages);
    expect(serialized).not.toContain("You are Claude Code, Anthropic's official CLI for Claude");
    expect(serialized).toContain("You are CodeBuddy Code");
    expect(out.messages.find(m => m.role === "user").content).toBe("Fix bug");
  });

  it("neutralizes Claude Code identity when present in assistant turn", () => {
    const messages = [
      { role: "user", content: "Who are you?" },
      { role: "assistant", content: "Sure! " + CLAUDE_CODE_SYSTEM },
      { role: "user", content: "Next task" }
    ];

    const sanitized = neutralizeCodeBuddyChannelIdentity(messages);
    expect(sanitized[1].content).not.toContain("You are Claude Code, Anthropic's official CLI for Claude");
    expect(sanitized[1].content).toContain("You are CodeBuddy Code");
    expect(sanitized[0].content).toBe("Who are you?");
    expect(sanitized[2].content).toBe("Next task");
  });

  it("neutralizes structured block content ({ type: 'text', text: '...' }) in system messages", () => {
    const messages = [
      {
        role: "system",
        content: [{ type: "text", text: CLAUDE_CODE_SYSTEM }]
      },
      { role: "user", content: [{ type: "text", text: "Test" }] }
    ];

    const sanitized = neutralizeCodeBuddyChannelIdentity(messages);
    expect(sanitized[0].content[0].text).not.toContain("You are Claude Code, Anthropic's official CLI for Claude");
    expect(sanitized[0].content[0].text).toContain("You are CodeBuddy Code");
    expect(sanitized[1].content[0].text).toBe("Test");
  });

  it("sanitizes ZCode and competitor harness headers", () => {
    const messages = [
      {
        role: "system",
        content: "You are ZCode, an advanced developer agent.\nMain branch (you will usually use this for PRs): main\nx-anthropic-billing-header: test\n# Claude Code Desktop Context\nInstructions here."
      }
    ];

    const sanitized = neutralizeCodeBuddyChannelIdentity(messages);
    const content = sanitized[0].content;
    expect(content).not.toContain("You are ZCode");
    expect(content).not.toContain("x-anthropic-billing-header");
    expect(content).not.toContain("(you will usually use this for PRs)");
    expect(content).toContain("Main branch: main");
    expect(content).toContain("# Desktop Context");
    expect(content).toContain("Instructions here.");
  });

  it("compacts oversized tools (>64KB) by stripping redundant descriptions", () => {
    const longDesc = "A".repeat(1000);
    const tools = Array.from({ length: 70 }, (_, i) => ({
      type: "function",
      function: {
        name: `tool_${i}`,
        description: longDesc,
        parameters: { type: "object" }
      }
    }));

    const rawSize = new TextEncoder().encode(JSON.stringify(tools)).byteLength;
    expect(rawSize).toBeGreaterThan(65536);

    const compacted = compactOversizedTools(tools);
    const compactedSize = new TextEncoder().encode(JSON.stringify(compacted)).byteLength;
    expect(compactedSize).toBeLessThan(65536);
    expect(compacted[0].function.name).toBe("tool_0");
    expect(compacted[0].function.description).toBeUndefined();
  });

  it("parses code 11128 as a content policy refusal", () => {
    const rawError = JSON.stringify({
      code: 11128,
      msg: "Illegal API invocation from an unapproved channel",
      requestId: "127e2359-df1f-4d41-992e-22a5d218ab94",
      displayMsg: { en: "The request was blocked by security policy." }
    });

    const parsed = parseCodeBuddyError({ status: 400 }, rawError);
    expect(parsed).toEqual({
      status: 400,
      message: "Illegal API invocation from an unapproved channel",
      isContentFilter: true
    });

    // Verify errorConfig classifies it with isContentFilter: true
    const fallback = checkFallbackError(400, parsed.message);
    expect(fallback.isContentFilter).toBe(true);
    expect(fallback.shouldFallback).toBe(false);
  });

  it("harvests rotated bearer tokens from response headers", () => {
    const credentials = { accessToken: "old-token" };
    const response = new Response(JSON.stringify({ ok: true }), {
      headers: { Authorization: "Bearer new-token-456" }
    });

    captureRotatedToken(response, credentials);
    expect(credentials.accessToken).toBe("new-token-456");
  });
});
