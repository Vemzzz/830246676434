import { describe, expect, it } from "vitest";
import { hasTrustedPeerHeaders } from "../../src/lib/auth/trustedPeer.js";

describe("trusted peer headers", () => {
  it("rejects client-supplied peer headers without the process token", () => {
    const previous = process.env.NINEROUTER_PEER_TOKEN;
    process.env.NINEROUTER_PEER_TOKEN = "server-token";
    try {
      const request = (token) => ({ headers: new Headers({ "x-9r-peer-token": token }) });
      expect(hasTrustedPeerHeaders(request("client-token"))).toBe(false);
      expect(hasTrustedPeerHeaders(request("server-token"))).toBe(true);
    } finally {
      if (previous === undefined) delete process.env.NINEROUTER_PEER_TOKEN;
      else process.env.NINEROUTER_PEER_TOKEN = previous;
    }
  });
});
