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
  claimTaskService,
  claimWorkflowStepService,
  completeTaskService,
  createTaskService,
  createProjectService,
  createWorkspaceService,
  latestSnapshotService,
  listEventsService,
  listProjectsService,
  listTasksService,
  publishSnapshotService,
  recallService,
  updateTaskService,
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
 * Keyset cursor for event listing: `<created_at>|<id>`, matching the listing
 * order `created_at DESC, id DESC`. Split on the first `|` because RFC3339
 * timestamps never contain one, while a client-supplied id could.
 */
function parseBeforeCursor(raw: string | null): { createdAt: string; id: string } | undefined {
  if (!raw) return undefined;
  const separator = raw.indexOf("|");
  if (separator <= 0 || separator === raw.length - 1) {
    throw badRequest("invalid before cursor; expected <created_at>|<id>");
  }
  return { createdAt: raw.slice(0, separator), id: raw.slice(separator + 1) };
}

function parseAfterCursor(raw: string | null): { createdAt: string; id: string } | undefined {
  if (!raw) return undefined;
  const separator = raw.indexOf("|");
  if (separator <= 0 || separator === raw.length - 1) {
    throw badRequest("invalid after cursor; expected <created_at>|<id>");
  }
  return { createdAt: raw.slice(0, separator), id: raw.slice(separator + 1) };
}

function requestContext(body: Record<string, unknown>, principal: Principal): ContextEnvelope | null {
  const raw = body.context;
  const context = raw && typeof raw === "object" ? { ...(raw as ContextEnvelope) } : {};
  context.agent = principal.agentId;
  return context;
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
      agent_id:
        typeof body.agent_id === "string" && body.agent_id.trim()
          ? body.agent_id.trim()
          : typeof body.label === "string" && body.label.trim()
            ? body.label.trim()
            : "agent",
      client_instance_id:
        typeof body.client_instance_id === "string" && body.client_instance_id.trim()
          ? body.client_instance_id.trim()
          : null,
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
        client_instance_id: authorized.principal.clientInstanceId ?? String(body.client_instance_id ?? ""),
        local_path_hint: typeof body.local_path_hint === "string" ? body.local_path_hint : null,
      });
      return json(workspace, 201);
    }

    if (tail.length === 1 && tail[0] === "tasks") {
      if (method === "GET") {
        const authorized = await authorize(db, principal, selector, "read");
        return json({ tasks: await listTasksService(db, authorized) });
      }
      if (method === "POST") {
        const authorized = await authorize(db, principal, selector, "write");
        const body = await readJson(request);
        const context = requestContext(body, principal);
        const task = await createTaskService(db, authorized, {
          title: String(body.title ?? ""),
          body: typeof body.body === "string" ? body.body : null,
          context,
        });
        return json(task, 201);
      }
    }

    if (tail.length === 3 && tail[0] === "tasks" && tail[2] === "claim" && method === "POST") {
      const authorized = await authorize(db, principal, selector, "write");
      const body = await readJson(request);
      const result = await claimTaskService(db, authorized, decodeURIComponent(tail[1]), {
        event_id: typeof body.event_id === "string" ? body.event_id : null,
        session_id: typeof body.session_id === "string" ? body.session_id : null,
        context: requestContext(body, principal),
        created_at: typeof body.created_at === "string" ? body.created_at : null,
        takeover: body.takeover === true,
        reason: typeof body.reason === "string" ? body.reason : null,
      });
      return json(result, result.deduplicated ? 200 : 201);
    }

    if (tail.length === 3 && tail[0] === "tasks" && tail[2] === "update" && method === "POST") {
      const authorized = await authorize(db, principal, selector, "write");
      const body = await readJson(request);
      const result = await updateTaskService(
        db,
        authorized,
        decodeURIComponent(tail[1]),
        String(body.text ?? ""),
        requestContext(body, principal),
      );
      return json(result, 201);
    }

    if (tail.length === 3 && tail[0] === "tasks" && tail[2] === "done" && method === "POST") {
      const authorized = await authorize(db, principal, selector, "write");
      const body = await readJson(request);
      const result = await completeTaskService(
        db,
        authorized,
        decodeURIComponent(tail[1]),
        requestContext(body, principal),
      );
      return json(result, 201);
    }

    if (
      tail.length === 5 &&
      tail[0] === "workflows" &&
      tail[2] === "steps" &&
      tail[4] === "claim" &&
      method === "POST"
    ) {
      const authorized = await authorize(db, principal, selector, "write");
      const body = await readJson(request);
      const result = await claimWorkflowStepService(
        db,
        authorized,
        decodeURIComponent(tail[1]),
        decodeURIComponent(tail[3]),
        {
          event_id: typeof body.event_id === "string" ? body.event_id : null,
          session_id: typeof body.session_id === "string" ? body.session_id : null,
          context: requestContext(body, principal),
          created_at: typeof body.created_at === "string" ? body.created_at : null,
          takeover: body.takeover === true,
          reason: typeof body.reason === "string" ? body.reason : null,
        },
      );
      return json(result, result.deduplicated ? 200 : 201);
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
          created_at: typeof body.created_at === "string" ? body.created_at : null,
        });
        return json(result, result.deduplicated ? 200 : 201);
      }
      if (method === "GET") {
        const authorized = await authorize(db, principal, selector, "read");
        const url = new URL(request.url);
        const typeParam = url.searchParams.get("event_type");
        const agent = url.searchParams.get("agent") || undefined;
        const recipient = url.searchParams.get("recipient") || undefined;
        const tag = url.searchParams.get("tag") || undefined;
        const query = url.searchParams.get("query") || undefined;
        const id = url.searchParams.get("id") || undefined;
        const workspaceId = url.searchParams.get("workspace_id") || undefined;
        const after = parseAfterCursor(url.searchParams.get("after"));
        const rawLimit = Number(url.searchParams.get("limit") ?? "50");
        const limit = Math.max(1, Math.min(Number.isFinite(rawLimit) ? rawLimit : 50, 500));
        const page = await listEventsService(db, authorized, {
          eventType: (typeParam as EventType) ?? undefined,
          agent,
          recipient,
          tag,
          query,
          id,
          workspaceId,
          after,
          limit: limit + 1,
          before: parseBeforeCursor(url.searchParams.get("before")),
        });
        const hasMore = page.length > limit;
        const events = page.slice(0, limit);
        const last = events[events.length - 1];
        return json({
          events,
          has_more: hasMore,
          next_before: last ? `${last.created_at}|${last.id}` : null,
          next_after: last ? `${last.created_at}|${last.id}` : null,
        });
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
