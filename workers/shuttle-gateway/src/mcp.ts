import { authorize, authorizeAccount, type Principal } from "./auth.js";
import type { Database } from "./database.js";
import { badRequest } from "./errors.js";
import {
  completeTaskService,
  createProjectService,
  createTaskService,
  latestSnapshotService,
  listProjectsService,
  listTasksService,
  publishSnapshotService,
  recallService,
  rememberService,
  updateTaskService,
} from "./services.js";
import type { ContextEnvelope } from "./types.js";

export interface McpContext {
  db: Database;
  principal: Principal;
}

export interface RpcRequest {
  jsonrpc?: string;
  id?: unknown;
  method: string;
  params?: Record<string, unknown>;
}

const str = (args: Record<string, unknown>, key: string): string => {
  const value = args[key];
  if (typeof value !== "string" || value.trim() === "") {
    throw badRequest(`${key} is required`);
  }
  return value;
};

const optStr = (args: Record<string, unknown>, key: string): string | undefined => {
  const value = args[key];
  return typeof value === "string" && value.trim() !== "" ? value : undefined;
};

const envelope = (args: Record<string, unknown>): ContextEnvelope | null => {
  const context = args.context;
  return context && typeof context === "object" ? (context as ContextEnvelope) : null;
};

const stringSchema = (description: string) => ({ type: "string", description });
const contextSchema = {
  type: "object",
  description: "Client context envelope: workspace_id, agent, session_id, and repo metadata.",
  additionalProperties: true,
};

function tool(
  name: string,
  description: string,
  properties: Record<string, unknown>,
  required: string[],
) {
  return {
    name,
    description,
    inputSchema: {
      type: "object",
      properties,
      required,
      additionalProperties: false,
    },
  };
}

export function gatewayTools() {
  return [
    tool("shuttle_projects", "List Shuttle projects visible to the caller", {}, []),
    tool(
      "shuttle_project_create",
      "Create a logical Shuttle project (no repository path required)",
      {
        slug: stringSchema("Unique project slug"),
        display_name: stringSchema("Human-readable project name"),
        description: stringSchema("Project description"),
        canonical_git_remote: stringSchema("Optional canonical Git remote for linking"),
      },
      ["slug"],
    ),
    tool(
      "shuttle_remember",
      "Store a memory in a project's shared event log",
      {
        project: stringSchema("Project slug or id"),
        kind: {
          type: "string",
          description: "Memory kind",
          enum: ["memory", "decision", "observation", "pattern", "fact", "bug"],
        },
        text: stringSchema("Memory text"),
        context: contextSchema,
      },
      ["project", "text"],
    ),
    tool(
      "shuttle_recall",
      "Search a project's shared memories",
      { project: stringSchema("Project slug or id"), query: stringSchema("Recall query") },
      ["project", "query"],
    ),
    tool(
      "shuttle_context",
      "Read the latest published context snapshot for a project",
      { project: stringSchema("Project slug or id") },
      ["project"],
    ),
    tool(
      "shuttle_context_publish",
      "Publish a repository/context snapshot for a project",
      {
        project: stringSchema("Project slug or id"),
        content: { type: "object", description: "Snapshot payload", additionalProperties: true },
        context: contextSchema,
      },
      ["project", "content"],
    ),
    tool(
      "shuttle_task_create",
      "Create a task in a project",
      {
        project: stringSchema("Project slug or id"),
        title: stringSchema("Task title"),
        body: stringSchema("Optional task body"),
        context: contextSchema,
      },
      ["project", "title"],
    ),
    tool(
      "shuttle_task_list",
      "List tasks in a project",
      { project: stringSchema("Project slug or id") },
      ["project"],
    ),
    tool(
      "shuttle_task_update",
      "Append an update to a task",
      {
        project: stringSchema("Project slug or id"),
        task_id: stringSchema("Task id"),
        text: stringSchema("Update text"),
        context: contextSchema,
      },
      ["project", "task_id", "text"],
    ),
    tool(
      "shuttle_task_done",
      "Mark a task complete",
      {
        project: stringSchema("Project slug or id"),
        task_id: stringSchema("Task id"),
        context: contextSchema,
      },
      ["project", "task_id"],
    ),
  ];
}

