import { NextResponse } from "next/server";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

const CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, OPTIONS",
  "Access-Control-Allow-Headers": "*",
};

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }
  return NextResponse.json({ ok: true }, { headers: CORS_HEADERS });
}

export async function OPTIONS() {
  return new NextResponse(null, { status: 204, headers: CORS_HEADERS });
}
