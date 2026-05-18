set dotenv-load

version := `grep '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/'`

build: generate
    cargo build

generate:
    just examples/flight-booking/generate

lint:
    cargo fmt --all --check
    cargo clippy -- --no-deps -Dclippy::pedantic -Dclippy::nursery -Dwarnings

test-lib:
    cargo insta test --review --test-runner nextest --all-features --lib

test:
    cargo nextest run --all-features
    cargo test --doc --all-features --workspace --exclude cqrs-es-crypto-derive

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
