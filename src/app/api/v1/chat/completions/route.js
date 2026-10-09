import { handleChat } from "@/sse/handlers/chat.js";
import { initTranslators } from "open-sse/translator/index.js";
import {
  isRustBackendEnabled,
  isRustChatModelSupported,
  proxyRustChatRequest,
} from "@/lib/rustBackendProxy";

let initialized = false;

/**
 * Initialize translators once
 */
async function ensureInitialized() {
  if (!initialized) {
    await initTranslators();
    initialized = true;
  }
}

/**
 * Handle CORS preflight
 */
export async function OPTIONS() {
  return new Response(null, {
    headers: {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
      "Access-Control-Allow-Headers": "*"
    }
  });
}

export async function POST(request) {
  let payload;
  if (isRustBackendEnabled()) {
    try {
      payload = await request.clone().json();
    } catch {
      // Keep malformed requests on the existing handler for its established error response.
    }
  }
  if (isRustBackendEnabled() && isRustChatModelSupported(payload?.model)) {
    const response = await proxyRustChatRequest(request);
    if (response.status !== 403) return response;
    const error = await response.clone().json().catch(() => null);
    if (
      error?.error?.message !==
      "Rust chat IP-whitelisted keys require a trusted client-IP proxy configuration"
    ) {
      return response;
    }
  }
  // Fallback to local handling
  await ensureInitialized();
  
  return await handleChat(request);
}
