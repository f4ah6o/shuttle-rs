package auth

import (
	"crypto/subtle"
	"net/http"
	"os"
	"strings"
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
