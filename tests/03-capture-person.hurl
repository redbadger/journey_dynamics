# Register a subject and capture their personal details on an existing journey.
# Run: hurl --variable host=http://localhost:3030 --variable journey_id=YOUR_UUID 03-capture-person.hurl

# Step 1: Register the lead booker and bind them to the persons/lead_booker role path.
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "RegisterAndBindSubject": {
        "role_path": "persons/lead_booker",
        "subject_id": "{{newUuid}}",
        "email": "alice@example.com"
    }
}

HTTP 204


# Step 2: Set the lead booker's personal details.
# firstName, lastName, and dateOfBirth are secret fields encrypted under the
# subject's DEK. passengerType is a plaintext attribute stored in shared_data.
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "lead_booker": {
                "firstName": "Alice",
                "lastName": "Johnson",
                "dateOfBirth": "1985-06-15",
                "passengerType": "adult"
            }
        }
    }
}

HTTP 204
