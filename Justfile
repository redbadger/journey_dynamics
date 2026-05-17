set dotenv-load

version := `grep '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/'`

build: generate
    cargo build

generate:
    just examples/flight-booking/generate

lint:
    cargo fmt --all --check
    cargo clippy -- --no-deps -Dclippy::pedantic -Dclippy::nursery -Dwarnings

# Run unit tests and non-database integration tests (no DATABASE_URL required).
# --lib restricts to inline #[cfg(test)] modules; the derive integration test
# is added explicitly because it also needs no database.
test-unit:
    cargo nextest run --features testing,derive,chrono --lib
    cargo nextest run --package cqrs-es-crypto-derive
    cargo test --doc --features testing,derive,chrono --workspace --exclude cqrs-es-crypto-derive

# Run only the Postgres integration tests (requires DATABASE_URL).
test-integration:
    cargo nextest run --all-features --test postgres_key_store --test postgres_repository
    cargo nextest run --test postgres_view_repository --test postgres_subject_lookup_hook --package journey_dynamics

# Run everything: unit + integration (requires DATABASE_URL).
test: test-unit test-integration

# Snapshot review helper — lib tests only, no database required.
test-lib:
    cargo insta test --review --test-runner nextest --features testing,derive,chrono --lib

# Remove rows written by integration tests that may have leaked due to a
# mid-test panic (cleanup_key did not fire).  Safe to run while the app is
# stopped; do not run while tests are executing.
#
# kek_id values starting with "test:" are exclusively written by the Postgres
# integration tests and are never produced by the running application.
clean-test-keys:
    psql "$DATABASE_URL" -c "DELETE FROM subject_encryption_keys WHERE kek_id LIKE 'test:%';"

# Assumes the server is already running on localhost:3030.
# Files 01-05 are step-by-step tutorial examples that require a manual
# --variable journey_id=<uuid> and are not run here.
test-hurl:
    hurl --variable host=http://localhost:3030 --test \
        tests/error-cases.hurl \
        tests/full-flight-booking.hurl \
        tests/full-flight-booking_with_shredding.hurl \
        tests/full-flight-booking_with_shredding_by_email.hurl

ci: lint build test test-hurl

# Publish cqrs-es-crypto-derive and cqrs-es-crypto to crates.io.
#
# Publishing order matters: the derive crate must go first because the main
# crate optionally depends on it. The derive crate's dev-dependency on
# cqrs-es-crypto is path-only (no version), so cargo excludes it from the
# published manifest — avoiding a chicken-and-egg resolution failure.
# cargo publish waits for each crate to be available in the index before
# returning, so no sleep is needed between steps.
#
# Requires `cargo login` to have been run with a valid crates.io token.
publish:
    cargo publish -p cqrs-es-crypto-derive
    cargo publish -p cqrs-es-crypto
    git tag "cqrs-es-crypto-derive-v{{ version }}"
    git tag "cqrs-es-crypto-v{{ version }}"
    git push origin "cqrs-es-crypto-derive-v{{ version }}" "cqrs-es-crypto-v{{ version }}"
