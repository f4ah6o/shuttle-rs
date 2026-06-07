package project

import (
	"fmt"
	"sort"
)

type Config struct {
	Repo        string `toml:"repo"`
	DB          string `toml:"db"`
	Description string `toml:"description"`
}

type Project struct {
	Name        string `json:"name"`
	Repo        string `json:"repo"`
	DB          string `json:"db,omitempty"`
	Description string `json:"description,omitempty"`
}

type Registry struct {
	defaultProject string
	projects       map[string]Project
}

func NewRegistry(defaultProject string, configs map[string]Config) (*Registry, error) {
	projects := make(map[string]Project, len(configs))
	for name, cfg := range configs {
		if name == "" {
			return nil, fmt.Errorf("project name cannot be empty")
		}
		projects[name] = Project{
			Name:        name,
			Repo:        cfg.Repo,
			DB:          cfg.DB,
			Description: cfg.Description,
		}
	}
	if defaultProject != "" {
		if _, ok := projects[defaultProject]; !ok {
			return nil, fmt.Errorf("default project %q is not configured", defaultProject)
		}
	}
	return &Registry{defaultProject: defaultProject, projects: projects}, nil
}

func (r *Registry) List() []Project {
	names := make([]string, 0, len(r.projects))
	for name := range r.projects {
		names = append(names, name)
	}
	sort.Strings(names)
	out := make([]Project, 0, len(r.projects))
	for _, name := range names {
		out = append(out, r.projects[name])
	}
	return out
}

func (r *Registry) Default() (Project, bool) {
	if r.defaultProject == "" {
		return Project{}, false
	}
	p, ok := r.projects[r.defaultProject]
	return p, ok
}

func (r *Registry) Get(name string) (Project, bool) {
	p, ok := r.projects[name]
	return p, ok
}

func (r *Registry) Resolve(projectArg string, write bool) (Project, error) {
	if projectArg != "" {
		p, ok := r.Get(projectArg)
		if !ok {
			return Project{}, fmt.Errorf("unknown project %q", projectArg)
		}
		return p, nil
	}
	if write {
		return Project{}, fmt.Errorf("project is required for write operations")
	}
	if p, ok := r.Default(); ok {
		return p, nil
	}
	return Project{}, fmt.Errorf("project is required")
}
