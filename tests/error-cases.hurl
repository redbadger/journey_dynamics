# Error Cases - Test error handling
# Run: hurl --test error-cases.hurl

# ===========================================
# Error: Query non-existent journey
# ===========================================
GET http://localhost:3030/journeys/00000000-0000-0000-0000-000000000000

HTTP 404


# ===========================================
# Error: Capture on non-existent journey
# ===========================================
POST http://localhost:3030/journeys/00000000-0000-0000-0000-000000000000
Content-Type: application/json
{
    "Capture": {
        "step": "test",
        "data": {"foo": "bar"}
    }
}

HTTP 400
[Asserts]
body contains "not found"


# ===========================================
# Error: Complete non-existent journey
# ===========================================
POST http://localhost:3030/journeys/00000000-0000-0000-0000-000000000000
Content-Type: application/json
{
    "Complete": null
}

HTTP 400
[Asserts]
body contains "not found"


# ===========================================
# Error: CapturePerson on non-existent journey
# ===========================================
POST http://localhost:3030/journeys/00000000-0000-0000-0000-000000000000
Content-Type: application/json
{
    "CapturePerson": {
        "name": "Test",
        "email": "test@example.com",
        "phone": null
    }
}

HTTP 400
[Asserts]
body contains "not found"


# ===========================================
# Error: Invalid JSON command
# ===========================================
POST http://localhost:3030/journeys/00000000-0000-0000-0000-000000000001
Content-Type: application/json
{
    "InvalidCommand": {}
}

HTTP 400
