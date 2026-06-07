package project

import "testing"

func TestResolveRequiresProjectForWrites(t *testing.T) {
	registry, err := NewRegistry("demo", map[string]Config{
		"demo": {Repo: "/tmp/demo"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := registry.Resolve("", true); err == nil {
		t.Fatal("expected write without project to fail")
	}
}

func TestResolveUsesDefaultForReads(t *testing.T) {
	registry, err := NewRegistry("demo", map[string]Config{
		"demo": {Repo: "/tmp/demo"},
	})
	if err != nil {
		t.Fatal(err)
	}
	p, err := registry.Resolve("", false)
	if err != nil {
		t.Fatal(err)
	}
	if p.Name != "demo" {
		t.Fatalf("unexpected project: %q", p.Name)
	}
}
