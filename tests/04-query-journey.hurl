# Query a journey's current state
# Run: hurl --variable host=http://localhost::3030 --variable journey_id=YOUR_UUID 04-query-journey.hurl

GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.id" exists
jsonpath "$.state" exists
jsonpath "$.accumulated_data" exists
