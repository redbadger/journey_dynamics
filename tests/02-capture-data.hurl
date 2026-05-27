# Capture step data on an existing journey
# Run: hurl --variable host=http://localhost:3030 --variable journey_id=YOUR_UUID 02-capture-data.hurl

POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "search": {
            "tripType": "round-trip",
            "origin": "JFK",
            "destination": "LAX",
            "departureDate": "2024-03-15",
            "returnDate": "2024-03-22",
            "passengers": {
                "adults": 2,
                "children": 1,
                "infants": 0
            }
        }
    }
}

HTTP 204
