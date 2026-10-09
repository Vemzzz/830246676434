import { NextResponse } from "next/server";
import { jwtVerify } from "jose";
import { getSettings } from "@/lib/localDb";
import { isRustBackendEnabled, proxyRustAuthRequest } from "@/lib/rustBackendProxy";

function getSecret() {
  const secret = process.env.JWT_SECRET;
  if (!secret) {
    throw new Error("JWT_SECRET environment variable is required");
  }
  return new TextEncoder().encode(secret);
}

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAuthRequest(request);
  }
  try {
    const settings = await getSettings();

    if (settings && settings.requireLogin === false) {
      return NextResponse.json({ authenticated: true });
    }

    const { cookies } = await import("next/headers");
    const cookieStore = await cookies();
    const token = cookieStore.get("auth_token")?.value;

    if (!token) {
      return NextResponse.json({ authenticated: false }, { status: 401 });
    }

    try {
      await jwtVerify(token, getSecret());
      return NextResponse.json({ authenticated: true });
    } catch {
      return NextResponse.json({ authenticated: false }, { status: 401 });
    }
  } catch {
    return NextResponse.json({ authenticated: false }, { status: 401 });
  }
}
