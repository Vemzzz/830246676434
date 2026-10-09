import { ollamaModels } from "open-sse/config/ollamaModels.js";
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

const CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, OPTIONS",
  "Access-Control-Allow-Headers": "*"
};

export async function OPTIONS(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }
  return new Response(null, { headers: CORS_HEADERS });
}

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }
  return new Response(JSON.stringify(ollamaModels), {
    headers: { "Content-Type": "application/json", ...CORS_HEADERS }
  });
}
