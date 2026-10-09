import { NextResponse } from "next/server";
import { cookies } from "next/headers";
import { clearDashboardAuthCookie } from "@/lib/auth/dashboardSession";
import { isRustBackendEnabled, proxyRustAuthRequest } from "@/lib/rustBackendProxy";

export async function POST(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAuthRequest(request);
  }
  const cookieStore = await cookies();
  clearDashboardAuthCookie(cookieStore);
  cookieStore.delete("oidc_state");
  cookieStore.delete("oidc_nonce");
  cookieStore.delete("oidc_code_verifier");
  return NextResponse.json({ success: true }, { headers: { "Cache-Control": "no-store" } });
}
