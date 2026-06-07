package router

import (
	"context"
	"encoding/json"
	"slices"
	"testing"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
)

type fakeRunner struct {
	project project.Project
	args    []string
}

func (f *fakeRunner) Run(_ context.Context, p project.Project, args ...string) (json.RawMessage, error) {
	f.project = p
	f.args = append([]string{}, args...)
	return json.RawMessage(`{"ok":true}`), nil
}

func TestRememberRequiresExplicitProjectAndMapsKind(t *testing.T) {
	runner := &fakeRunner{}
	service := NewService(registry(t), runner)
	if _, err := service.Remember(context.Background(), "", "decision", "ship it"); err == nil {
		t.Fatal("expected missing write project to fail")
	}

	response, err := service.Remember(context.Background(), "demo", "decision", "ship it")
	if err != nil {
		t.Fatal(err)
	}
	if response.Project != "demo" {
		t.Fatalf("unexpected project: %q", response.Project)
	}
	if response.Stored == nil || !*response.Stored {
		t.Fatal("expected stored response")
	}
	if !slices.Equal(runner.args, []string{"decide", "ship it"}) {
		t.Fatalf("unexpected args: %#v", runner.args)
	}
}

func TestTaskCreateCombinesTitleAndBody(t *testing.T) {
	runner := &fakeRunner{}
	service := NewService(registry(t), runner)

	_, err := service.TaskCreate(context.Background(), "demo", "title", "body")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"task", "create", "title\n\nbody"}
	if !slices.Equal(runner.args, want) {
		t.Fatalf("unexpected args: %#v", runner.args)
	}
}

func registry(t *testing.T) *project.Registry {
	t.Helper()
	registry, err := project.NewRegistry("demo", map[string]project.Config{
		"demo": {Repo: "/tmp/demo"},
	})
	if err != nil {
		t.Fatal(err)
	}
	return registry
}
