import { describe, it, expect } from "vitest";
import { testOAuthConnection } from "../../src/app/api/providers/[id]/test/testUtils.js";

describe("testOAuthConnection - cursor", () => {
  function makeJwt(payload) {
    const header = Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url");
    const body = Buffer.from(JSON.stringify(payload)).toString("base64url");
    return `${header}.${body}.signature`;
  }

  it("returns invalid with error message when Cursor token is an expired JWT (seconds)", async () => {
    const expiredExp = Math.floor(Date.now() / 1000) - 3600; // 1 hour ago
    const token = makeJwt({ sub: "user-123", exp: expiredExp });
    const connection = { provider: "cursor", accessToken: token };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: false,
      error: "Cursor token expired. Please re-import token from Cursor IDE.",
    });
  });

  it("returns invalid with error message when Cursor token is an expired JWT (milliseconds)", async () => {
    const expiredExp = Date.now() - 3600000; // 1 hour ago in ms
    const token = makeJwt({ sub: "user-123", exp: expiredExp });
    const connection = { provider: "cursor", accessToken: token };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: false,
      error: "Cursor token expired. Please re-import token from Cursor IDE.",
    });
  });

  it("returns valid when Cursor token is a valid unexpired JWT", async () => {
    const futureExp = Math.floor(Date.now() / 1000) + 3600; // 1 hour ahead
    const token = makeJwt({ sub: "user-123", exp: futureExp });
    const connection = { provider: "cursor", accessToken: token };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: true,
      error: null,
      refreshed: false,
      newTokens: null,
    });
  });

  it("returns valid when Cursor token is a JWT without exp claim", async () => {
    const token = makeJwt({ sub: "user-123" });
    const connection = { provider: "cursor", accessToken: token };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: true,
      error: null,
      refreshed: false,
      newTokens: null,
    });
  });

  it("returns valid when Cursor token is not a JWT", async () => {
    const connection = { provider: "cursor", accessToken: "opaque-cursor-token-without-jwt-format" };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: true,
      error: null,
      refreshed: false,
      newTokens: null,
    });
  });

  it("returns error when connection has no accessToken", async () => {
    const connection = { provider: "cursor", accessToken: "" };

    const result = await testOAuthConnection(connection);
    expect(result).toEqual({
      valid: false,
      error: "No access token",
      refreshed: false,
    });
  });
});
