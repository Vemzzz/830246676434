import { resetComboRotation } from "open-sse/services/combo.js";
import { invalidateAllowedModelsCache } from "@/sse/services/allowedModels.js";

const HOP_BY_HOP_HEADERS = new Set([
  "connection",
  "content-length",
  "host",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

async function proxyRustRequest(request, invalidateAdminState, upstreamPath) {
  const baseUrl = process.env.NEXT_PUBLIC_RUST_API_URL?.replace(/\/+$/, "");
  if (!baseUrl) {
    return Response.json(
      { error: "Rust API URL is not configured" },
      { status: 503 }
    );
  }

  const incomingUrl = new URL(request.url);
  const isComboMutation = invalidateAdminState && (
    incomingUrl.pathname === "/api/combos"
    || /^\/api\/combos\/[^/]+$/.test(incomingUrl.pathname)
  ) && ["POST", "PUT", "DELETE"].includes(request.method);
  const isModelConfigurationMutation = invalidateAdminState && [
    "/api/models",
    "/api/models/alias",
    "/api/models/custom",
    "/api/models/disabled",
  ].includes(incomingUrl.pathname) && ["POST", "PUT", "DELETE"].includes(request.method);
  const headers = new Headers();
  for (const [name, value] of request.headers) {
    if (!HOP_BY_HOP_HEADERS.has(name.toLowerCase())) headers.set(name, value);
  }

  let upstream;
  try {
    upstream = await fetch(`${baseUrl}${upstreamPath || incomingUrl.pathname}${incomingUrl.search}`, {
      method: request.method,
      headers,
      ...(request.method === "GET" || request.method === "HEAD"
        ? {}
        : { body: await request.arrayBuffer() }),
      cache: "no-store",
      redirect: "manual",
      signal: AbortSignal.timeout(60_000),
    });
  } catch (error) {
    console.error("[rust-backend-proxy] upstream request failed:", error);
    return Response.json(
      { error: "Rust backend is unavailable" },
      { status: 502 }
    );
  }

  const responseHeaders = new Headers();
  if (isComboMutation && upstream.ok) {
    resetComboRotation();
    invalidateAllowedModelsCache();
  }
  if (isModelConfigurationMutation && upstream.ok) {
    invalidateAllowedModelsCache();
  }
  for (const name of [
    "content-type",
    "cache-control",
    "retry-after",
    "location",
    "access-control-allow-origin",
    "access-control-allow-methods",
    "access-control-allow-headers",
    "access-control-expose-headers",
    "access-control-max-age",
  ]) {
    const value = upstream.headers.get(name);
    if (value) responseHeaders.set(name, value);
  }
  const setCookies = upstream.headers.getSetCookie?.();
  if (setCookies?.length) {
    for (const cookie of setCookies) responseHeaders.append("set-cookie", cookie);
  } else {
    const setCookie = upstream.headers.get("set-cookie");
    if (setCookie) responseHeaders.set("set-cookie", setCookie);
  }
  return new Response(upstream.body, {
    status: upstream.status,
    headers: responseHeaders,
  });
}

export async function proxyRustAdminRequest(request) {
  return proxyRustRequest(request, true);
}

export async function proxyRustAuthRequest(request) {
  return proxyRustRequest(request, false);
}

export async function proxyRustChatRequest(request) {
  return proxyRustRequest(request, false, "/v1/chat/completions");
}

export async function proxyRustModelsRequest(request) {
  return proxyRustRequest(request, false, "/v1/models");
}

export async function proxyRustModelInfoRequest(request) {
  return proxyRustRequest(request, false, "/v1/models/info");
}

export function isRustBackendEnabled() {
  return process.env.RUST_BE === "true";
}

export function isRustChatModelSupported(model) {
  return typeof model === "string"
    && (model.startsWith("openai/") || model.startsWith("combo/"));
}
