package oauth

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const ScopeMCP = "mcp"

type Config struct {
	PublicURL  string
	AdminToken string
}

func NormalizePublicURL(publicURL string) string {
	return strings.TrimRight(strings.TrimSpace(publicURL), "/")
}

func (c Config) ResourceURL() string {
	return c.PublicURL + "/mcp"
}

type Store struct {
	db *sql.DB
}

func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	store := &Store{db: db}
	if err := store.init(context.Background()); err != nil {
		_ = db.Close()
		return nil, err
	}
	return store, nil
}

func (s *Store) Close() error {
	return s.db.Close()
}

func (s *Store) init(ctx context.Context) error {
	_, err := s.db.ExecContext(ctx, `
CREATE TABLE IF NOT EXISTS oauth_clients (
	client_id TEXT PRIMARY KEY NOT NULL,
	client_secret TEXT,
	redirect_uris TEXT NOT NULL,
	client_name TEXT,
	created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_codes (
	code TEXT PRIMARY KEY NOT NULL,
	client_id TEXT NOT NULL,
	redirect_uri TEXT NOT NULL,
	code_challenge TEXT NOT NULL,
	code_challenge_method TEXT NOT NULL,
	scope TEXT NOT NULL,
	expires_at TEXT NOT NULL,
	used_at TEXT,
	created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_tokens (
	token TEXT PRIMARY KEY NOT NULL,
	client_id TEXT NOT NULL,
	scope TEXT NOT NULL,
	expires_at TEXT NOT NULL,
	created_at TEXT NOT NULL
);
`)
	if err != nil {
		return err
	}
	return s.purgeExpired(ctx)
}

type RegisterRequest struct {
	RedirectURIs []string `json:"redirect_uris"`
	ClientName   *string  `json:"client_name,omitempty"`
}

type RegisteredClient struct {
	ClientID     string   `json:"client_id"`
	ClientSecret *string  `json:"client_secret"`
	RedirectURIs []string `json:"redirect_uris"`
	ClientName   *string  `json:"client_name,omitempty"`
}

func (s *Store) RegisterClient(ctx context.Context, req RegisterRequest) (RegisteredClient, error) {
	if len(req.RedirectURIs) == 0 {
		return RegisteredClient{}, errors.New("redirect_uris must contain at least one URI")
	}
	client := RegisteredClient{
		ClientID:     token(),
		RedirectURIs: req.RedirectURIs,
		ClientName:   req.ClientName,
	}
	redirectURIs, err := json.Marshal(client.RedirectURIs)
	if err != nil {
		return RegisteredClient{}, err
	}
	_, err = s.db.ExecContext(ctx, `
INSERT INTO oauth_clients (client_id, client_secret, redirect_uris, client_name, created_at)
VALUES (?, NULL, ?, ?, ?)`,
		client.ClientID, string(redirectURIs), nullableString(client.ClientName), time.Now().UTC().Format(time.RFC3339),
	)
	if err != nil {
		return RegisteredClient{}, err
	}
	return client, nil
}

type AuthorizeRequest struct {
	ResponseType        string
	ClientID            string
	RedirectURI         string
	State               string
	Scope               string
	CodeChallenge       string
	CodeChallengeMethod string
}

func (s *Store) ClientAllowsRedirect(ctx context.Context, clientID, redirectURI string) (bool, error) {
	var raw string
	err := s.db.QueryRowContext(ctx, `SELECT redirect_uris FROM oauth_clients WHERE client_id = ?`, clientID).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	var redirectURIs []string
	if err := json.Unmarshal([]byte(raw), &redirectURIs); err != nil {
		return false, err
	}
	for _, candidate := range redirectURIs {
		if candidate == redirectURI {
			return true, nil
		}
	}
	return false, nil
}

