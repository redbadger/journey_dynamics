build: generate
    cargo build

generate:
    just examples/flight-booking/generate

lint:
    cargo fmt --all --check
    cargo clippy -- --no-deps -Dclippy::pedantic -Dwarnings

test-lib:
    cargo insta test --review --test-runner nextest --all-features --lib

test:
    cargo nextest run --all-features
    cargo test --doc --all-features

ci: lint build test
