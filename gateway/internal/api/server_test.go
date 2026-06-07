package api

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/auth"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/oauth"
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

func TestOAuthRoutesIssueTokenThatAuthorizesProtectedRoutes(t *testing.T) {
	server, _ := newOAuthTestServer(t)
	routes := auth.OAuthMiddleware(*server.oauth, server.Routes())

	registerBody := `{"redirect_uris":["https://client.example.test/callback"],"client_name":"client"}`
	rec := httptest.NewRecorder()
	routes.ServeHTTP(rec, httptest.NewRequest(http.MethodPost, "/oauth/register", bytes.NewBufferString(registerBody)))
	if rec.Code != http.StatusCreated {
		t.Fatalf("register status=%d body=%s", rec.Code, rec.Body.String())
	}
	var registered struct {
		ClientID string `json:"client_id"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &registered); err != nil {
		t.Fatal(err)
	}

	verifier := "abc123abc123abc123abc123abc123abc123abc123abc123"
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	form := url.Values{
		"admin_token":           {"admin-token"},
		"response_type":         {"code"},
		"client_id":             {registered.ClientID},
		"redirect_uri":          {"https://client.example.test/callback"},
		"state":                 {"opaque=value+with/special&fragment#part"},
		"scope":                 {"mcp"},
		"code_challenge":        {challenge},
		"code_challenge_method": {"S256"},
	}
	rec = httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/oauth/authorize", strings.NewReader(form.Encode()))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	routes.ServeHTTP(rec, req)
	if rec.Code != http.StatusSeeOther {
		t.Fatalf("authorize status=%d body=%s", rec.Code, rec.Body.String())
	}
	location := rec.Header().Get("Location")
	parsed, err := url.Parse(location)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Query().Get("state") != "opaque=value+with/special&fragment#part" {
		t.Fatalf("state not preserved: %s", location)
	}

	tokenForm := url.Values{
		"grant_type":    {"authorization_code"},
		"client_id":     {registered.ClientID},
		"redirect_uri":  {"https://client.example.test/callback"},
		"code":          {parsed.Query().Get("code")},
		"code_verifier": {verifier},
	}
	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodPost, "/oauth/token", strings.NewReader(tokenForm.Encode()))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	routes.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("token status=%d body=%s", rec.Code, rec.Body.String())
	}
	var token struct {
		AccessToken string `json:"access_token"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &token); err != nil {
		t.Fatal(err)
	}

	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/projects", nil)
	routes.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("expected unauthorized without token, got %d", rec.Code)
	}
	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/projects", nil)
	req.Header.Set("Authorization", "Bearer "+token.AccessToken)
	routes.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("projects status=%d body=%s", rec.Code, rec.Body.String())
	}
}

func TestOAuthAuthorizePageRendersForKnownRedirect(t *testing.T) {
	server, _ := newOAuthTestServer(t)
	client, err := server.oauth.Store.RegisterClient(context.Background(), oauth.RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	values := url.Values{
		"response_type":         {"code"},
		"client_id":             {client.ClientID},
		"redirect_uri":          {"https://client.example.test/callback"},
		"scope":                 {"mcp"},
		"code_challenge":        {"challenge"},
		"code_challenge_method": {"S256"},
	}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/oauth/authorize?"+values.Encode(), nil)

	auth.OAuthMiddleware(*server.oauth, server.Routes()).ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("authorize page status=%d body=%s", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), `name="admin_token"`) {
		t.Fatalf("authorize page missing admin token input: %s", rec.Body.String())
	}
}

func TestOAuthAuthorizeSubmitRejectsInvalidAdminToken(t *testing.T) {
	server, _ := newOAuthTestServer(t)
	client, err := server.oauth.Store.RegisterClient(context.Background(), oauth.RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	form := url.Values{
		"admin_token":           {"wrong"},
		"response_type":         {"code"},
		"client_id":             {client.ClientID},
		"redirect_uri":          {"https://client.example.test/callback"},
		"scope":                 {"mcp"},
		"code_challenge":        {"challenge"},
		"code_challenge_method": {"S256"},
	}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/oauth/authorize", strings.NewReader(form.Encode()))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	auth.OAuthMiddleware(*server.oauth, server.Routes()).ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("authorize submit status=%d body=%s", rec.Code, rec.Body.String())
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

func newOAuthTestServer(t *testing.T) (*Server, *fakeRunner) {
	t.Helper()
	server, runner := newTestServer(t)
	store, err := oauth.Open(t.TempDir() + "/oauth.db")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	server.oauth = &auth.OAuthRuntime{
		Config: oauth.Config{PublicURL: "https://shuttle.example.test", AdminToken: "admin-token"},
		Store:  store,
	}
	return server, runner
}
