package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadRejectsRelativeRepo(t *testing.T) {
	path := writeConfig(t, `
[projects.demo]
repo = "relative/path"
`)
	_, err := Load(path)
	if err == nil {
		t.Fatal("expected relative repo to be rejected")
	}
}

func TestLoadRejectsUnknownDefault(t *testing.T) {
	path := writeConfig(t, `
[defaults]
project = "missing"

[projects.demo]
repo = "/tmp/demo"
`)
	_, err := Load(path)
	if err == nil {
		t.Fatal("expected unknown default to be rejected")
	}
}

func TestLoadAppliesDefaults(t *testing.T) {
	path := writeConfig(t, `
[projects.demo]
repo = "/tmp/demo"
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Server.Addr != "127.0.0.1:8787" {
		t.Fatalf("unexpected addr: %q", cfg.Server.Addr)
	}
	if cfg.Auth.BearerTokenEnv != "SHUTTLE_GATEWAY_TOKEN" {
		t.Fatalf("unexpected token env: %q", cfg.Auth.BearerTokenEnv)
	}
	if cfg.OAuth.AdminTokenEnv != "SHUTTLE_OAUTH_ADMIN_TOKEN" {
		t.Fatalf("unexpected admin token env: %q", cfg.OAuth.AdminTokenEnv)
	}
}

func TestLoadNormalizesOAuthDefaults(t *testing.T) {
	path := writeConfig(t, `
[oauth]
public_url = "https://shuttle.example.test/"

[projects.demo]
repo = "/tmp/demo"
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.OAuth.PublicURL != "https://shuttle.example.test" {
		t.Fatalf("unexpected public url: %q", cfg.OAuth.PublicURL)
	}
	if cfg.OAuth.DBPath != filepath.Join(filepath.Dir(path), "gateway-oauth.db") {
		t.Fatalf("unexpected oauth db path: %q", cfg.OAuth.DBPath)
	}
}

func TestLoadRejectsRelativeOAuthDBPath(t *testing.T) {
	path := writeConfig(t, `
[oauth]
public_url = "https://shuttle.example.test"
db_path = "relative.db"

[projects.demo]
repo = "/tmp/demo"
`)
	_, err := Load(path)
	if err == nil {
		t.Fatal("expected relative oauth db_path to be rejected")
	}
}

func writeConfig(t *testing.T, contents string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "projects.toml")
	if err := os.WriteFile(path, []byte(contents), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}
