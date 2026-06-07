package api

import (
	"encoding/json"
	"html"
	"net/http"
	"strings"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/auth"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/mcp"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/oauth"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/router"
)

type Server struct {
	service *router.Service
	oauth   *auth.OAuthRuntime
}

func NewServer(service *router.Service) *Server {
	return &Server{service: service}
}

func NewServerWithOAuth(service *router.Service, oauthRuntime auth.OAuthRuntime) *Server {
	return &Server{service: service, oauth: &oauthRuntime}
}

func (s *Server) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/projects", s.projects)
	mux.HandleFunc("GET /api/projects/current", s.currentProject)
	mux.HandleFunc("POST /api/projects/use", s.useProject)
	mux.HandleFunc("POST /api/recall", s.recall)
	mux.HandleFunc("POST /api/remember", s.remember)
	mux.HandleFunc("GET /api/context", s.context)
	mux.HandleFunc("GET /api/tasks", s.tasks)
	mux.HandleFunc("POST /api/tasks", s.createTask)
	mux.HandleFunc("PATCH /api/tasks/", s.updateTask)
	mux.HandleFunc("POST /api/tasks/", s.doneTask)
	mux.HandleFunc("POST /mcp", mcp.NewServer(s.service).Handle)
	mux.HandleFunc("GET /mcp", func(w http.ResponseWriter, _ *http.Request) {
		_ = writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	mux.HandleFunc("GET /.well-known/oauth-protected-resource", s.oauthProtectedResource)
	mux.HandleFunc("GET /.well-known/oauth-protected-resource/mcp", s.oauthProtectedResource)
	mux.HandleFunc("GET /.well-known/oauth-authorization-server", s.oauthAuthorizationServer)
	mux.HandleFunc("POST /oauth/register", s.oauthRegister)
	mux.HandleFunc("GET /oauth/authorize", s.oauthAuthorizePage)
	mux.HandleFunc("POST /oauth/authorize", s.oauthAuthorizeSubmit)
	mux.HandleFunc("POST /oauth/token", s.oauthToken)
	return mux
}

func (s *Server) projects(w http.ResponseWriter, _ *http.Request) {
	_ = writeJSON(w, http.StatusOK, map[string]any{"projects": s.service.ListProjects()})
}

func (s *Server) currentProject(w http.ResponseWriter, _ *http.Request) {
	p, ok := s.service.CurrentProject()
	if !ok {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "no current or default project"})
		return
	}
	_ = writeJSON(w, http.StatusOK, p)
}

func (s *Server) useProject(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
	}
	if !decode(w, r, &req) {
		return
	}
	p, err := s.service.UseProject(req.Project)
	if err != nil {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": err.Error()})
		return
	}
	_ = writeJSON(w, http.StatusOK, p)
}

func (s *Server) recall(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Query   string `json:"query"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Query == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "query is required"})
		return
	}
	response, err := s.service.Recall(r.Context(), req.Project, req.Query)
	s.respond(w, response, err)
}

func (s *Server) remember(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Kind    string `json:"kind"`
		Text    string `json:"text"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Text == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "text is required"})
		return
	}
	response, err := s.service.Remember(r.Context(), req.Project, req.Kind, req.Text)
	s.respond(w, response, err)
}

func (s *Server) context(w http.ResponseWriter, r *http.Request) {
	response, err := s.service.Context(r.Context(), r.URL.Query().Get("project"))
	s.respond(w, response, err)
}

func (s *Server) tasks(w http.ResponseWriter, r *http.Request) {
	response, err := s.service.TaskList(r.Context(), r.URL.Query().Get("project"))
	s.respond(w, response, err)
}

