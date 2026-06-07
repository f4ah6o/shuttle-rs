package oauth

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/base64"
	"strings"
	"testing"
	"time"
)

func TestMetadataUsesPublicURL(t *testing.T) {
	config := Config{PublicURL: "https://shuttle.example.test"}

	if got := ProtectedResourceMetadata(config)["resource"]; got != "https://shuttle.example.test/mcp" {
		t.Fatalf("unexpected resource: %v", got)
	}
	if got := AuthorizationServerMetadata(config)["token_endpoint"]; got != "https://shuttle.example.test/oauth/token" {
		t.Fatalf("unexpected token endpoint: %v", got)
	}
}

func TestAuthorizeRedirectEncodesState(t *testing.T) {
	url := AuthorizeRedirect(
		"https://claude.ai/api/mcp/auth_callback",
		"stl_abc123",
		"opaque=value+with/special&fragment#part",
	)
	want := "https://claude.ai/api/mcp/auth_callback?code=stl_abc123&state=opaque%3Dvalue%2Bwith%2Fspecial%26fragment%23part"
	if url != want {
		t.Fatalf("unexpected redirect:\n got: %s\nwant: %s", url, want)
	}
}

func TestRegisterRejectsEmptyRedirectURIs(t *testing.T) {
	store := newStore(t)
	_, err := store.RegisterClient(context.Background(), RegisterRequest{})
	if err == nil {
		t.Fatal("expected error")
	}
}

func TestCodeExchangeValidatesPKCEAndSingleUse(t *testing.T) {
	ctx := context.Background()
	store := newStore(t)
	client, err := store.RegisterClient(ctx, RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	verifier := "abc123abc123abc123abc123abc123abc123abc123abc123"
	code, err := store.CreateCode(ctx, AuthorizeRequest{
		ResponseType:        "code",
		ClientID:            client.ClientID,
		RedirectURI:         "https://client.example.test/callback",
		Scope:               ScopeMCP,
		CodeChallenge:       challenge(verifier),
		CodeChallengeMethod: "S256",
	})
	if err != nil {
		t.Fatal(err)
	}
	token, err := store.ExchangeCode(ctx, TokenRequest{
		GrantType:    "authorization_code",
		ClientID:     client.ClientID,
		RedirectURI:  "https://client.example.test/callback",
		Code:         code,
		CodeVerifier: verifier,
	})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.EqualFold(token.TokenType, "Bearer") {
		t.Fatalf("unexpected token type: %s", token.TokenType)
	}
	valid, err := store.ValidateAccessToken(ctx, token.AccessToken)
	if err != nil {
		t.Fatal(err)
	}
	if !valid {
		t.Fatal("expected token to validate")
	}
	_, err = store.ExchangeCode(ctx, TokenRequest{
		GrantType:    "authorization_code",
		ClientID:     client.ClientID,
		RedirectURI:  "https://client.example.test/callback",
		Code:         code,
		CodeVerifier: verifier,
	})
	if err == nil {
		t.Fatal("expected reused code to fail")
	}
}

func TestValidateAccessTokenRejectsExpiredToken(t *testing.T) {
	ctx := context.Background()
	store := newStore(t)
	_, err := store.db.ExecContext(ctx, `
INSERT INTO oauth_tokens (token, client_id, scope, expires_at, created_at)
VALUES ('expired', 'client', 'mcp', ?, ?)`,
		time.Now().UTC().Add(-time.Minute).Format(time.RFC3339),
		time.Now().UTC().Add(-time.Hour).Format(time.RFC3339),
	)
	if err != nil {
		t.Fatal(err)
	}
	valid, err := store.ValidateAccessToken(ctx, "expired")
	if err != nil {
		t.Fatal(err)
	}
	if valid {
		t.Fatal("expected expired token to be rejected")
	}
}

func TestExchangeCodeRollsBackCodeUseWhenTokenCreationFails(t *testing.T) {
	ctx := context.Background()
	store := newStore(t)
	client, code, verifier := issueCode(t, store)
	if _, err := store.db.ExecContext(ctx, `DROP TABLE oauth_tokens`); err != nil {
		t.Fatal(err)
	}
	_, err := store.ExchangeCode(ctx, TokenRequest{
		GrantType:    "authorization_code",
		ClientID:     client.ClientID,
		RedirectURI:  "https://client.example.test/callback",
		Code:         code,
		CodeVerifier: verifier,
	})
	if err == nil {
		t.Fatal("expected token creation failure")
	}
	var usedAt sql.NullString
	if err := store.db.QueryRowContext(ctx, `SELECT used_at FROM oauth_codes WHERE code = ?`, code).Scan(&usedAt); err != nil {
		t.Fatal(err)
	}
	if usedAt.Valid {
		t.Fatal("expected code use to roll back after token creation failure")
	}
}

func TestCreateCodeRejectsUnknownRedirectAndNonS256(t *testing.T) {
	ctx := context.Background()
	store := newStore(t)
	client, err := store.RegisterClient(ctx, RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	_, err = store.CreateCode(ctx, AuthorizeRequest{
		ResponseType:        "code",
		ClientID:            client.ClientID,
		RedirectURI:         "https://client.example.test/callback/",
		CodeChallenge:       "challenge",
		CodeChallengeMethod: "S256",
	})
	if err == nil {
		t.Fatal("expected redirect mismatch to fail")
	}
	_, err = store.CreateCode(ctx, AuthorizeRequest{
		ResponseType:        "code",
		ClientID:            client.ClientID,
		RedirectURI:         "https://client.example.test/callback",
		CodeChallenge:       "challenge",
		CodeChallengeMethod: "plain",
	})
	if err == nil {
		t.Fatal("expected non-S256 challenge to fail")
	}
}

func newStore(t *testing.T) *Store {
	t.Helper()
	store, err := Open(t.TempDir() + "/oauth.db")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store
}

func issueCode(t *testing.T, store *Store) (RegisteredClient, string, string) {
	t.Helper()
	ctx := context.Background()
	client, err := store.RegisterClient(ctx, RegisterRequest{
		RedirectURIs: []string{"https://client.example.test/callback"},
	})
	if err != nil {
		t.Fatal(err)
	}
	verifier := "abc123abc123abc123abc123abc123abc123abc123abc123"
	code, err := store.CreateCode(ctx, AuthorizeRequest{
		ResponseType:        "code",
		ClientID:            client.ClientID,
		RedirectURI:         "https://client.example.test/callback",
		Scope:               ScopeMCP,
		CodeChallenge:       challenge(verifier),
		CodeChallengeMethod: "S256",
	})
	if err != nil {
		t.Fatal(err)
	}
	return client, code, verifier
}

func challenge(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}
