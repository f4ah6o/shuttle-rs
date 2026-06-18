import {
  authorizeProject,
  mintGrant,
  requireAccountScope,
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
  resolveProject,
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
 * application services, so neither one is a privileged path.
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
    requireAccountScope(principal, "admin");
    const body = await readJson(request);
    let projectId: string | null = null;
    if (typeof body.project === "string" && body.project.trim()) {
      projectId = (await resolveProject(db, principal.ownerId, body.project)).id;
    }
    const minted = await mintGrant(db, {
      owner_id: principal.ownerId,
      project_id: projectId,
      scopes: scopeList(body.scopes),
      label: typeof body.label === "string" ? body.label : null,
    });
    return json(minted, 201);
  }

  // /api/projects
  if (segments.length === 1 && segments[0] === "projects") {
    if (method === "GET") {
      return json({ projects: await listProjectsService(db, principal.ownerId) });
    }
    if (method === "POST") {
      requireAccountScope(principal, "admin");
      const body = await readJson(request);
      const project = await createProjectService(db, principal.ownerId, {
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
    const project = await resolveProject(db, principal.ownerId, selector);
    const tail = segments.slice(2);
    const writeScope: Scope = "write";
    const readScope: Scope = "read";

    if (tail.length === 1 && tail[0] === "workspaces" && method === "POST") {
      authorizeProject(principal, project, writeScope);
      const body = await readJson(request);
      const workspace = await createWorkspaceService(db, project, {
        client_instance_id: String(body.client_instance_id ?? ""),
        local_path_hint: typeof body.local_path_hint === "string" ? body.local_path_hint : null,
      });
      return json(workspace, 201);
    }

    if (tail.length === 1 && tail[0] === "events") {
      if (method === "POST") {
        authorizeProject(principal, project, writeScope);
        const body = await readJson(request);
        const result = await appendEventService(db, project, {
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
        authorizeProject(principal, project, readScope);
        const url = new URL(request.url);
        const typeParam = url.searchParams.get("event_type");
        const limit = Number(url.searchParams.get("limit") ?? "50");
        const events = await listEventsService(db, project, {
          eventType: (typeParam as EventType) ?? undefined,
          limit: Number.isFinite(limit) ? limit : 50,
        });
        return json({ events });
      }
    }

    if (tail.length === 1 && tail[0] === "recall" && method === "POST") {
      authorizeProject(principal, project, readScope);
      const body = await readJson(request);
      const results = await recallService(db, project, String(body.query ?? ""));
      return json({ results });
    }

    if (tail[0] === "context-snapshots") {
      if (tail.length === 1 && method === "POST") {
        authorizeProject(principal, project, writeScope);
        const body = await readJson(request);
        const snapshot = await publishSnapshotService(db, project, {
          workspace_id: typeof body.workspace_id === "string" ? body.workspace_id : null,
          agent: typeof body.agent === "string" ? body.agent : null,
          content: body.content,
        });
        return json(snapshot, 201);
      }
      if (tail.length === 2 && tail[1] === "latest" && method === "GET") {
        authorizeProject(principal, project, readScope);
        const snapshot = await latestSnapshotService(db, project);
        if (!snapshot) throw notFound("no context snapshot published");
        return json(snapshot);
      }
    }
  }

  return errorResponse(notFound("not found"));
}
