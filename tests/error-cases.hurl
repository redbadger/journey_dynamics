# Error Cases - Test error handling
# Run: hurl --test error-cases.hurl

# ===========================================
# Error: Query non-existent journey
# ===========================================
GET {{host}}/journeys/00000000-0000-0000-0000-000000000000

HTTP 404


# ===========================================
# Error: SetAttributes on non-existent journey
# ===========================================
POST {{host}}/journeys/00000000-0000-0000-0000-000000000000
Content-Type: application/json
{
    "SetAttributes": {
        "search": {
            "tripType": "one-way",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-01",
            "passengers": {
                "adults": 1,
                "children": 0,
                "infants": 0
            }
        }
    }
}

HTTP 400
[Asserts]
body contains "not found"


# ===========================================
# Error: Complete non-existent journey
# ===========================================
POST {{host}}/journeys/00000000-0000-0000-0000-000000000000
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
POST {{host}}/journeys/00000000-0000-0000-0000-000000000000
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "lead_booker",
        "subject_id": "{{newUuid}}",
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
POST {{host}}/journeys/00000000-0000-0000-0000-000000000001
Content-Type: application/json
{
    "InvalidCommand": {}
}

HTTP 400
