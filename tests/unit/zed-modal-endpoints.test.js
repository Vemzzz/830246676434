import { describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const readSource = (relativePath) =>
  readFileSync(fileURLToPath(new URL(relativePath, import.meta.url)), "utf8");

/**
 * ZedAuthModal shipped a browser/paste OAuth flow wired to
 * /api/oauth/zed/{start-proxy,authorize,register-session,poll-status,exchange,
 * stop-proxy}. None of those routes exist — the generic
 * /api/oauth/[provider]/[action] handler only serves codex and xai — so every
 * button except "Import from Zed IDE" failed at runtime.
 *
 * This guards the general rule: a component may only call /api/oauth/zed/<name>
 * paths that are actually routed on disk.
 */
describe("ZedAuthModal only calls zed oauth endpoints that exist", () => {
  const modalPath = "../../src/shared/components/ZedAuthModal.js";

  it("every referenced /api/oauth/zed/<name> path has a route.js on disk", () => {
    const source = readSource(modalPath);
    const referenced = [...source.matchAll(/\/api\/oauth\/zed\/([a-z0-9-]+)/g)].map((m) => m[1]);
    expect(referenced.length).toBeGreaterThan(0);

    for (const name of new Set(referenced)) {
      const route = `../../src/app/api/oauth/zed/${name}/route.js`;
      expect(
        existsSync(fileURLToPath(new URL(route, import.meta.url))),
        `/api/oauth/zed/${name} is called by ZedAuthModal but has no route`,
      ).toBe(true);
    }
  });

  it("keeps the import path it can actually serve", () => {
    const source = readSource(modalPath);
    expect(source).toContain("/api/oauth/zed/auto-import");
    expect(source).toContain("/api/oauth/zed/import");
  });
});
