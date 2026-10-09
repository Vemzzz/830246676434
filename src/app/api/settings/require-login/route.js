import { NextResponse } from "next/server";
import { getSettings } from "@/lib/localDb";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }

  try {
    const settings = await getSettings();
    const requireLogin = settings.requireLogin !== false;
    const tunnelDashboardAccess = settings.tunnelDashboardAccess !== false;
    const tunnelUrl = settings.tunnelUrl || "";
    const tailscaleUrl = settings.tailscaleUrl || "";
    return NextResponse.json({ requireLogin, tunnelDashboardAccess, tunnelUrl, tailscaleUrl });
  } catch (error) {
    return NextResponse.json({ requireLogin: true }, { status: 200 });
  }
}
