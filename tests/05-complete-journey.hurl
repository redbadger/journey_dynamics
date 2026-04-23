# Complete a journey
# Run: hurl --variable host=http://localhost::3030 --variable journey_id=YOUR_UUID 05-complete-journey.hurl

POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204
