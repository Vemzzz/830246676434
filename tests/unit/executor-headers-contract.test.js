import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const readSource = (relativePath) =>
  readFileSync(fileURLToPath(new URL(relativePath, import.meta.url)), "utf8");
const base = readSource("../../open-sse/executors/base.js");
const defaultExecutor = readSource("../../open-sse/executors/default.js");
const codexExecutor = readSource("../../open-sse/executors/codex.js");

/**
 * base.execute() passes (credentials, stream, url, model, body) into buildHeaders,
 * so subclasses reading slot 4 (model id) or slot 5 (request body) rely on that
 * overload. The base declaration only names the two slots it uses itself, and this
 * test keeps the producer and the consumers in step.
 */
describe("executor buildHeaders contract", () => {
  it("base passes url, model and body through on the execute path", () => {
    expect(base).toContain(
      "this.buildHeaders(credentials, stream, url, model, transformedBody)",
    );
  });

  it("the default executor consumes the model in slot 4 and the body in slot 5", () => {
    expect(defaultExecutor).toMatch(
      /buildHeaders\(credentials, stream = true, url, model, body = null\)/,
    );
  });

  it("the codex executor consumes the model in slot 4 and the body in slot 5", () => {
    expect(codexExecutor).toMatch(
      /buildHeaders\(credentials, stream = true, _url = null, model = null, body = null\)/,
    );
  });
});
