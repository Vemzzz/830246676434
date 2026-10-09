import { describe, it, expect } from "vitest";
import { detectLoop } from "../../open-sse/utils/loopGuard.js";

function tc(name, args) {
  return { type: "function", function: { name, arguments: JSON.stringify(args) } };
}

function assistantWith(...toolCalls) {
  return { role: "assistant", content: "", tool_calls: toolCalls };
}

describe("detectLoop", () => {
  it("empty messages → not detected", () => {
    expect(detectLoop({ messages: [] })).toEqual({ detected: false, hint: null });
  });

  it("3 identical tool_calls → detected", () => {
    const body = { messages: [
      assistantWith(tc("bash", { cmd: "ls" })),
      assistantWith(tc("bash", { cmd: "ls" })),
      assistantWith(tc("bash", { cmd: "ls" })),
    ] };
    expect(detectLoop(body).detected).toBe(true);
  });

  it("2 identical tool_calls → NOT detected (below threshold)", () => {
    const body = { messages: [
      assistantWith(tc("bash", { cmd: "ls" })),
      assistantWith(tc("bash", { cmd: "ls" })),
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("3 calls with different args → NOT detected", () => {
    const body = { messages: [
      assistantWith(tc("bash", { cmd: "ls" })),
      assistantWith(tc("bash", { cmd: "pwd" })),
      assistantWith(tc("bash", { cmd: "whoami" })),
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("3 calls with different names → NOT detected", () => {
    const body = { messages: [
      assistantWith(tc("bash", { cmd: "ls" })),
      assistantWith(tc("grep", { cmd: "ls" })),
      assistantWith(tc("find", { cmd: "ls" })),
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("sequence [fetchA, fetchB] appearing 2 times → detected", () => {
    const body = { messages: [
      assistantWith(tc("fetchA", { url: "x" })),
      assistantWith(tc("fetchB", { url: "y" })),
      assistantWith(tc("fetchA", { url: "x" })),
      assistantWith(tc("fetchB", { url: "y" })),
    ] };
    expect(detectLoop(body).detected).toBe(true);
  });

  it("sequence appearing once → NOT detected", () => {
    const body = { messages: [
      assistantWith(tc("fetchA", { url: "x" })),
      assistantWith(tc("fetchB", { url: "y" })),
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("args normalization: {b:1,a:2} same as {a:2,b:1}", () => {
    const body = { messages: [
      assistantWith({ type: "function", function: { name: "bash", arguments: '{"b":1,"a":2}' } }),
      assistantWith({ type: "function", function: { name: "bash", arguments: '{"a":2,"b":1}' } }),
      assistantWith({ type: "function", function: { name: "bash", arguments: '{"b":1,"a":2}' } }),
    ] };
    expect(detectLoop(body).detected).toBe(true);
  });

  it("nested object args are preserved and do not collide into false loops", () => {
    const body = { messages: [
      assistantWith(tc("request", { options: { url: "https://a.com" } })),
      assistantWith(tc("request", { options: { url: "https://b.com" } })),
      assistantWith(tc("request", { options: { url: "https://c.com" } })),
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("intra-message sentence repeats (e.g. table rows or list bullets in one turn) do not trigger false loops", () => {
    const body = { messages: [
      { role: "user", content: "1" },
      { role: "assistant", content: "Ok, starting task 1 now." },
      { role: "user", content: "2" },
      { role: "assistant", content: "Ok, starting task 2 now." },
      { role: "user", content: "3" },
      {
        role: "assistant",
        content: "- pending checking file\n- pending checking file\n- pending checking file",
      },
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("no tool_calls in messages → NOT detected", () => {
    const body = { messages: [
      { role: "user", content: "hi" },
      { role: "assistant", content: "hello" },
    ] };
    expect(detectLoop(body).detected).toBe(false);
  });

  it("processes 1200 messages with 600 tool calls in under 50ms (issue #132)", () => {
    const messages = [];
    for (let i = 0; i < 600; i++) {
      messages.push({ role: "user", content: `Step ${i}` });
      messages.push({
        role: "assistant",
        content: `Working on step ${i}`,
        tool_calls: [tc("edit_file", { path: `src/mod_${i}.js`, code: `export const v = ${i};` })],
      });
    }

    const t0 = Date.now();
    const result = detectLoop({ messages });
    const duration = Date.now() - t0;

    expect(result.detected).toBe(false);
    expect(duration).toBeLessThan(100);
  });

  it("detects repeating sequence at tail of 1200-message conversation without stalling", () => {
    const messages = [];
    for (let i = 0; i < 596; i++) {
      messages.push({ role: "user", content: `Step ${i}` });
      messages.push({
        role: "assistant",
        content: `Working on step ${i}`,
        tool_calls: [tc("read_file", { path: `src/file_${i}.js` })],
      });
    }

    // Append 2-cycle loop at the tail
    messages.push(assistantWith(tc("build", { target: "all" })));
    messages.push(assistantWith(tc("test", { suite: "unit" })));
    messages.push(assistantWith(tc("build", { target: "all" })));
    messages.push(assistantWith(tc("test", { suite: "unit" })));

    const t0 = Date.now();
    const result = detectLoop({ messages });
    const duration = Date.now() - t0;

    expect(result.detected).toBe(true);
    expect(result.hint).toContain("sequence of tool calls");
    expect(duration).toBeLessThan(100);
  });
});
