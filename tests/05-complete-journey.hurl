# Complete a journey
# Run: hurl --variable journey_id=YOUR_UUID 05-complete-journey.hurl

POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204
