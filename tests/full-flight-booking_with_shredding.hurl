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
# Step 6: Register passenger 0 and bind to role path.
# Capture the subject_id so we can shred it later.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
[Options]
variable: subject_id={{newUuid}}
{
    "RegisterAndBindSubject": {
        "role_path": "/persons/passenger_0",
        "subject_id": "{{subject_id}}",
        "email": "alice.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 7: Capture lead booker details
# Secret fields encrypted under passenger_0's DEK.
# passengerType lands in shared_data.
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
    "RegisterAndBindSubject": {
        "role_path": "/persons/passenger_1",
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
    "RegisterAndBindSubject": {
        "role_path": "/persons/passenger_2",
        "subject_id": "{{newUuid}}",
        "email": "charlie.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 11: Capture passenger 2 details
# After this step all three passengers have passengerType set in shared_data.
# The JDM reads persons.*.passengerType directly — no explicit signal needed.
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
# Step 16: Verify final state before shredding
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"
# All person slots present and not yet forgotten
jsonpath "$.persons[0].person_ref" == "passenger_0"
jsonpath "$.persons[0].forgotten" == false
jsonpath "$.persons[0].email" == "alice.johnson@email.com"
jsonpath "$.persons[1].person_ref" == "passenger_1"
jsonpath "$.persons[1].forgotten" == false
jsonpath "$.persons[2].person_ref" == "passenger_2"
jsonpath "$.persons[2].forgotten" == false


# ===========================================
# Step 17: GDPR erasure — shred passenger_0's DEK
# This permanently destroys Alice's SubjectRegistered and AttributesSet
# event payloads.  All other passengers' data and all shared_data survive.
# ===========================================
DELETE {{host}}/subjects/{{subject_id}}

HTTP 204


# ===========================================
# Step 18: Verify journey after shredding
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
# ── Journey state ──────────────────────────────────────────────────────────
jsonpath "$.state" == "Complete"

# ── Shared data: search criteria (plaintext — survives shredding) ──────────
jsonpath "$.shared_data.search.tripType" == "round-trip"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.search.departureDate" == "2024-03-15"
jsonpath "$.shared_data.search.returnDate" == "2024-03-22"
jsonpath "$.shared_data.search.passengers.adults" == 2
jsonpath "$.shared_data.search.passengers.children" == 1
jsonpath "$.shared_data.search.passengers.infants" == 0

# ── Shared data: flight selections ────────────────────────────────────────
jsonpath "$.shared_data.booking.selectedOutboundFlight.flightId" == "AA1234"
jsonpath "$.shared_data.booking.selectedOutboundFlight.airline" == "American Airlines"
jsonpath "$.shared_data.booking.selectedOutboundFlight.price" == 299.99
jsonpath "$.shared_data.booking.selectedReturnFlight.flightId" == "AA5678"
jsonpath "$.shared_data.booking.selectedReturnFlight.price" == 279.99

# ── Shared data: seat selection ───────────────────────────────────────────
jsonpath "$.shared_data.booking.selectedSeats.outbound[0]" == "12A"
jsonpath "$.shared_data.booking.selectedSeats.outbound[2]" == "12C"
jsonpath "$.shared_data.booking.selectedSeats.return[0]" == "14A"
jsonpath "$.shared_data.booking.seatUpgradeTotal" == 45.0

# ── Shared data: ancillaries ──────────────────────────────────────────────
jsonpath "$.shared_data.booking.insurance.selected" == true
jsonpath "$.shared_data.booking.insurance.insurance_type" == "comprehensive"
jsonpath "$.shared_data.booking.insurance.price" == 45.0

# ── Shared data: pricing and payment ─────────────────────────────────────
jsonpath "$.shared_data.booking.paymentStatus" == "completed"
jsonpath "$.shared_data.booking.pricing.basePrice" == 579.98
jsonpath "$.shared_data.booking.pricing.taxes" == 180.0
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.pricing.currency" == "USD"
jsonpath "$.shared_data.booking.payment.method" == "credit_card"
jsonpath "$.shared_data.booking.payment.status" == "completed"
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"

# ── Shared data: plaintext person attributes survive shredding ────────────
# passengerType is a plaintext attribute stored in shared_data, not in the
# encrypted person slot — shredding a DEK cannot touch it.
jsonpath "$.shared_data.persons.passenger_0.passengerType" == "adult"
jsonpath "$.shared_data.persons.passenger_1.passengerType" == "adult"
jsonpath "$.shared_data.persons.passenger_2.passengerType" == "child"

# ── Person identity slots (ordered by insertion) ──────────────────────────
# passenger_0 (Alice) — DEK shredded: all identity fields nulled
jsonpath "$.persons[0].person_ref" == "passenger_0"
jsonpath "$.persons[0].forgotten" == true
jsonpath "$.persons[0].name" == null
jsonpath "$.persons[0].email" == null
jsonpath "$.persons[0].phone" == null

# passenger_1 (Bob) — different DEK, completely unaffected
jsonpath "$.persons[1].person_ref" == "passenger_1"
jsonpath "$.persons[1].forgotten" == false
jsonpath "$.persons[1].email" == "bob.johnson@email.com"

# passenger_2 (Charlie) — different DEK, completely unaffected
jsonpath "$.persons[2].person_ref" == "passenger_2"
jsonpath "$.persons[2].forgotten" == false
jsonpath "$.persons[2].email" == "charlie.johnson@email.com"