func (s *Store) CreateCode(ctx context.Context, req AuthorizeRequest) (string, error) {
	if req.ResponseType != "code" {
		return "", errors.New("response_type must be code")
	}
	ok, err := s.ClientAllowsRedirect(ctx, req.ClientID, req.RedirectURI)
	if err != nil {
		return "", err
	}
	if !ok {
		return "", errors.New("unknown client_id or redirect_uri")
	}
	if req.CodeChallengeMethod != "S256" {
		return "", errors.New("code_challenge_method must be S256")
	}
	if req.CodeChallenge == "" {
		return "", errors.New("missing code_challenge")
	}
	scope := normalizeScope(req.Scope)
	code := token()
	now := time.Now().UTC()
	_, err = s.db.ExecContext(ctx, `
INSERT INTO oauth_codes (
	code, client_id, redirect_uri, code_challenge, code_challenge_method,
	scope, expires_at, created_at
) VALUES (?, ?, ?, ?, 'S256', ?, ?, ?)`,
		code, req.ClientID, req.RedirectURI, req.CodeChallenge, scope,
		now.Add(10*time.Minute).Format(time.RFC3339), now.Format(time.RFC3339),
	)
	if err != nil {
		return "", err
	}
	return code, nil
}

type TokenRequest struct {
	GrantType    string
	ClientID     string
	RedirectURI  string
	Code         string
	CodeVerifier string
}

type TokenResponse struct {
	AccessToken string `json:"access_token"`
	TokenType   string `json:"token_type"`
	ExpiresIn   int64  `json:"expires_in"`
	Scope       string `json:"scope"`
}

func (s *Store) ExchangeCode(ctx context.Context, req TokenRequest) (TokenResponse, error) {
	if req.GrantType != "authorization_code" {
		return TokenResponse{}, errors.New("grant_type must be authorization_code")
	}
	if req.Code == "" {
		return TokenResponse{}, errors.New("missing code")
	}
	if req.CodeVerifier == "" {
		return TokenResponse{}, errors.New("missing code_verifier")
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return TokenResponse{}, err
	}
	committed := false
	defer func() {
		if !committed {
			_ = tx.Rollback()
		}
	}()
	var stored struct {
		ClientID      string
		RedirectURI   string
		CodeChallenge string
		Scope         string
		ExpiresAt     string
	}
	err = tx.QueryRowContext(ctx, `
SELECT client_id, redirect_uri, code_challenge, scope, expires_at
FROM oauth_codes WHERE code = ? AND used_at IS NULL`, req.Code).Scan(
		&stored.ClientID, &stored.RedirectURI, &stored.CodeChallenge,
		&stored.Scope, &stored.ExpiresAt,
	)
	if errors.Is(err, sql.ErrNoRows) {
		var exists int
		existsErr := tx.QueryRowContext(ctx, `SELECT 1 FROM oauth_codes WHERE code = ?`, req.Code).Scan(&exists)
		if errors.Is(existsErr, sql.ErrNoRows) {
			return TokenResponse{}, errors.New("invalid code")
		}
		if existsErr != nil {
			return TokenResponse{}, existsErr
		}
		return TokenResponse{}, errors.New("code already used")
	}
	if err != nil {
		return TokenResponse{}, err
	}
	if stored.ClientID != req.ClientID {
		return TokenResponse{}, errors.New("invalid client_id")
	}
	if stored.RedirectURI != req.RedirectURI {
		return TokenResponse{}, errors.New("invalid redirect_uri")
	}
	expiresAt, err := time.Parse(time.RFC3339, stored.ExpiresAt)
	if err != nil {
		return TokenResponse{}, err
	}
	if time.Now().UTC().After(expiresAt) {
		return TokenResponse{}, errors.New("code expired")
	}
	if pkceS256(req.CodeVerifier) != stored.CodeChallenge {
		return TokenResponse{}, errors.New("invalid code_verifier")
	}
	result, err := tx.ExecContext(ctx, `UPDATE oauth_codes SET used_at = ? WHERE code = ? AND used_at IS NULL`, time.Now().UTC().Format(time.RFC3339), req.Code)
	if err != nil {
		return TokenResponse{}, err
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return TokenResponse{}, err
	}
	if rows != 1 {
		return TokenResponse{}, errors.New("code already used")
	}
	token, err := createToken(ctx, tx, stored.ClientID, stored.Scope)
	if err != nil {
		return TokenResponse{}, err
	}
	if err := tx.Commit(); err != nil {
		return TokenResponse{}, err
	}
	committed = true
	return token, nil
}

