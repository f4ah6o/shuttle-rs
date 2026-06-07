package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/oauth"
)

func TestMiddlewareAllowsWhenEnvUnset(t *testing.T) {
	t.Setenv("TEST_GATEWAY_TOKEN", "")
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	rec := httptest.NewRecorder()
	Middleware("TEST_GATEWAY_TOKEN", okHandler()).ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
}

func TestMiddlewareRequiresBearerWhenEnvSet(t *testing.T) {
	t.Setenv("TEST_GATEWAY_TOKEN", "secret")
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	rec := httptest.NewRecorder()
	Middleware("TEST_GATEWAY_TOKEN", okHandler()).ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("unexpected status: %d", rec.Code)
	}

	req = httptest.NewRequest(http.MethodGet, "/", nil)
	req.Header.Set("Authorization", "Bearer secret")
	rec = httptest.NewRecorder()
	Middleware("TEST_GATEWAY_TOKEN", okHandler()).ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
}

func TestOAuthMiddlewareAllowsPublicMetadata(t *testing.T) {
	runtime := oauthRuntime(t)
	req := httptest.NewRequest(http.MethodGet, "/.well-known/oauth-authorization-server", nil)
	rec := httptest.NewRecorder()

	OAuthMiddleware(runtime, okHandler()).ServeHTTP(rec, req)

	if rec.Code != http.StatusNoContent {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
}

func TestOAuthMiddlewareRequiresIssuedBearer(t *testing.T) {
	runtime := oauthRuntime(t)
	req := httptest.NewRequest(http.MethodGet, "/mcp", nil)
	rec := httptest.NewRecorder()

	OAuthMiddleware(runtime, okHandler()).ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
	if rec.Header().Get("WWW-Authenticate") == "" {
		t.Fatal("missing WWW-Authenticate header")
	}

	token := issueAccessToken(t, runtime)
	req = httptest.NewRequest(http.MethodGet, "/mcp", nil)
	req.Header.Set("Authorization", "Bearer "+token)
	rec = httptest.NewRecorder()
	OAuthMiddleware(runtime, okHandler()).ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("unexpected status: %d", rec.Code)
	}
}

func okHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	})
}

func oauthRuntime(t *testing.T) OAuthRuntime {
	t.Helper()
	store, err := oauth.Open(t.TempDir() + "/oauth.db")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return OAuthRuntime{
		Config: oauth.Config{PublicURL: "https://shuttle.example.test", AdminToken: "admin-token"},
		Store:  store,
	}
}

func issueAccessToken(t *testing.T, runtime OAuthRuntime) string {
	t.Helper()
	ctx := context.Background()
	verifier := "abc123abc123abc123abc123abc123abc123abc123abc123"
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	client, err := runtime.Store.RegisterClient(ctx, oauth.RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	code, err := runtime.Store.CreateCode(ctx, oauth.AuthorizeRequest{
		ResponseType:        "code",
		ClientID:            client.ClientID,
		RedirectURI:         "https://client.example.test/callback",
		Scope:               oauth.ScopeMCP,
		CodeChallenge:       challenge,
		CodeChallengeMethod: "S256",
	})
	if err != nil {
		t.Fatal(err)
	}
	token, err := runtime.Store.ExchangeCode(ctx, oauth.TokenRequest{
		GrantType:    "authorization_code",
		ClientID:     client.ClientID,
		RedirectURI:  "https://client.example.test/callback",
		Code:         code,
		CodeVerifier: verifier,
	})
	if err != nil {
		t.Fatal(err)
	}
	return token.AccessToken
}
