# Complete Flight Booking Journey
# Run: hurl --test full-flight-booking.hurl

# ===========================================
# Step 1: Create a new journey (UUID auto-generated)
# ===========================================
POST {{host}}/journeys

HTTP 201
[Captures]
journey_id: header "Location" regex "/journeys/([a-f0-9-]+)"
[Asserts]
header "Location" matches "/journeys/[a-f0-9-]+"


# ===========================================
# Step 2: Capture search criteria (non-PII shared data)
# ===========================================
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


# ===========================================
# Step 3: Verify search criteria captured
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "InProgress"
jsonpath "$.latest_workflow_decision.phase" == "selecting_outbound"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"


# ===========================================
# Step 4: Select outbound flight (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "selectedOutboundFlight": {
                "flightId": "AA1234",
                "flightNumber": "AA1234",
                "departure": "2024-03-15T08:30:00",
                "arrival": "2024-03-15T11:45:00",
                "price": 299.99,
                "airline": "American Airlines"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 5: Select return flight (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "selectedReturnFlight": {
                "flightId": "AA5678",
                "flightNumber": "AA5678",
                "departure": "2024-03-22T14:00:00",
                "arrival": "2024-03-22T22:15:00",
                "price": 279.99,
                "airline": "American Airlines"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 6: Register passenger 0 and bind to role path
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CaptureAndBindSubject": {
        "role_path": "persons/passenger_0",
        "subject_id": "{{newUuid}}",
        "email": "alice.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 7: Capture passenger 0 details
# Secret fields (firstName, lastName, dateOfBirth, passportNumber) are
# encrypted under passenger_0's DEK. passengerType lands in shared_data.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "passenger_0": {
                "firstName": "Alice",
                "lastName": "Johnson",
                "dateOfBirth": "1985-06-15",
                "passportNumber": "AB123456",
                "passengerType": "adult"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 8: Register passenger 1 and bind to role path
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CaptureAndBindSubject": {
        "role_path": "persons/passenger_1",
        "subject_id": "{{newUuid}}",
        "email": "bob.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 9: Capture passenger 1 details
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "passenger_1": {
                "firstName": "Bob",
                "lastName": "Johnson",
                "dateOfBirth": "1983-09-22",
                "passportNumber": "CD789012",
                "passengerType": "adult"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 10: Register passenger 2 and bind to role path
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CaptureAndBindSubject": {
        "role_path": "persons/passenger_2",
        "subject_id": "{{newUuid}}",
        "email": "charlie.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 11: Capture passenger 2 details
# After this step all three passengers have passengerType set in shared_data.
# The JDM reads persons.*.passengerType directly to determine passengersComplete
# — no explicit "ready" signal is needed.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "passenger_2": {
                "firstName": "Charlie",
                "lastName": "Johnson",
                "dateOfBirth": "2016-03-10",
                "passengerType": "child"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 12: Select seats (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "selectedSeats": {
                "outbound": ["12A", "12B", "12C"],
                "return": ["14A", "14B", "14C"]
            },
            "seatUpgradeTotal": 45.00
        }
    }
}

HTTP 204


# ===========================================
# Step 13: Add extras / ancillaries (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "insurance": {
                "selected": true,
                "insurance_type": "comprehensive",
                "price": 45.00
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 14: Payment (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "paymentStatus": "completed",
            "payment": {
                "method": "credit_card",
                "status": "completed",
                "transaction_id": "txn_abc123xyz"
            },
            "pricing": {
                "basePrice": 579.98,
                "taxes": 180.00,
                "totalPrice": 759.98,
                "currency": "USD"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 15: Complete the journey
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204


# ===========================================
# Step 16: Verify final state
# shared_data contains all non-PII journey data.
# PII lives in encrypted person slots (journey_person table) — not exposed here.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"