func (s *Store) ValidateAccessToken(ctx context.Context, bearerToken string) (bool, error) {
	var scope, expiresAtRaw string
	err := s.db.QueryRowContext(ctx, `SELECT scope, expires_at FROM oauth_tokens WHERE token = ?`, bearerToken).Scan(&scope, &expiresAtRaw)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	expiresAt, err := time.Parse(time.RFC3339, expiresAtRaw)
	if err != nil {
		return false, err
	}
	return strings.Contains(" "+scope+" ", " "+ScopeMCP+" ") && time.Now().UTC().Before(expiresAt), nil
}

type tokenCreator interface {
	ExecContext(context.Context, string, ...any) (sql.Result, error)
}

func createToken(ctx context.Context, db tokenCreator, clientID, scope string) (TokenResponse, error) {
	accessToken := token()
	now := time.Now().UTC()
	expiresIn := int64(3600)
	_, err := db.ExecContext(ctx, `
INSERT INTO oauth_tokens (token, client_id, scope, expires_at, created_at)
VALUES (?, ?, ?, ?, ?)`,
		accessToken, clientID, scope, now.Add(time.Duration(expiresIn)*time.Second).Format(time.RFC3339), now.Format(time.RFC3339),
	)
	if err != nil {
		return TokenResponse{}, err
	}
	return TokenResponse{
		AccessToken: accessToken,
		TokenType:   "Bearer",
		ExpiresIn:   expiresIn,
		Scope:       scope,
	}, nil
}

func (s *Store) createToken(ctx context.Context, clientID, scope string) (TokenResponse, error) {
	return createToken(ctx, s.db, clientID, scope)
}

func (s *Store) purgeExpired(ctx context.Context) error {
	now := time.Now().UTC().Format(time.RFC3339)
	if _, err := s.db.ExecContext(ctx, `DELETE FROM oauth_codes WHERE expires_at < ? OR used_at IS NOT NULL`, now); err != nil {
		return err
	}
	_, err := s.db.ExecContext(ctx, `DELETE FROM oauth_tokens WHERE expires_at < ?`, now)
	return err
}

func AuthorizationServerMetadata(config Config) map[string]any {
	return map[string]any{
		"issuer":                                config.PublicURL,
		"authorization_endpoint":                config.PublicURL + "/oauth/authorize",
		"token_endpoint":                        config.PublicURL + "/oauth/token",
		"registration_endpoint":                 config.PublicURL + "/oauth/register",
		"response_types_supported":              []string{"code"},
		"grant_types_supported":                 []string{"authorization_code"},
		"code_challenge_methods_supported":      []string{"S256"},
		"token_endpoint_auth_methods_supported": []string{"none"},
		"scopes_supported":                      []string{ScopeMCP},
	}
}

func ProtectedResourceMetadata(config Config) map[string]any {
	return map[string]any{
		"resource":                 config.ResourceURL(),
		"authorization_servers":    []string{config.PublicURL},
		"scopes_supported":         []string{ScopeMCP},
		"bearer_methods_supported": []string{"header"},
	}
}

func AuthorizeRedirect(redirectURI, code, state string) string {
	sep := "?"
	if strings.Contains(redirectURI, "?") {
		sep = "&"
	}
	target := redirectURI + sep + "code=" + url.QueryEscape(code)
	if state != "" {
		target += "&state=" + url.QueryEscape(state)
	}
	return target
}

func normalizeScope(scope string) string {
	if scope == "" {
		return ScopeMCP
	}
	for _, part := range strings.Fields(scope) {
		if part == ScopeMCP {
			return scope
		}
	}
	return ScopeMCP
}

func pkceS256(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func token() string {
	var bytes [16]byte
	if _, err := rand.Read(bytes[:]); err != nil {
		panic(err)
	}
	return "stl_" + hex.EncodeToString(bytes[:])
}

func nullableString(value *string) any {
	if value == nil {
		return nil
	}
	return *value
}
