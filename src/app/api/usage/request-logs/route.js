import { NextResponse } from "next/server";
import { getRecentLogs } from "@/lib/usageDb";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }
  try {
    const logs = await getRecentLogs(200);
    return NextResponse.json(logs);
  } catch (error) {
    console.error("[API ERROR] /api/usage/logs failed:", error);
    console.error("[API ERROR] Stack:", error?.stack);
    return NextResponse.json({ error: "Failed to fetch logs" }, { status: 500 });
  }
}
