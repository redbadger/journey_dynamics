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

# Assumes the server is already running on localhost:3030.
# Files 01-05 are step-by-step tutorial examples that require a manual
# --variable journey_id=<uuid> and are not run here.
test-hurl:
    hurl --variable host=http://localhost:3030 --test \
        tests/error-cases.hurl \
        tests/full-flight-booking.hurl \
        tests/full-flight-booking_with_shredding.hurl

ci: lint build test test-hurl
