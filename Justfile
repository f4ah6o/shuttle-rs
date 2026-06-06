# Release new version (tag + push)

release-check:
    cargo test --package shuttle-rs --all-targets
    cargo build --release --package shuttle-rs
    cargo publish --package shuttle-rs --dry-run

release: release-check
    version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "shuttle-rs") | .version'); \
    git tag "v${version}"; \
    git push origin "v${version}"
