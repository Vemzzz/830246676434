// This API route is called automatically to initialize app
import { isRustBackendEnabled, proxyRustAdminRequest } from "@/lib/rustBackendProxy";

export async function GET(request) {
  if (isRustBackendEnabled()) {
    return proxyRustAdminRequest(request);
  }
  return new Response("Initialized", { status: 200 });
}
