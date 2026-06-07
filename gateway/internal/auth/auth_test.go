package auth

import (
	"net/http"
	"net/http/httptest"
	"testing"
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

func okHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	})
}
