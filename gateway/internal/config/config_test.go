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
}

func writeConfig(t *testing.T, contents string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "projects.toml")
	if err := os.WriteFile(path, []byte(contents), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}
