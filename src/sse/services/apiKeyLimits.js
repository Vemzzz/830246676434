import { consumeApiKeyRequest } from "@/lib/localDb";
import { estimateInputTokens, normalizeIpAddress } from "@/lib/apiKeyLimitUtils";
import { getClientIp } from "@/lib/auth/loginLimiter";
import { errorResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";

export async function enforceApiKeyRequest(request, apiKeyInfo) {
  if (!apiKeyInfo) return null;

  if (Array.isArray(apiKeyInfo.ipWhitelist)) {
    const clientIp = normalizeIpAddress(getClientIp(request));
    if (!clientIp || !apiKeyInfo.ipWhitelist.includes(clientIp)) {
      return errorResponse(HTTP_STATUS.FORBIDDEN, "This API key cannot be used from your IP address");
    }
  }

  if (Number.isSafeInteger(apiKeyInfo.requestsPerMinute) && apiKeyInfo.requestsPerMinute > 0) {
    const result = await consumeApiKeyRequest(apiKeyInfo.id, apiKeyInfo.requestsPerMinute);
    if (!result.allowed) {
      return errorResponse(
        HTTP_STATUS.RATE_LIMITED,
        "API key requests-per-minute limit exceeded",
        { "Retry-After": String(result.retryAfterSeconds) }
      );
    }
  }
  return null;
}

export function enforceApiKeyInputLimit(apiKeyInfo, requestBody) {
  const limit = apiKeyInfo?.maxInputTokens;
  if (!Number.isSafeInteger(limit) || limit < 1) return null;

  const estimatedTokens = estimateInputTokens(requestBody);
  if (estimatedTokens > limit) {
    return errorResponse(
      HTTP_STATUS.BAD_REQUEST,
      `Estimated input size (${estimatedTokens} tokens) exceeds this API key's limit (${limit})`
    );
  }
  return null;
}

export function applyApiKeyOutputLimit(request, requestBody, apiKeyInfo) {
  const limit = apiKeyInfo?.maxOutputTokens;
  if (!Number.isSafeInteger(limit) || limit < 1) return;

  const fields = ["max_tokens", "max_completion_tokens", "max_output_tokens"];
  let applied = false;
  for (const field of fields) {
    if (field in requestBody) {
      const requested = Number(requestBody[field]);
      if (Number.isFinite(requested)) requestBody[field] = Math.min(requested, limit);
      applied = true;
    }
  }

  if (requestBody.generationConfig && typeof requestBody.generationConfig === "object") {
    const generationConfig = requestBody.generationConfig;
    const requested = Number(generationConfig.maxOutputTokens);
    generationConfig.maxOutputTokens = Number.isFinite(requested)
      ? Math.min(requested, limit)
      : limit;
    applied = true;
  }

  if (!applied) {
    const pathname = new URL(request.url).pathname;
    if (pathname.includes("/responses") || (requestBody.input !== undefined && requestBody.messages === undefined)) {
      requestBody.max_output_tokens = limit;
    } else {
      requestBody.max_tokens = limit;
    }
  }
}
