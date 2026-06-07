package mcp

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

func TestToolsListIncludesGatewayTools(t *testing.T) {
	server, _ := newTestServer(t)
	rec := httptest.NewRecorder()
	req := rpcRequest(`{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}`)

	server.Handle(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
	if !bytes.Contains(rec.Body.Bytes(), []byte("shuttle_recall")) {
		t.Fatalf("tools/list missing shuttle_recall: %s", rec.Body.String())
	}
}

func TestToolCallRoutesToService(t *testing.T) {
	server, runner := newTestServer(t)
	rec := httptest.NewRecorder()
	req := rpcRequest(`{
		"jsonrpc":"2.0",
		"id":1,
		"method":"tools/call",
		"params":{"name":"shuttle_recall","arguments":{"project":"demo","query":"sqlite"}}
	}`)

	server.Handle(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
	if got := runner.args; len(got) != 2 || got[0] != "recall" || got[1] != "sqlite" {
		t.Fatalf("unexpected runner args: %#v", got)
	}
}

func rpcRequest(body string) *http.Request {
	return httptest.NewRequest(http.MethodPost, "/mcp", bytes.NewBufferString(body))
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
