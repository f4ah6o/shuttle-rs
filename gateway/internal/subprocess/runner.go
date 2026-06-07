package subprocess

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"time"

	"github.com/f4ah6o/shuttle-rs/gateway/internal/project"
)

type Runner interface {
	Run(ctx context.Context, project project.Project, args ...string) (json.RawMessage, error)
}

type STLRunner struct {
	Binary  string
	Timeout time.Duration
}

func (r STLRunner) Run(ctx context.Context, p project.Project, args ...string) (json.RawMessage, error) {
	timeout := r.Timeout
	if timeout == 0 {
		timeout = 10 * time.Second
	}
	binary := r.Binary
	if binary == "" {
		binary = "stl"
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	fullArgs := append([]string{"--json"}, args...)
	cmd := exec.CommandContext(ctx, binary, fullArgs...)
	cmd.Dir = p.Repo
	out, err := cmd.Output()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			return nil, fmt.Errorf("stl failed: %s", string(exitErr.Stderr))
		}
		return nil, err
	}
	return json.RawMessage(out), nil
}
