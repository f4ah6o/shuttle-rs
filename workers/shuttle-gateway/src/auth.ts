import type { Database } from "./database.js";
import type { Env } from "./env.js";
import { forbidden, notFound, unauthorized } from "./errors.js";
import { mintToken, nowIso, sha256Hex } from "./ids.js";
import { ensureOwner, findProject } from "./repository.js";
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

async function hasAdminGrant(db: Database, ownerId: string): Promise<boolean> {
  const rows = await db.query("SELECT scopes FROM project_grants WHERE owner_id = ?", [ownerId]);
  return rows.some((row) =>
    String(row.scopes)
      .split(",")
      .map((scope) => scope.trim())
      .includes("admin"),
  );
}

/**
 * Identify the caller from a bearer token.
 *
 * The bootstrap admin token is genuinely one-time: it is accepted only until
 * the first persistent admin token is minted for the owner. After that it is
 * rejected, so it cannot serve as a permanent admin bearer. All other tokens
 * are scoped personal access tokens stored as SHA-256 hashes in
 * `project_grants`.
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
      if (await hasAdminGrant(db, ownerId)) {
        throw unauthorized(
          "bootstrap token already consumed; an admin token has been minted",
        );
      }
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
function hasScope(principal: Principal, scope: Scope): boolean {
  return principal.scopes.has(scope) || principal.scopes.has("admin");
}

// Branded capability tokens. The brand symbols are module-private, so an
// AuthorizedProject/AuthorizedAccount can only be produced by the authorize
// functions below — never fabricated by a caller. Service entry points accept
// these instead of a raw project_id, which makes it impossible to reach the
// repository without having passed authorization first.
const projectBrand: unique symbol = Symbol("authorized-project");
const accountBrand: unique symbol = Symbol("authorized-account");

/** Proof that the caller is authorized for `scope` on a specific project. */
export interface AuthorizedProject<S extends Scope = Scope> {
  readonly project: Project;
  readonly principal: Principal;
  readonly scope: S;
  readonly [projectBrand]: true;
}

/** Proof that the caller holds `scope` across the whole account. */
export interface AuthorizedAccount<S extends Scope = Scope> {
  readonly principal: Principal;
  readonly scope: S;
  readonly [accountBrand]: true;
}

/** Require that the principal holds `scope` over the whole account. */
export function authorizeAccount<S extends Scope>(
  principal: Principal,
  scope: S,
): AuthorizedAccount<S> {
  if (!hasScope(principal, scope)) {
    throw forbidden(`missing ${scope} scope`);
  }
  if (principal.projectId) {
    throw forbidden("token is limited to a single project");
  }
  return { principal, scope, [accountBrand]: true };
}

/**
 * Resolve a project by id or slug and authorize the principal for `scope` on
 * it in one step. This is the only way to obtain an AuthorizedProject, so every
 * transport (HTTP, MCP, future CLI/workers) shares one authorization contract.
 */
export async function authorize<S extends Scope>(
  db: Database,
  principal: Principal,
  selector: string,
  scope: S,
): Promise<AuthorizedProject<S>> {
  const trimmed = (selector ?? "").trim();
  if (!trimmed) throw forbidden("project is required");
  const project = await findProject(db, principal.ownerId, trimmed);
  if (!project) throw notFound(`unknown project ${JSON.stringify(trimmed)}`);

  if (principal.ownerId !== project.owner_id) {
    throw forbidden("project belongs to a different owner");
  }
  if (principal.projectId && principal.projectId !== project.id) {
    throw forbidden("token is not scoped to this project");
  }
  if (!hasScope(principal, scope)) {
    throw forbidden(`missing ${scope} scope`);
  }
  return { project, principal, scope, [projectBrand]: true };
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
