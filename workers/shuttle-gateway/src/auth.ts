import type { Database } from "./database.js";
import type { Env } from "./env.js";
import { forbidden, unauthorized } from "./errors.js";
import { mintToken, nowIso, sha256Hex } from "./ids.js";
import { ensureOwner } from "./repository.js";
import type { Project } from "./types.js";

export type Scope = "read" | "write" | "admin";

export interface Principal {
  ownerId: string;
  scopes: Set<Scope>;
  /** When set, the principal is limited to a single project. */
  projectId: string | null;
}

function bearerToken(request: Request): string | null {
  const header = request.headers.get("authorization");
  if (!header) return null;
  const [scheme, token] = header.split(" ");
  if (!scheme || scheme.toLowerCase() !== "bearer" || !token) return null;
  return token.trim();
}

/**
 * Identify the caller from a bearer token. The bootstrap admin token maps to
 * the configured owner with full scope; all other tokens are scoped personal
 * access tokens stored as SHA-256 hashes in `project_grants`.
 */
export async function authenticate(
  request: Request,
  env: Env,
  db: Database,
): Promise<Principal> {
  const token = bearerToken(request);
  if (!token) throw unauthorized();

  const presented = await sha256Hex(token);

  if (env.ADMIN_BOOTSTRAP_TOKEN) {
    const expected = await sha256Hex(env.ADMIN_BOOTSTRAP_TOKEN);
    if (presented === expected) {
      const ownerId = env.ADMIN_OWNER_ID ?? "owner-local";
      await ensureOwner(db, ownerId);
      return { ownerId, scopes: new Set<Scope>(["read", "write", "admin"]), projectId: null };
    }
  }

  const row = await db.first(
    "SELECT owner_id, project_id, scopes FROM project_grants WHERE token_hash = ?",
    [presented],
  );
  if (!row) throw unauthorized("invalid token");

  const scopes = new Set(
    String(row.scopes)
      .split(",")
      .map((scope) => scope.trim())
      .filter(Boolean) as Scope[],
  );
  return {
    ownerId: String(row.owner_id),
    projectId: (row.project_id as string | null) ?? null,
    scopes,
  };
}

/** Require that the principal holds `scope` over the whole account. */
export function requireAccountScope(principal: Principal, scope: Scope): void {
  if (!principal.scopes.has(scope) && !principal.scopes.has("admin")) {
    throw forbidden(`missing ${scope} scope`);
  }
  if (principal.projectId) {
    throw forbidden("token is limited to a single project");
  }
}

/** Authorize the principal for `scope` on a specific project. */
export function authorizeProject(principal: Principal, project: Project, scope: Scope): void {
  if (principal.ownerId !== project.owner_id) {
    throw forbidden("project belongs to a different owner");
  }
  if (principal.projectId && principal.projectId !== project.id) {
    throw forbidden("token is not scoped to this project");
  }
  if (!principal.scopes.has(scope) && !principal.scopes.has("admin")) {
    throw forbidden(`missing ${scope} scope`);
  }
}

export interface MintedToken {
  token: string;
  owner_id: string;
  project_id: string | null;
  scopes: Scope[];
  label: string | null;
}

/** Create a scoped personal access token; the plaintext is returned once. */
export async function mintGrant(
  db: Database,
  input: { owner_id: string; project_id?: string | null; scopes: Scope[]; label?: string | null },
): Promise<MintedToken> {
  const token = mintToken();
  const hash = await sha256Hex(token);
  await ensureOwner(db, input.owner_id);
  await db.run(
    `INSERT INTO project_grants (token_hash, owner_id, project_id, scopes, label, created_at)
     VALUES (?, ?, ?, ?, ?, ?)`,
    [
      hash,
      input.owner_id,
      input.project_id ?? null,
      input.scopes.join(","),
      input.label ?? null,
      nowIso(),
    ],
  );
  return {
    token,
    owner_id: input.owner_id,
    project_id: input.project_id ?? null,
    scopes: input.scopes,
    label: input.label ?? null,
  };
}
