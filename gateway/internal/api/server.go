package api

import (
	"encoding/json"
	"net/http"
	"strings"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/mcp"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/router"
)

type Server struct {
	service *router.Service
}

func NewServer(service *router.Service) *Server {
	return &Server{service: service}
}

func (s *Server) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/projects", s.projects)
	mux.HandleFunc("GET /api/projects/current", s.currentProject)
	mux.HandleFunc("POST /api/projects/use", s.useProject)
	mux.HandleFunc("POST /api/recall", s.recall)
	mux.HandleFunc("POST /api/remember", s.remember)
	mux.HandleFunc("GET /api/context", s.context)
	mux.HandleFunc("GET /api/tasks", s.tasks)
	mux.HandleFunc("POST /api/tasks", s.createTask)
	mux.HandleFunc("PATCH /api/tasks/", s.updateTask)
	mux.HandleFunc("POST /api/tasks/", s.doneTask)
	mux.HandleFunc("POST /mcp", mcp.NewServer(s.service).Handle)
	mux.HandleFunc("GET /mcp", func(w http.ResponseWriter, _ *http.Request) {
		_ = writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	return mux
}

func (s *Server) projects(w http.ResponseWriter, _ *http.Request) {
	_ = writeJSON(w, http.StatusOK, map[string]any{"projects": s.service.ListProjects()})
}

func (s *Server) currentProject(w http.ResponseWriter, _ *http.Request) {
	p, ok := s.service.CurrentProject()
	if !ok {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "no current or default project"})
		return
	}
	_ = writeJSON(w, http.StatusOK, p)
}

func (s *Server) useProject(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
	}
	if !decode(w, r, &req) {
		return
	}
	p, err := s.service.UseProject(req.Project)
	if err != nil {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": err.Error()})
		return
	}
	_ = writeJSON(w, http.StatusOK, p)
}

func (s *Server) recall(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Query   string `json:"query"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Query == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "query is required"})
		return
	}
	response, err := s.service.Recall(r.Context(), req.Project, req.Query)
	s.respond(w, response, err)
}

func (s *Server) remember(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Kind    string `json:"kind"`
		Text    string `json:"text"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Text == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "text is required"})
		return
	}
	response, err := s.service.Remember(r.Context(), req.Project, req.Kind, req.Text)
	s.respond(w, response, err)
}

func (s *Server) context(w http.ResponseWriter, r *http.Request) {
	response, err := s.service.Context(r.Context(), r.URL.Query().Get("project"))
	s.respond(w, response, err)
}

func (s *Server) tasks(w http.ResponseWriter, r *http.Request) {
	response, err := s.service.TaskList(r.Context(), r.URL.Query().Get("project"))
	s.respond(w, response, err)
}

func (s *Server) createTask(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Title   string `json:"title"`
		Body    string `json:"body"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Title == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "title is required"})
		return
	}
	response, err := s.service.TaskCreate(r.Context(), req.Project, req.Title, req.Body)
	s.respond(w, response, err)
}

func (s *Server) updateTask(w http.ResponseWriter, r *http.Request) {
	id := strings.TrimPrefix(r.URL.Path, "/api/tasks/")
	if id == "" || strings.Contains(id, "/") {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
		return
	}
	var req struct {
		Project string `json:"project"`
		Text    string `json:"text"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Text == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "text is required"})
		return
	}
	response, err := s.service.TaskUpdate(r.Context(), req.Project, id, req.Text)
	s.respond(w, response, err)
}

func (s *Server) doneTask(w http.ResponseWriter, r *http.Request) {
	path := strings.TrimPrefix(r.URL.Path, "/api/tasks/")
	id, ok := strings.CutSuffix(path, "/done")
	if !ok || id == "" {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
		return
	}
	var req struct {
		Project string `json:"project"`
	}
	if !decode(w, r, &req) {
		return
	}
	response, err := s.service.TaskDone(r.Context(), req.Project, id)
	s.respond(w, response, err)
}

func (s *Server) respond(w http.ResponseWriter, response router.Response, err error) {
	if err != nil {
		body := map[string]any{"error": err.Error()}
		if response.Project != "" {
			body["project"] = response.Project
		}
		_ = writeJSON(w, http.StatusBadRequest, body)
		return
	}
	_ = writeJSON(w, http.StatusOK, response)
}

func decode(w http.ResponseWriter, r *http.Request, v any) bool {
	if err := json.NewDecoder(r.Body).Decode(v); err != nil {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid json"})
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, v any) error {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	return json.NewEncoder(w).Encode(v)
}
