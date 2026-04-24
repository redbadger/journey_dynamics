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
    "Capture": {
        "step": "search_criteria",
        "data": {
            "search": {
                "origin": "JFK",
                "destination": "LAX",
                "tripType": "round-trip",
                "departureDate": "2024-03-15",
                "returnDate": "2024-03-22",
                "passengers": {
                    "adults": 2,
                    "children": 1,
                    "infants": 0,
                    "total": 3
                }
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
jsonpath "$.current_step" == "search_criteria"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"


# ===========================================
# Step 4: Select outbound flight (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "outbound_flight_selection",
        "data": {
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
}

HTTP 204


# ===========================================
# Step 5: Select return flight (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "return_flight_selection",
        "data": {
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
}

HTTP 204


# ===========================================
# Step 6: Capture passenger 0 identity (PII — encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "passenger_0",
        "subject_id": "{{newUuid}}",
        "name": "Alice Johnson",
        "email": "alice.johnson@email.com",
        "phone": "+1-555-123-4567"
    }
}

HTTP 204


# ===========================================
# Step 7: Capture passenger 0 PII details (encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePersonDetails": {
        "person_ref": "passenger_0",
        "data": {
            "firstName": "Alice",
            "lastName": "Johnson",
            "dateOfBirth": "1985-06-15",
            "passportNumber": "AB123456",
            "passengerType": "adult"
        }
    }
}

HTTP 204


# ===========================================
# Step 8: Capture passenger 1 identity (PII — encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "passenger_1",
        "subject_id": "{{newUuid}}",
        "name": "Bob Johnson",
        "email": "bob.johnson@email.com",
        "phone": null
    }
}

HTTP 204


# ===========================================
# Step 9: Capture passenger 1 PII details (encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePersonDetails": {
        "person_ref": "passenger_1",
        "data": {
            "firstName": "Bob",
            "lastName": "Johnson",
            "dateOfBirth": "1983-09-22",
            "passportNumber": "CD789012",
            "passengerType": "adult"
        }
    }
}

HTTP 204


# ===========================================
# Step 10: Capture passenger 2 identity (PII — encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "passenger_2",
        "subject_id": "{{newUuid}}",
        "name": "Charlie Johnson",
        "email": "charlie.johnson@email.com",
        "phone": null
    }
}

HTTP 204


# ===========================================
# Step 11: Capture passenger 2 PII details (encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePersonDetails": {
        "person_ref": "passenger_2",
        "data": {
            "firstName": "Charlie",
            "lastName": "Johnson",
            "dateOfBirth": "2016-03-10",
            "passengerType": "child"
        }
    }
}

HTTP 204


# ===========================================
# Step 12: Signal passengers ready (non-PII workflow signal)
# The decision engine uses this to advance to seat selection.
# The actual PII details live in encrypted person slots — never in shared_data.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "passenger_details",
        "data": {
            "booking": {
                "passengersReady": 3
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 13: Select seats (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "seat_selection",
        "data": {
            "booking": {
                "selectedSeats": {
                    "outbound": ["12A", "12B", "12C"],
                    "return": ["14A", "14B", "14C"]
                },
                "seatUpgradeTotal": 45.00
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 14: Add extras / ancillaries (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "ancillary_services",
        "data": {
            "booking": {
                "insurance": {
                    "selected": true,
                    "insurance_type": "comprehensive",
                    "price": 45.00
                }
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 15: Payment (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "payment",
        "data": {
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
}

HTTP 204


# ===========================================
# Step 16: Complete the journey
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204


# ===========================================
# Step 17: Verify final state
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
