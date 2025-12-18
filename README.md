### Journey Dynamics


## Setup

```bash
cargo install cargo-sqlx
cargo sqlx database create
cargo sqlx migrate run
```

## Run

```bash
cargo run
```

1. Create a new journey

```
curl -v -X POST http://localhost:3030/journeys
```

Returns 201 CREATED with a location header to let clients know where to find the created journey.

2. See created journey

```
curl -v http://localhost:3030/journeys/{id}
```
