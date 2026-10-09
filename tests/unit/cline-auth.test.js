import { describe, it, expect } from "vitest";
import {
  getClineAccessToken,
  getClineAuthorizationHeader,
} from "../../open-sse/shared/clineAuth.js";

describe("Cline auth tokens", () => {
  it("prefixes a bare WorkOS JWT", () => {
    const token = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJjbGluZSJ9.signature";
    expect(getClineAccessToken(token)).toBe(`workos:${token}`);
  });

  it("preserves existing WorkOS prefixes without changing case", () => {
    const token = "WORKOS:eyJhbGciOiJSUzI1NiJ9.payload.signature";
    expect(getClineAccessToken(token)).toBe(token);
  });

  it("preserves opaque ClinePass API keys verbatim", () => {
    expect(getClineAccessToken("clp_1234567890")).toBe("clp_1234567890");
    expect(getClineAuthorizationHeader("clp_1234567890")).toBe("Bearer clp_1234567890");
  });

  it("trims tokens and rejects empty values", () => {
    expect(getClineAccessToken("  clp_abc  ")).toBe("clp_abc");
    expect(getClineAccessToken("   ")).toBe("");
    expect(getClineAccessToken(null)).toBe("");
  });
});
