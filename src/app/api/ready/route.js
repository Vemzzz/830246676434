import { getAdapter } from "@/lib/db/driver";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

export const dynamic = "force-dynamic";

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }

  try {
    const db = await getAdapter();
    db.get("SELECT 1 AS ok");
    return Response.json(
      { ok: true, database: "ready" },
      { headers: { "Cache-Control": "no-store" } },
    );
  } catch (error) {
    console.error("[ready] database check failed:", error.message);
    return Response.json(
      { ok: false, database: "unavailable" },
      { status: 503, headers: { "Cache-Control": "no-store" } },
    );
  }
}
