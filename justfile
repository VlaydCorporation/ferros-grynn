# Build lib
[group('dev')]
build:
    cargo build

# Check compilation
[group('dev')]
check:
    cargo check

# Run tests
[group('dev')]
test filter="":
    cargo test {{filter}}
