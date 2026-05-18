set dotenv-load

version := `grep '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/'`

build: generate
    cargo build

generate:
    just examples/flight-booking/generate

lint:
    cargo fmt --all --check
    cargo clippy -- --no-deps -Dclippy::pedantic -Dclippy::nursery -Dwarnings

# Run unit tests
test-unit:
    cargo nextest run --all-features --lib

# Run doc tests
test-doc:
    cargo test --doc --all-features

# Run unit and integration tests
test-all:
    cargo nextest run --all-features

# Run all tests: unit, integration, doc (requires DATABASE_URL)
test: test-all test-doc

# Snapshot review helper — lib tests only, no database required.
test-review:
    cargo insta test --review --test-runner nextest --all-features --lib

# Remove rows written by integration tests that may have leaked
clean-test-keys:
    psql "$DATABASE_URL" -c "DELETE FROM subject_encryption_keys WHERE kek_id LIKE 'test:%';"

# Assumes the server is already running on localhost:3030
test-hurl:
    hurl --variable host=http://localhost:3030 --test \
        tests/error-cases.hurl \
        tests/full-flight-booking.hurl \
        tests/full-flight-booking_with_shredding.hurl \
        tests/full-flight-booking_with_shredding_by_email.hurl

# Lint, build, test, and run hurl tests (needs a running server)
ci: lint build test test-hurl

# Publish cqrs-es-crypto-derive and cqrs-es-crypto to crates.io
publish:
    cargo publish -p cqrs-es-crypto-derive
    cargo publish -p cqrs-es-crypto
    git tag "cqrs-es-crypto-derive-v{{ version }}"
    git tag "cqrs-es-crypto-v{{ version }}"
    git push origin "cqrs-es-crypto-derive-v{{ version }}" "cqrs-es-crypto-v{{ version }}"