async function callTool(
  name: string,
  args: Record<string, unknown>,
  ctx: McpContext,
): Promise<unknown> {
  const { db, principal } = ctx;

  // Every project operation resolves an explicit project and authorizes it per
  // request via `authorize`. There is no process-global "current project", so
  // one client's selection can never affect another's reads or writes.
  const forProject = <S extends "read" | "write">(scope: S) =>
    authorize(db, principal, str(args, "project"), scope);

  switch (name) {
    case "shuttle_projects": {
      return { projects: await listProjectsService(db, authorizeAccount(principal, "read")) };
    }
    case "shuttle_project_create": {
      return createProjectService(db, authorizeAccount(principal, "admin"), {
        slug: str(args, "slug"),
        display_name: optStr(args, "display_name"),
        description: optStr(args, "description"),
        canonical_git_remote: optStr(args, "canonical_git_remote"),
      });
    }
    case "shuttle_remember": {
      return rememberService(db, await forProject("write"), {
        kind: optStr(args, "kind"),
        text: str(args, "text"),
        context: envelope(args),
      });
    }
    case "shuttle_recall": {
      return { results: await recallService(db, await forProject("read"), str(args, "query")) };
    }
    case "shuttle_context": {
      return { snapshot: await latestSnapshotService(db, await forProject("read")) };
    }
    case "shuttle_context_publish": {
      return publishSnapshotService(db, await forProject("write"), {
        workspace_id: envelope(args)?.workspace_id ?? null,
        agent: envelope(args)?.agent ?? null,
        content: args.content,
      });
    }
    case "shuttle_task_create": {
      return createTaskService(db, await forProject("write"), {
        title: str(args, "title"),
        body: optStr(args, "body"),
        context: envelope(args),
      });
    }
    case "shuttle_task_list": {
      return { tasks: await listTasksService(db, await forProject("read")) };
    }
    case "shuttle_task_update": {
      return updateTaskService(
        db,
        await forProject("write"),
        str(args, "task_id"),
        str(args, "text"),
        envelope(args),
      );
    }
    case "shuttle_task_done": {
      return completeTaskService(db, await forProject("write"), str(args, "task_id"), envelope(args));
    }
    default:
      throw badRequest(`unknown tool: ${name}`);
  }
}

const ok = (id: unknown, result: unknown) => ({ jsonrpc: "2.0", id, result });
const fail = (id: unknown, code: number, message: string) => ({
  jsonrpc: "2.0",
  id,
  error: { code, message },
});

export async function handleMcp(request: RpcRequest, ctx: McpContext): Promise<unknown> {
  const id = request.id ?? null;
  if (request.jsonrpc !== "2.0") {
    return fail(id, -32600, "invalid jsonrpc version");
  }
  switch (request.method) {
    case "initialize":
      return ok(id, {
        protocolVersion: "2025-11-25",
        capabilities: { tools: {} },
        serverInfo: { name: "shuttle-gateway", version: "0.1.0" },
      });
    case "tools/list":
      return ok(id, { tools: gatewayTools() });
    case "tools/call": {
      const params = request.params ?? {};
      const name = typeof params.name === "string" ? params.name : "";
      const args = (params.arguments as Record<string, unknown>) ?? {};
      if (!name) return fail(id, -32602, "missing tool name");
      try {
        const value = await callTool(name, args, ctx);
        return ok(id, {
          content: [{ type: "text", text: JSON.stringify(value) }],
          structuredContent: value,
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return fail(id, -32603, message);
      }
    }
    default:
      return fail(id, -32601, "method not found");
  }
}
