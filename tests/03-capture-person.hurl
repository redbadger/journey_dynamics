# Capture person (contact) data
# Run: hurl --variable host=http://localhost:3030 --variable journey_id=YOUR_UUID 03-capture-person.hurl

POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "lead_booker",
        "subject_id": "{{newUuid}}",
        "name": "Alice Johnson",
        "email": "alice@example.com",
        "phone": "+1-555-0123"
    }
}

HTTP 204
