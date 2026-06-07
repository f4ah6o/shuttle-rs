package router

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/subprocess"
)

type Service struct {
	Projects *project.Registry
	Runner   subprocess.Runner
	mu       sync.Mutex
	current  string
}

type Response struct {
	Project string          `json:"project"`
	Result  json.RawMessage `json:"result"`
	Stored  *bool           `json:"stored,omitempty"`
}

func NewService(projects *project.Registry, runner subprocess.Runner) *Service {
	return &Service{Projects: projects, Runner: runner}
}

func (s *Service) ListProjects() []project.Project {
	return s.Projects.List()
}

func (s *Service) UseProject(name string) (project.Project, error) {
	p, ok := s.Projects.Get(name)
	if !ok {
		return project.Project{}, fmt.Errorf("unknown project %q", name)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.current = name
	return p, nil
}

func (s *Service) CurrentProject() (project.Project, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.current != "" {
		if p, ok := s.Projects.Get(s.current); ok {
			return p, true
		}
	}
	return s.Projects.Default()
}

func (s *Service) Context(ctx context.Context, projectName string) (Response, error) {
	return s.run(ctx, projectName, false, "context")
}

func (s *Service) Recall(ctx context.Context, projectName, query string) (Response, error) {
	if strings.TrimSpace(query) == "" {
		return Response{}, fmt.Errorf("query is required")
	}
	return s.run(ctx, projectName, false, "recall", query)
}

func (s *Service) Remember(ctx context.Context, projectName, kind, text string) (Response, error) {
	if strings.TrimSpace(text) == "" {
		return Response{}, fmt.Errorf("text is required")
	}
	command := "remember"
	switch kind {
	case "", "memory":
		command = "remember"
	case "decision":
		command = "decide"
	case "observation":
		command = "observe"
	case "pattern":
		command = "pattern"
	case "fact":
		command = "fact"
	case "bug":
		command = "bug"
	default:
		return Response{}, fmt.Errorf("unknown memory kind %q", kind)
	}
	return s.runStored(ctx, projectName, command, text)
}

func (s *Service) TaskList(ctx context.Context, projectName string) (Response, error) {
	return s.run(ctx, projectName, false, "task", "list")
}

func (s *Service) TaskCreate(ctx context.Context, projectName, title, body string) (Response, error) {
	if strings.TrimSpace(title) == "" {
		return Response{}, fmt.Errorf("title is required")
	}
	content := title
	if body != "" {
		content += "\n\n" + body
	}
	return s.runStored(ctx, projectName, "task", "create", content)
}

func (s *Service) TaskUpdate(ctx context.Context, projectName, id, text string) (Response, error) {
	if strings.TrimSpace(id) == "" {
		return Response{}, fmt.Errorf("task id is required")
	}
	if strings.TrimSpace(text) == "" {
		return Response{}, fmt.Errorf("text is required")
	}
	return s.runStored(ctx, projectName, "task", "update", id, text)
}

func (s *Service) TaskDone(ctx context.Context, projectName, id string) (Response, error) {
	if strings.TrimSpace(id) == "" {
		return Response{}, fmt.Errorf("task id is required")
	}
	return s.runStored(ctx, projectName, "task", "done", id)
}

func (s *Service) runStored(ctx context.Context, projectName string, args ...string) (Response, error) {
	return s.run(ctx, projectName, true, args...)
}

func (s *Service) run(ctx context.Context, projectName string, write bool, args ...string) (Response, error) {
	p, err := s.Projects.Resolve(projectName, write)
	if err != nil {
		return Response{}, err
	}
	result, err := s.Runner.Run(ctx, p, args...)
	if err != nil {
		return Response{Project: p.Name}, err
	}
	response := Response{Project: p.Name, Result: result}
	if write {
		stored := true
		response.Stored = &stored
	}
	return response, nil
}
