import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const readSource = (relativePath) =>
  readFile(fileURLToPath(new URL(relativePath, import.meta.url)), "utf8");

describe("Codex settings refresh", () => {
  it("bypasses cached status after applying a selected endpoint", async () => {
    const [routeSource, cardSource, lifecycleSource] = await Promise.all([
      readSource("../../src/app/api/cli-tools/codex-settings/route.js"),
      readSource("../../src/app/(dashboard)/dashboard/cli-tools/components/CodexToolCard.js"),
      readSource("../../src/app/(dashboard)/dashboard/cli-tools/components/useCliToolLifecycle.js"),
    ]);

    // Route Handlers already run on the server; a Server Action directive would reject this export.
    expect(routeSource).not.toContain('"use server";');
    expect(routeSource).toContain('export const dynamic = "force-dynamic";');
    // The status refetch lives in the shared card lifecycle hook in this fork.
    expect(lifecycleSource).toContain('fetch(statusEndpoint, { cache: "no-store" })');
    // The card hydrates the base URL and bearer key from the active provider table.
    expect(cardSource).toContain("getCurrentCodexProviderSettings(codexStatus?.config).baseUrl");
    expect(cardSource).toContain("getCurrentCodexProviderSettings(status?.config).apiKey");
  });

  it("keeps an unmatched active URL in the custom endpoint slot", async () => {
    const selectorSource = await readSource("../../src/app/(dashboard)/dashboard/cli-tools/components/BaseUrlSelect.js");

    expect(selectorSource).toContain("if (current) {");
    expect(selectorSource).toContain("setCustomInput(current);");
    expect(selectorSource).toContain("onChange(current);");
  });
});