func (s *Server) createTask(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Project string `json:"project"`
		Title   string `json:"title"`
		Body    string `json:"body"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Title == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "title is required"})
		return
	}
	response, err := s.service.TaskCreate(r.Context(), req.Project, req.Title, req.Body)
	s.respond(w, response, err)
}

func (s *Server) updateTask(w http.ResponseWriter, r *http.Request) {
	id := strings.TrimPrefix(r.URL.Path, "/api/tasks/")
	if id == "" || strings.Contains(id, "/") {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
		return
	}
	var req struct {
		Project string `json:"project"`
		Text    string `json:"text"`
	}
	if !decode(w, r, &req) {
		return
	}
	if req.Text == "" {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "text is required"})
		return
	}
	response, err := s.service.TaskUpdate(r.Context(), req.Project, id, req.Text)
	s.respond(w, response, err)
}

func (s *Server) doneTask(w http.ResponseWriter, r *http.Request) {
	path := strings.TrimPrefix(r.URL.Path, "/api/tasks/")
	id, ok := strings.CutSuffix(path, "/done")
	if !ok || id == "" {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
		return
	}
	var req struct {
		Project string `json:"project"`
	}
	if !decode(w, r, &req) {
		return
	}
	response, err := s.service.TaskDone(r.Context(), req.Project, id)
	s.respond(w, response, err)
}

func (s *Server) respond(w http.ResponseWriter, response router.Response, err error) {
	if err != nil {
		body := map[string]any{"error": err.Error()}
		if response.Project != "" {
			body["project"] = response.Project
		}
		_ = writeJSON(w, http.StatusBadRequest, body)
		return
	}
	_ = writeJSON(w, http.StatusOK, response)
}

func decode(w http.ResponseWriter, r *http.Request, v any) bool {
	if err := json.NewDecoder(r.Body).Decode(v); err != nil {
		_ = writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid json"})
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, v any) error {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	return json.NewEncoder(w).Encode(v)
}

func (s *Server) oauthProtectedResource(w http.ResponseWriter, _ *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	_ = writeJSON(w, http.StatusOK, oauth.ProtectedResourceMetadata(s.oauth.Config))
}

func (s *Server) oauthAuthorizationServer(w http.ResponseWriter, _ *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	_ = writeJSON(w, http.StatusOK, oauth.AuthorizationServerMetadata(s.oauth.Config))
}

func (s *Server) oauthRegister(w http.ResponseWriter, r *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	var req oauth.RegisterRequest
	if !decode(w, r, &req) {
		return
	}
	client, err := s.oauth.Store.RegisterClient(r.Context(), req)
	if err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_request", err.Error())
		return
	}
	body := map[string]any{
		"client_id":                  client.ClientID,
		"client_secret":              client.ClientSecret,
		"redirect_uris":              client.RedirectURIs,
		"client_name":                client.ClientName,
		"token_endpoint_auth_method": "none",
	}
	_ = writeJSON(w, http.StatusCreated, body)
}

func (s *Server) oauthAuthorizePage(w http.ResponseWriter, r *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	req := authorizeRequestFromValues(r.URL.Query())
	ok, err := s.oauth.Store.ClientAllowsRedirect(r.Context(), req.ClientID, req.RedirectURI)
	if err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_request", err.Error())
		return
	}
	if !ok {
		oauthError(w, http.StatusBadRequest, "invalid_request", "unknown client_id or redirect_uri")
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte(authorizeHTML(req)))
}

func (s *Server) oauthAuthorizeSubmit(w http.ResponseWriter, r *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	if err := r.ParseForm(); err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_request", "invalid form")
		return
	}
	if !auth.CheckAdminToken(r.PostForm.Get("admin_token"), s.oauth.Config.AdminToken) {
		oauthError(w, http.StatusUnauthorized, "access_denied", "invalid admin token")
		return
	}
	req := authorizeRequestFromValues(r.PostForm)
	code, err := s.oauth.Store.CreateCode(r.Context(), req)
	if err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_request", err.Error())
		return
	}
	http.Redirect(w, r, oauth.AuthorizeRedirect(req.RedirectURI, code, req.State), http.StatusSeeOther)
}

func (s *Server) oauthToken(w http.ResponseWriter, r *http.Request) {
	if s.oauth == nil {
		_ = writeJSON(w, http.StatusNotFound, map[string]string{"error": "oauth is not configured"})
		return
	}
	if err := r.ParseForm(); err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_request", "invalid form")
		return
	}
	token, err := s.oauth.Store.ExchangeCode(r.Context(), oauth.TokenRequest{
		GrantType:    r.PostForm.Get("grant_type"),
		ClientID:     r.PostForm.Get("client_id"),
		RedirectURI:  r.PostForm.Get("redirect_uri"),
		Code:         r.PostForm.Get("code"),
		CodeVerifier: r.PostForm.Get("code_verifier"),
	})
	if err != nil {
		oauthError(w, http.StatusBadRequest, "invalid_grant", err.Error())
		return
	}
	_ = writeJSON(w, http.StatusOK, token)
}

func authorizeRequestFromValues(values map[string][]string) oauth.AuthorizeRequest {
	get := func(key string) string {
		if values == nil || len(values[key]) == 0 {
			return ""
		}
		return values[key][0]
	}
	return oauth.AuthorizeRequest{
		ResponseType:        get("response_type"),
		ClientID:            get("client_id"),
		RedirectURI:         get("redirect_uri"),
		State:               get("state"),
		Scope:               get("scope"),
		CodeChallenge:       get("code_challenge"),
		CodeChallengeMethod: get("code_challenge_method"),
	}
}

func authorizeHTML(req oauth.AuthorizeRequest) string {
	return `<!doctype html>
<html>
<head><meta charset="utf-8"><title>Authorize Shuttle Gateway</title></head>
<body>
<main>
<h1>Authorize Shuttle Gateway</h1>
<p>Client ` + html.EscapeString(req.ClientID) + ` is requesting ` + html.EscapeString(scopeOrDefault(req.Scope)) + ` access.</p>
<form method="post" action="/oauth/authorize">
<label>Admin token <input name="admin_token" type="password" autocomplete="current-password" required></label>
` + hidden("response_type", req.ResponseType) +
		hidden("client_id", req.ClientID) +
		hidden("redirect_uri", req.RedirectURI) +
		hidden("state", req.State) +
		hidden("scope", req.Scope) +
		hidden("code_challenge", req.CodeChallenge) +
		hidden("code_challenge_method", req.CodeChallengeMethod) + `
<button type="submit">Authorize</button>
</form>
</main>
</body>
</html>`
}

func hidden(name, value string) string {
	return `<input type="hidden" name="` + html.EscapeString(name) + `" value="` + html.EscapeString(value) + `">`
}

func scopeOrDefault(scope string) string {
	if scope == "" {
		return oauth.ScopeMCP
	}
	return scope
}

func oauthError(w http.ResponseWriter, status int, code, description string) {
	_ = writeJSON(w, status, map[string]string{
		"error":             code,
		"error_description": description,
	})
}
