# Complete Flight Booking Journey with GDPR Crypto-Shredding
#
# Demonstrates the key property of the multi-subject design: shredding one
# subject's DEK destroys only that subject's PII.  All shared journey data
# (search criteria, flight selections, pricing) survives completely intact.
#
# Run: hurl --variable host=http://localhost:3030 --test full-flight-booking_with_shredding.hurl

# ===========================================
# Step 1: Create a new journey
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
# Step 6: Capture lead booker identity (PII — encrypted at rest)
# Capture the subject_id so we can shred it later.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
[Options]
variable: subject_id={{newUuid}}
{
    "CapturePerson": {
        "person_ref": "passenger_0",
        "subject_id": "{{subject_id}}",
        "name": "Alice Johnson",
        "email": "alice.johnson@email.com",
        "phone": "+1-555-123-4567"
    }
}

HTTP 204


# ===========================================
# Step 7: Capture lead booker PII details (encrypted at rest)
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
# Step 8: Capture passenger 1 identity (PII — encrypted under a different DEK)
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
# Step 10: Capture passenger 2 identity (PII — encrypted under a different DEK)
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
# Actual PII details live in encrypted person slots — never in shared_data.
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
# Step 17: Verify final state before shredding
# All shared_data is visible; PII is in encrypted person slots.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"


# ===========================================
# Step 18: GDPR erasure — shred passenger_0's DEK
# This permanently destroys Alice's PersonCaptured and PersonDetailsUpdated
# event payloads.  All other passengers' data and all shared_data survive.
# ===========================================
DELETE {{host}}/subjects/{{subject_id}}

HTTP 204


# ===========================================
# Step 19: Verify journey after shredding
#
# KEY ASSERTION: shared_data is completely intact.
# Search criteria, flight selections, pricing, and booking reference all
# survive because they were never PII and were never encrypted.
#
# Alice's person slot (passenger_0) is now a tombstone in journey_person
# (forgotten = true, all fields nulled).  Bob and Charlie's slots are
# unaffected — their DEKs were not deleted.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"
