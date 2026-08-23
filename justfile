# Build lib
[group('dev')]
build:
    cargo +nightly build

# Check compilation
[group('dev')]
check:
    cargo check

[group('dev')]
all-check:
    cargo check
    cargo +nightly fmt
    cargo clippy

# Run tests
[group('dev')]
test filter="":
    cargo test {{filter}}
