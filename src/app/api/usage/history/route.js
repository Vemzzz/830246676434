import { NextResponse } from "next/server";
import { getUsageHistory } from "@/lib/usageDb";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }

  try {
    const { searchParams } = new URL(request.url);
    const filter = {
      provider: searchParams.get("provider") || undefined,
      model: searchParams.get("model") || undefined,
      startDate: searchParams.get("startDate") || undefined,
      endDate: searchParams.get("endDate") || undefined,
    };
    const history = await getUsageHistory(filter);
    return NextResponse.json(history);
  } catch (error) {
    console.error("Error fetching usage history:", error);
    return NextResponse.json({ error: "Failed to fetch usage history" }, { status: 500 });
  }
}
