package api

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/router"
)

type fakeRunner struct {
	args []string
}

func (f *fakeRunner) Run(_ context.Context, _ project.Project, args ...string) (json.RawMessage, error) {
	f.args = append([]string{}, args...)
	return json.RawMessage(`{"ok":true}`), nil
}

func TestRememberHTTPRequiresProjectForWrite(t *testing.T) {
	server, _ := newTestServer(t)
	req := httptest.NewRequest(http.MethodPost, "/api/remember", bytes.NewBufferString(`{"text":"note"}`))
	rec := httptest.NewRecorder()

	server.Routes().ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
}

func TestRecallHTTPUsesDefaultForRead(t *testing.T) {
	server, runner := newTestServer(t)
	req := httptest.NewRequest(http.MethodPost, "/api/recall", bytes.NewBufferString(`{"query":"sqlite"}`))
	rec := httptest.NewRecorder()

	server.Routes().ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("unexpected status: %d body=%s", rec.Code, rec.Body.String())
	}
	if got := runner.args; len(got) != 2 || got[0] != "recall" || got[1] != "sqlite" {
		t.Fatalf("unexpected runner args: %#v", got)
	}
}

func newTestServer(t *testing.T) (*Server, *fakeRunner) {
	t.Helper()
	registry, err := project.NewRegistry("demo", map[string]project.Config{
		"demo": {Repo: "/tmp/demo"},
	})
	if err != nil {
		t.Fatal(err)
	}
	runner := &fakeRunner{}
	return NewServer(router.NewService(registry, runner)), runner
}
