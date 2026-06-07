package main

import (
	"flag"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"time"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/api"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/auth"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/config"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/oauth"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/router"
	"github.com/f4ah6o/shuttle-rs/gateway/internal/subprocess"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "shuttle-gateway: %v\n", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 || args[0] != "serve" {
		return fmt.Errorf("usage: shuttle-gateway serve --config <projects.toml> [--addr 127.0.0.1:8787] [--stl stl]")
	}

	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	configPath := flags.String("config", "", "project configuration TOML")
	addrOverride := flags.String("addr", "", "listen address")
	stlBinary := flags.String("stl", "stl", "stl executable")
	timeout := flags.Duration("timeout", 10*time.Second, "stl subprocess timeout")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *configPath == "" {
		return fmt.Errorf("--config is required")
	}

	cfg, err := config.Load(*configPath)
	if err != nil {
		return err
	}
	addr := cfg.Server.Addr
	if *addrOverride != "" {
		addr = *addrOverride
	}
	if addr == "" {
		addr = "127.0.0.1:8787"
	}
	if _, err := net.ResolveTCPAddr("tcp", addr); err != nil {
		return fmt.Errorf("invalid listen address %q: %w", addr, err)
	}

	registry, err := project.NewRegistry(cfg.Defaults.Project, cfg.Projects)
	if err != nil {
		return err
	}
	service := router.NewService(registry, subprocess.STLRunner{
		Binary:  *stlBinary,
		Timeout: *timeout,
	})
	var oauthRuntime *auth.OAuthRuntime
	if cfg.OAuth.PublicURL != "" {
		adminToken := os.Getenv(cfg.OAuth.AdminTokenEnv)
		if adminToken == "" {
			return fmt.Errorf("%s is required when oauth public_url is configured", cfg.OAuth.AdminTokenEnv)
		}
		store, err := oauth.Open(cfg.OAuth.DBPath)
		if err != nil {
			return fmt.Errorf("open oauth store: %w", err)
		}
		defer store.Close()
		oauthRuntime = &auth.OAuthRuntime{
			Config: oauth.Config{
				PublicURL:  oauth.NormalizePublicURL(cfg.OAuth.PublicURL),
				AdminToken: adminToken,
			},
			Store: store,
		}
	}
	apiServer := api.NewServer(service)
	if oauthRuntime != nil {
		apiServer = api.NewServerWithOAuth(service, *oauthRuntime)
	}
	handler := auth.Authorizer(cfg.Auth.BearerTokenEnv, oauthRuntime)(apiServer.Routes())
	server := &http.Server{
		Addr:              addr,
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
	}

	slog.Info("serving shuttle gateway", "addr", addr)
	return server.ListenAndServe()
}
