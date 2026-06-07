package auth

import (
	"context"
	"crypto/subtle"
	"fmt"
	"net/http"
	"os"
	"strings"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/oauth"
)

func Middleware(tokenEnv string, next http.Handler) http.Handler {
	if tokenEnv == "" {
		tokenEnv = "SHUTTLE_GATEWAY_TOKEN"
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		token := os.Getenv(tokenEnv)
		if token == "" {
			next.ServeHTTP(w, r)
			return
		}
		got := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		if subtle.ConstantTimeCompare([]byte(got), []byte(token)) != 1 {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":"unauthorized"}`))
			return
		}
		next.ServeHTTP(w, r)
	})
}

type OAuthRuntime struct {
	Config oauth.Config
	Store  *oauth.Store
}

func OAuthMiddleware(runtime OAuthRuntime, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if IsOAuthPublicRoute(r.Method, r.URL.Path) {
			next.ServeHTTP(w, r)
			return
		}
		token, ok := bearerToken(r.Header.Get("Authorization"))
		if !ok {
			UnauthorizedOAuth(w, runtime.Config)
			return
		}
		valid, err := runtime.Store.ValidateAccessToken(r.Context(), token)
		if err != nil {
			writeAuthError(w, http.StatusUnauthorized, "invalid_token", "failed to validate access token")
			return
		}
		if !valid {
			UnauthorizedOAuth(w, runtime.Config)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func IsOAuthPublicRoute(method, path string) bool {
	switch path {
	case "/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-authorization-server":
		return method == http.MethodGet
	case "/oauth/register", "/oauth/token":
		return method == http.MethodPost
	case "/oauth/authorize":
		return method == http.MethodGet || method == http.MethodPost
	default:
		return false
	}
}

func Authorizer(tokenEnv string, oauthRuntime *OAuthRuntime) func(http.Handler) http.Handler {
	if oauthRuntime != nil {
		return func(next http.Handler) http.Handler {
			return OAuthMiddleware(*oauthRuntime, next)
		}
	}
	return func(next http.Handler) http.Handler {
		return Middleware(tokenEnv, next)
	}
}

func CheckAdminToken(got, expected string) bool {
	return subtle.ConstantTimeCompare([]byte(got), []byte(expected)) == 1
}

func UnauthorizedOAuth(w http.ResponseWriter, config oauth.Config) {
	w.Header().Set("WWW-Authenticate", fmt.Sprintf(
		`Bearer resource_metadata="%s/.well-known/oauth-protected-resource/mcp", scope="mcp"`,
		config.PublicURL,
	))
	writeAuthError(w, http.StatusUnauthorized, "unauthorized", "missing or invalid bearer token")
}

func ValidateAccessToken(ctx context.Context, runtime OAuthRuntime, authorization string) (bool, error) {
	token, ok := bearerToken(authorization)
	if !ok {
		return false, nil
	}
	return runtime.Store.ValidateAccessToken(ctx, token)
}

func bearerToken(authorization string) (string, bool) {
	scheme, token, ok := strings.Cut(authorization, " ")
	if !ok || !strings.EqualFold(scheme, "Bearer") || token == "" {
		return "", false
	}
	return token, true
}

func writeAuthError(w http.ResponseWriter, status int, code, description string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write([]byte(fmt.Sprintf(`{"error":%q,"error_description":%q}`, code, description)))
}
