# Create a new journey
# Server auto-generates UUID when body is empty
# Run: hurl 01-create-journey.hurl

POST http://localhost:3030/journeys

HTTP 201
[Captures]
journey_id: header "Location" regex "/journeys/([a-f0-9-]+)"
[Asserts]
header "Location" matches "/journeys/[a-f0-9-]+"
