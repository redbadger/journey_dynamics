build: generate
    cargo build

test:
    cargo insta test --review --test-runner nextest --all-features --lib

generate:
    just examples/flight-booking/generate
