import {
  authorize,
  authorizeAccount,
  mintGrant,
  type Principal,
  type Scope,
} from "./auth.js";
import type { Database } from "./database.js";
import { badRequest, notFound } from "./errors.js";
import { errorResponse, json, readJson } from "./http.js";
import {
  appendEventService,
  createProjectService,
  createWorkspaceService,
  latestSnapshotService,
  listEventsService,
  listProjectsService,
  publishSnapshotService,
  recallService,
} from "./services.js";
import type { ContextEnvelope, EventType } from "./types.js";

function scopeList(value: unknown): Scope[] {
  const raw = Array.isArray(value)
    ? value
    : typeof value === "string"
      ? value.split(",")
      : ["read", "write"];
  const scopes = raw
    .map((scope) => String(scope).trim())
    .filter((scope): scope is Scope => scope === "read" || scope === "write" || scope === "admin");
  if (scopes.length === 0) throw badRequest("at least one scope is required");
  return scopes;
}

/**
 * Resource-oriented API. MCP tools and these endpoints call the same
 * application services, so neither one is a privileged path. Every project
 * operation goes through `authorize`, which is the only way to obtain the
 * AuthorizedProject the services require.
 */
export async function handleApi(
  request: Request,
  db: Database,
  principal: Principal,
  segments: string[],
): Promise<Response> {
  const method = request.method;

  // /api/tokens — mint scoped personal access tokens (admin).
  if (segments.length === 1 && segments[0] === "tokens" && method === "POST") {
    const account = authorizeAccount(principal, "admin");
    const body = await readJson(request);
    let projectId: string | null = null;
    if (typeof body.project === "string" && body.project.trim()) {
      projectId = (await authorize(db, principal, body.project, "admin")).project.id;
    }
    const minted = await mintGrant(db, {
      owner_id: account.principal.ownerId,
      project_id: projectId,
      scopes: scopeList(body.scopes),
      label: typeof body.label === "string" ? body.label : null,
    });
    return json(minted, 201);
  }

  // /api/projects
  if (segments.length === 1 && segments[0] === "projects") {
    if (method === "GET") {
      const account = authorizeAccount(principal, "read");
      return json({ projects: await listProjectsService(db, account) });
    }
    if (method === "POST") {
      const account = authorizeAccount(principal, "admin");
      const body = await readJson(request);
      const project = await createProjectService(db, account, {
        slug: String(body.slug ?? ""),
        display_name: typeof body.display_name === "string" ? body.display_name : null,
        description: typeof body.description === "string" ? body.description : null,
        canonical_git_remote:
          typeof body.canonical_git_remote === "string" ? body.canonical_git_remote : null,
      });
      return json(project, 201);
    }
  }

  // /api/projects/:project/...
  if (segments.length >= 3 && segments[0] === "projects") {
    const selector = decodeURIComponent(segments[1]);
    const tail = segments.slice(2);

    if (tail.length === 1 && tail[0] === "workspaces" && method === "POST") {
      const authorized = await authorize(db, principal, selector, "write");
      const body = await readJson(request);
      const workspace = await createWorkspaceService(db, authorized, {
        client_instance_id: String(body.client_instance_id ?? ""),
        local_path_hint: typeof body.local_path_hint === "string" ? body.local_path_hint : null,
      });
      return json(workspace, 201);
    }

    if (tail.length === 1 && tail[0] === "events") {
      if (method === "POST") {
        const authorized = await authorize(db, principal, selector, "write");
        const body = await readJson(request);
        const result = await appendEventService(db, authorized, {
          event_id: typeof body.event_id === "string" ? body.event_id : null,
          event_type: String(body.event_type ?? ""),
          agent: String(body.agent ?? ""),
          session_id: String(body.session_id ?? ""),
          title: typeof body.title === "string" ? body.title : null,
          content: String(body.content ?? ""),
          tags: Array.isArray(body.tags) ? body.tags.map(String) : [],
          context: (body.context as ContextEnvelope) ?? null,
          metadata: (body.metadata as Record<string, unknown>) ?? null,
        });
        return json(result, result.deduplicated ? 200 : 201);
      }
      if (method === "GET") {
        const authorized = await authorize(db, principal, selector, "read");
        const url = new URL(request.url);
        const typeParam = url.searchParams.get("event_type");
        const limit = Number(url.searchParams.get("limit") ?? "50");
        const events = await listEventsService(db, authorized, {
          eventType: (typeParam as EventType) ?? undefined,
          limit: Number.isFinite(limit) ? limit : 50,
        });
        return json({ events });
      }
    }

    if (tail.length === 1 && tail[0] === "recall" && method === "POST") {
      const authorized = await authorize(db, principal, selector, "read");
      const body = await readJson(request);
      const results = await recallService(db, authorized, String(body.query ?? ""));
      return json({ results });
    }

    if (tail[0] === "context-snapshots") {
      if (tail.length === 1 && method === "POST") {
        const authorized = await authorize(db, principal, selector, "write");
        const body = await readJson(request);
        const snapshot = await publishSnapshotService(db, authorized, {
          workspace_id: typeof body.workspace_id === "string" ? body.workspace_id : null,
          agent: typeof body.agent === "string" ? body.agent : null,
          content: body.content,
        });
        return json(snapshot, 201);
      }
      if (tail.length === 2 && tail[1] === "latest" && method === "GET") {
        const authorized = await authorize(db, principal, selector, "read");
        const snapshot = await latestSnapshotService(db, authorized);
        if (!snapshot) throw notFound("no context snapshot published");
        return json(snapshot);
      }
    }
  }

  return errorResponse(notFound("not found"));
}
