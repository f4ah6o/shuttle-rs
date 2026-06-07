package config

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
)

type Config struct {
	Server   ServerConfig              `toml:"server"`
	Auth     AuthConfig                `toml:"auth"`
	OAuth    OAuthConfig               `toml:"oauth"`
	Defaults DefaultsConfig            `toml:"defaults"`
	Projects map[string]project.Config `toml:"projects"`
}

type ServerConfig struct {
	Addr string `toml:"addr"`
}

type AuthConfig struct {
	BearerTokenEnv string `toml:"bearer_token_env"`
}

type OAuthConfig struct {
	PublicURL     string `toml:"public_url"`
	DBPath        string `toml:"db_path"`
	AdminTokenEnv string `toml:"admin_token_env"`
}

type DefaultsConfig struct {
	Project string `toml:"project"`
}

func Load(path string) (Config, error) {
	var cfg Config
	absPath, err := filepath.Abs(path)
	if err != nil {
		return Config{}, err
	}
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return Config{}, err
	}
	if cfg.Server.Addr == "" {
		cfg.Server.Addr = "127.0.0.1:8787"
	}
	if cfg.Auth.BearerTokenEnv == "" {
		cfg.Auth.BearerTokenEnv = "SHUTTLE_GATEWAY_TOKEN"
	}
	if cfg.OAuth.AdminTokenEnv == "" {
		cfg.OAuth.AdminTokenEnv = "SHUTTLE_OAUTH_ADMIN_TOKEN"
	}
	cfg.OAuth.PublicURL = strings.TrimRight(strings.TrimSpace(cfg.OAuth.PublicURL), "/")
	if cfg.OAuth.PublicURL != "" {
		if cfg.OAuth.DBPath == "" {
			cfg.OAuth.DBPath = filepath.Join(filepath.Dir(absPath), "gateway-oauth.db")
		}
		if !filepath.IsAbs(cfg.OAuth.DBPath) {
			return Config{}, fmt.Errorf("oauth db_path must be an absolute path when set")
		}
	}
	if len(cfg.Projects) == 0 {
		return Config{}, fmt.Errorf("at least one project is required")
	}
	for name, p := range cfg.Projects {
		if p.Repo == "" {
			return Config{}, fmt.Errorf("project %q repo is required", name)
		}
		if !filepath.IsAbs(p.Repo) {
			return Config{}, fmt.Errorf("project %q repo must be an absolute path", name)
		}
		if p.DB != "" && !filepath.IsAbs(p.DB) {
			return Config{}, fmt.Errorf("project %q db must be an absolute path when set", name)
		}
	}
	if cfg.Defaults.Project != "" {
		if _, ok := cfg.Projects[cfg.Defaults.Project]; !ok {
			return Config{}, fmt.Errorf("default project %q is not configured", cfg.Defaults.Project)
		}
	}
	return cfg, nil
}
