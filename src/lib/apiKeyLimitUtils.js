import { isIP } from "node:net";

const LIMIT_FIELDS = ["maxInputTokens", "maxOutputTokens", "requestsPerMinute"];

function normalizeLimit(value, field) {
  if (value === null || value === undefined || value === "") return { value: null };
  if (typeof value !== "number" && typeof value !== "string") {
    return { error: `${field} must be a positive whole number or left blank` };
  }
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    return { error: `${field} must be a positive whole number or left blank` };
  }
  return { value: parsed };
}

export function normalizeIpAddress(value) {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  const normalized = trimmed.toLowerCase().startsWith("::ffff:")
    ? trimmed.slice(7)
    : trimmed;
  if (!isIP(normalized)) return null;
  if (isIP(normalized) === 6) {
    return new URL(`http://[${normalized}]/`).hostname.slice(1, -1).toLowerCase();
  }
  return normalized.toLowerCase();
}

export function normalizeApiKeyLimits(body, { partial = false } = {}) {
  const limits = {};
  for (const field of LIMIT_FIELDS) {
    if (partial && !(field in body)) continue;
    const result = normalizeLimit(body[field], field);
    if (result.error) return result;
    limits[field] = result.value;
  }

  if (partial && !("ipWhitelist" in body)) return { value: limits };
  const rawIps = body.ipWhitelist;
  if (rawIps === null || rawIps === undefined || rawIps === "") {
    limits.ipWhitelist = null;
    return { value: limits };
  }
  const candidates = Array.isArray(rawIps)
    ? rawIps
    : typeof rawIps === "string"
      ? rawIps.split(/[,\n]/)
      : null;
  if (!candidates) return { error: "ipWhitelist must be a list of IP addresses" };

  const ips = [];
  for (const candidate of candidates) {
    if (typeof candidate !== "string" || !candidate.trim()) continue;
    const ip = normalizeIpAddress(candidate);
    if (!ip) return { error: `Invalid IP address: ${candidate.trim()}` };
    if (!ips.includes(ip)) ips.push(ip);
  }
  limits.ipWhitelist = ips.length ? ips : null;
  return { value: limits };
}

export function estimateInputTokens(requestBody) {
  const serialized = JSON.stringify(requestBody);
  return Math.ceil(new TextEncoder().encode(serialized).byteLength / 4);
}
