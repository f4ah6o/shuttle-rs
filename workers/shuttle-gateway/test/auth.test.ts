import { beforeEach, describe, expect, it, vi } from "vitest";

const joseMocks = vi.hoisted(() => ({
  createRemoteJWKSet: vi.fn(() => ({})),
  jwtVerify: vi.fn(),
}));

vi.mock("jose", () => joseMocks);

import type { Env } from "../src/env.js";
import { handle } from "../src/index.js";
import { makeRequest, NodeSqliteDatabase } from "./helpers.js";

const env = {
  ADMIN_OWNER_ID: "owner-test",
  ACCESS_TEAM_DOMAIN: "https://team.cloudflareaccess.com",
  ACCESS_APPLICATION_AUD: "application-audience",
} as Env;

describe("authentication", () => {
  let db: NodeSqliteDatabase;

  beforeEach(() => {
    db = new NodeSqliteDatabase();
    joseMocks.jwtVerify.mockReset();
    joseMocks.jwtVerify.mockResolvedValue({ payload: { sub: "user-123" } });
  });

  it("accepts a verified Access user without a PAT", async () => {
    const response = await handle(
      makeRequest("GET", "/api/health", {
        headers: { "cf-access-jwt-assertion": "signed-access-assertion" },
      }),
      env,
      db,
    );

    expect(response.status).toBe(200);
    expect(joseMocks.jwtVerify).toHaveBeenCalledWith(
      "signed-access-assertion",
      expect.anything(),
      expect.objectContaining({
        issuer: "https://team.cloudflareaccess.com",
        audience: "application-audience",
      }),
    );
  });

  it("does not grant admin access to an Access user", async () => {
    const response = await handle(
      makeRequest("POST", "/api/projects", {
        headers: { "cf-access-jwt-assertion": "signed-access-assertion" },
        body: { slug: "forbidden-project" },
      }),
      env,
      db,
    );

    expect(response.status).toBe(403);
  });

  it("keeps PAT authentication for service-authenticated automation", async () => {
    joseMocks.jwtVerify.mockResolvedValue({ payload: { sub: "" } });
    const bootstrapEnv = { ...env, ADMIN_BOOTSTRAP_TOKEN: "admin-secret" };
    const response = await handle(
      makeRequest("GET", "/api/health", {
        token: "admin-secret",
        headers: { "cf-access-jwt-assertion": "service-token-assertion" },
      }),
      bootstrapEnv,
      db,
    );

    expect(response.status).toBe(200);
  });

  it("rejects an invalid Access assertion instead of falling back to PAT", async () => {
    joseMocks.jwtVerify.mockRejectedValue(new Error("bad signature"));
    const bootstrapEnv = { ...env, ADMIN_BOOTSTRAP_TOKEN: "admin-secret" };
    const response = await handle(
      makeRequest("GET", "/api/health", {
        token: "admin-secret",
        headers: { "cf-access-jwt-assertion": "invalid-access-assertion" },
      }),
      bootstrapEnv,
      db,
    );

    expect(response.status).toBe(401);
  });
});
