set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

toolchain := env('FG_RUST_TOOLCHAIN', 'stable')
fmt-toolchain := env('FG_RUSTFMT_TOOLCHAIN', 'nightly')

# Build the production library
[group('dev')]
build *args:
    cargo +{{toolchain}} build --locked -p ferros-grynn {{args}}

# Check compilation
[group('dev')]
check *args:
    cargo +{{toolchain}} check --locked -p ferros-grynn --all-targets {{args}}

# Format the workspace explicitly
[group('dev')]
fmt:
    cargo +{{fmt-toolchain}} fmt --all

# Check formatting without rewriting files
[group('dev')]
fmt-check:
    cargo +{{fmt-toolchain}} fmt --all -- --check

# Run the repository's Clippy rules
[group('dev')]
clippy *args:
    cargo +{{toolchain}} clippy --locked -p ferros-grynn --all-targets {{args}}

# Check compilation, formatting and Clippy without rewriting source
[group('dev')]
all-check: check fmt-check clippy

# Run tests
[group('dev')]
test *args:
    cargo +{{toolchain}} test --locked -p ferros-grynn {{args}} -- --test-threads=1

# Run the complete pre-commit gate
[group('dev')]
pre-commit:
#pre-commit: all-check test

# Install the tracked pre-commit hook without replacing other hooks
[group('dev')]
install-hooks:
    pwsh -NoLogo -NoProfile -File scripts/install-hooks.ps1

# Execute the README's Rust and GrynnQL example
[group('dev')]
quick-start:
    cargo +{{toolchain}} run --locked -p ferros-grynn --no-default-features --example quick_start

# Measure Windows core line coverage and generate report artifacts
[group('ci')]
coverage:
    pwsh -NoLogo -NoProfile -File scripts/coverage.ps1 -Toolchain {{toolchain}}

# Validate CI scripts, badge generation and documentation links
[group('ci')]
ci-check:
    pwsh -NoLogo -NoProfile -File scripts/check-ci.ps1
