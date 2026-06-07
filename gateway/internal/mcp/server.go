package mcp

import (
	"encoding/json"
	"net/http"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/router"
)

type Server struct {
	service *router.Service
}

type request struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

type toolCallParams struct {
	Name      string          `json:"name"`
	Arguments json.RawMessage `json:"arguments"`
}

func NewServer(service *router.Service) *Server {
	return &Server{service: service}
}

func (s *Server) Handle(w http.ResponseWriter, r *http.Request) {
	var req request
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		write(w, response(nil, nil, errObj(-32700, "parse error")))
		return
	}
	id := req.ID
	if len(id) == 0 {
		id = []byte("null")
	}
	if req.JSONRPC != "2.0" {
		write(w, response(id, nil, errObj(-32600, "invalid jsonrpc version")))
		return
	}
	switch req.Method {
	case "initialize":
		write(w, response(id, map[string]any{
			"protocolVersion": "2025-11-25",
			"capabilities":    map[string]any{"tools": map[string]any{}},
			"serverInfo":      map[string]string{"name": "shuttle-gateway", "version": "0.1.0"},
		}, nil))
	case "notifications/initialized":
		w.WriteHeader(http.StatusNoContent)
	case "tools/list":
		write(w, response(id, map[string]any{"tools": tools()}, nil))
	case "tools/call":
		result, err := s.callTool(r, req.Params)
		if err != nil {
			write(w, response(id, nil, errObj(-32603, err.Error())))
			return
		}
		write(w, response(id, map[string]any{
			"content": []map[string]string{{"type": "text", "text": string(result)}},
		}, nil))
	default:
		write(w, response(id, nil, errObj(-32601, "method not found")))
	}
}

func (s *Server) callTool(r *http.Request, params json.RawMessage) (json.RawMessage, error) {
	var call toolCallParams
	if err := json.Unmarshal(params, &call); err != nil {
		return nil, err
	}
	args := call.Arguments
	if len(args) == 0 {
		args = []byte("{}")
	}
	switch call.Name {
	case "shuttle_projects":
		return json.Marshal(map[string]any{"projects": s.service.ListProjects()})
	case "shuttle_current_project":
		p, ok := s.service.CurrentProject()
		if !ok {
			return nil, errString("no current or default project")
		}
		return json.Marshal(p)
	case "shuttle_use_project":
		var req struct {
			Project string `json:"project"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		p, err := s.service.UseProject(req.Project)
		if err != nil {
			return nil, err
		}
		return json.Marshal(p)
	case "shuttle_context":
		var req struct {
			Project string `json:"project"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.Context(r.Context(), req.Project))
	case "shuttle_recall":
		var req struct {
			Project string `json:"project"`
			Query   string `json:"query"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.Recall(r.Context(), req.Project, req.Query))
	case "shuttle_remember":
		var req struct {
			Project string `json:"project"`
			Kind    string `json:"kind"`
			Text    string `json:"text"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.Remember(r.Context(), req.Project, req.Kind, req.Text))
	case "shuttle_task_list":
		var req struct {
			Project string `json:"project"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.TaskList(r.Context(), req.Project))
	case "shuttle_task_create":
		var req struct {
			Project string `json:"project"`
			Title   string `json:"title"`
			Body    string `json:"body"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.TaskCreate(r.Context(), req.Project, req.Title, req.Body))
	case "shuttle_task_update":
		var req struct {
			Project string `json:"project"`
			TaskID  string `json:"task_id"`
			Text    string `json:"text"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.TaskUpdate(r.Context(), req.Project, req.TaskID, req.Text))
	case "shuttle_task_done":
		var req struct {
			Project string `json:"project"`
			TaskID  string `json:"task_id"`
		}
		if err := json.Unmarshal(args, &req); err != nil {
			return nil, err
		}
		return marshalServiceResponse(s.service.TaskDone(r.Context(), req.Project, req.TaskID))
	default:
		return nil, errString("unknown tool: " + call.Name)
	}
}

func marshalServiceResponse(response router.Response, err error) (json.RawMessage, error) {
	if err != nil {
		return nil, err
	}
	return json.Marshal(response)
}

type simpleError string

func (e simpleError) Error() string { return string(e) }

func errString(s string) error { return simpleError(s) }

func tools() []map[string]any {
	return []map[string]any{
		tool("shuttle_projects", "List configured Shuttle projects", nil, nil),
		tool("shuttle_current_project", "Read the convenience current project or configured default", nil, nil),
		tool("shuttle_use_project", "Set the convenience current project", map[string]any{
			"project": stringSchema("Configured project name"),
		}, []string{"project"}),
		tool("shuttle_context", "Read Shuttle context for a project", map[string]any{
			"project": stringSchema("Configured project name; optional only when a default project is configured"),
		}, nil),
		tool("shuttle_recall", "Search Shuttle memories in a project", map[string]any{
			"project": stringSchema("Configured project name; optional only when a default project is configured"),
			"query":   stringSchema("Recall query"),
		}, []string{"query"}),
		tool("shuttle_remember", "Store a Shuttle memory in a project", map[string]any{
			"project": stringSchema("Configured project name"),
			"kind":    enumSchema("Memory kind", []string{"memory", "decision", "observation", "pattern", "fact", "bug"}),
			"text":    stringSchema("Memory text"),
		}, []string{"project", "text"}),
		tool("shuttle_task_list", "List Shuttle tasks in a project", map[string]any{
			"project": stringSchema("Configured project name; optional only when a default project is configured"),
		}, nil),
		tool("shuttle_task_create", "Create a Shuttle task in a project", map[string]any{
			"project": stringSchema("Configured project name"),
			"title":   stringSchema("Task title"),
			"body":    stringSchema("Optional task body"),
		}, []string{"project", "title"}),
		tool("shuttle_task_update", "Update a Shuttle task in a project", map[string]any{
			"project": stringSchema("Configured project name"),
			"task_id": stringSchema("Task UUID"),
			"text":    stringSchema("Update text"),
		}, []string{"project", "task_id", "text"}),
		tool("shuttle_task_done", "Complete a Shuttle task in a project", map[string]any{
			"project": stringSchema("Configured project name"),
			"task_id": stringSchema("Task UUID"),
		}, []string{"project", "task_id"}),
	}
}

func tool(name string, description string, properties map[string]any, required []string) map[string]any {
	schema := map[string]any{
		"type":                 "object",
		"additionalProperties": false,
	}
	if properties != nil {
		schema["properties"] = properties
	}
	if len(required) > 0 {
		schema["required"] = required
	}
	return map[string]any{
		"name":        name,
		"description": description,
		"inputSchema": schema,
	}
}

func stringSchema(description string) map[string]any {
	return map[string]any{"type": "string", "description": description}
}

func enumSchema(description string, values []string) map[string]any {
	return map[string]any{"type": "string", "description": description, "enum": values}
}

func response(id json.RawMessage, result any, err any) map[string]any {
	resp := map[string]any{"jsonrpc": "2.0", "id": json.RawMessage(id)}
	if err != nil {
		resp["error"] = err
	} else {
		resp["result"] = result
	}
	return resp
}

func errObj(code int, message string) map[string]any {
	return map[string]any{"code": code, "message": message}
}

func write(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}
