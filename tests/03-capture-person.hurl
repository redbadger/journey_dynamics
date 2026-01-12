# Capture person (contact) data
# Run: hurl --variable journey_id=YOUR_UUID 03-capture-person.hurl

POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "name": "Alice Johnson",
        "email": "alice@example.com",
        "phone": "+1-555-0123"
    }
}

HTTP 204
