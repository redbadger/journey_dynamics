# Complete Flight Booking Journey with GDPR Crypto-Shredding by Email
#
# Demonstrates `DELETE /subjects/by-email`: support staff can honour an erasure
# request by supplying only the passenger's email address — no knowledge of the
# caller's subject-ID derivation scheme is needed.
#
# Key properties verified:
#   • The endpoint resolves every non-forgotten subject_id linked to that email
#     (case-insensitively) and runs the same crypto-shredding flow for each.
#   • Shared journey data (search criteria, flight selection, pricing) is
#     completely untouched — it was never PII and was never encrypted.
#   • Other passengers on the same journey are unaffected.
#   • A repeat erasure request (idempotent call) returns 204 without error.
#
# Run: hurl --variable host=http://localhost:3030 --test full-flight-booking_with_shredding_by_email.hurl

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
            "tripType": "one-way",
            "origin": "LHR",
            "destination": "CDG",
            "departureDate": "2024-06-01",
            "passengers": {
                "adults": 2,
                "children": 0,
                "infants": 0
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 3: Select flight (non-PII shared data)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "booking": {
            "selectedOutboundFlight": {
                "flightId": "BA0303",
                "flightNumber": "BA0303",
                "departure": "2024-06-01T07:00:00",
                "arrival": "2024-06-01T09:20:00",
                "price": 189.99,
                "airline": "British Airways"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 4: Capture lead passenger identity (PII — encrypted at rest)
# The subject_id is opaque to support staff — the by-email endpoint removes
# the need to ever surface it to them.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "passenger_0",
        "subject_id": "{{newUuid}}",
        "name": "Clara Dupont",
        "email": "Clara.Dupont@example.com",
        "phone": "+33-6-12-34-56-78"
    }
}

HTTP 204


# ===========================================
# Step 5: Capture lead passenger details
# Secret fields encrypted under passenger_0's DEK.
# passengerType lands in shared_data.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "passenger_0": {
                "firstName": "Clara",
                "lastName": "Dupont",
                "dateOfBirth": "1991-03-22",
                "passportNumber": "FR987654321",
                "passengerType": "adult"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 6: Capture second passenger identity (PII — different DEK)
# This passenger must survive the erasure of passenger_0.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "person_ref": "passenger_1",
        "subject_id": "{{newUuid}}",
        "name": "Henri Dubois",
        "email": "henri.dubois@example.com",
        "phone": null
    }
}

HTTP 204


# ===========================================
# Step 7: Capture second passenger details
# After this step both passengers have passengerType in shared_data.
# The JDM reads persons.*.passengerType directly — no explicit signal needed.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "persons": {
            "passenger_1": {
                "firstName": "Henri",
                "lastName": "Dubois",
                "dateOfBirth": "1988-11-05",
                "passportNumber": "FR123456789",
                "passengerType": "adult"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 8: Payment (non-PII shared data)
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
                "transaction_id": "txn_email_erase_demo"
            },
            "pricing": {
                "basePrice": 379.98,
                "taxes": 95.00,
                "totalPrice": 474.98,
                "currency": "GBP"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 9: Complete the journey
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204


# ===========================================
# Step 10: Verify final state before erasure
# All shared_data visible; PII is in encrypted person slots.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "LHR"
jsonpath "$.shared_data.search.destination" == "CDG"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 474.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_email_erase_demo"
# Both person slots present and not yet forgotten
jsonpath "$.persons[0].person_ref" == "passenger_0"
jsonpath "$.persons[0].forgotten" == false
jsonpath "$.persons[0].name" == "Clara Dupont"
jsonpath "$.persons[0].email" == "Clara.Dupont@example.com"
jsonpath "$.persons[1].person_ref" == "passenger_1"
jsonpath "$.persons[1].forgotten" == false
jsonpath "$.persons[1].name" == "Henri Dubois"


# ===========================================
# Step 11: GDPR erasure by email — support staff supplies the email address only.
#
# The query is case-insensitive: "Clara.Dupont@example.com" was stored at
# booking time but we send the address lowercased here to prove the match
# works regardless of case.
#
# The service:
#   1. SELECTs subject_id FROM subject_lookup
#      WHERE email_lower = lower('clara.dupont@example.com')
#   2. For each subject_id: deletes the DEK, removes the subject_lookup row,
#      and emits SubjectForgotten on every journey that referenced it.
# ===========================================
DELETE {{host}}/subjects/by-email
Content-Type: application/json
{
    "email": "clara.dupont@example.com"
}

HTTP 204


# ===========================================
# Step 12: Verify journey after erasure
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
# ── Journey state ──────────────────────────────────────────────────────────
jsonpath "$.state" == "Complete"

# ── Shared data: search criteria (plaintext — survives erasure) ───────────
jsonpath "$.shared_data.search.tripType" == "one-way"
jsonpath "$.shared_data.search.origin" == "LHR"
jsonpath "$.shared_data.search.destination" == "CDG"
jsonpath "$.shared_data.search.departureDate" == "2024-06-01"
jsonpath "$.shared_data.search.passengers.adults" == 2
jsonpath "$.shared_data.search.passengers.children" == 0
jsonpath "$.shared_data.search.passengers.infants" == 0

# ── Shared data: flight selection ─────────────────────────────────────────
jsonpath "$.shared_data.booking.selectedOutboundFlight.flightId" == "BA0303"
jsonpath "$.shared_data.booking.selectedOutboundFlight.airline" == "British Airways"
jsonpath "$.shared_data.booking.selectedOutboundFlight.price" == 189.99

# ── Shared data: pricing and payment ─────────────────────────────────────
jsonpath "$.shared_data.booking.paymentStatus" == "completed"
jsonpath "$.shared_data.booking.pricing.basePrice" == 379.98
jsonpath "$.shared_data.booking.pricing.taxes" == 95.0
jsonpath "$.shared_data.booking.pricing.totalPrice" == 474.98
jsonpath "$.shared_data.booking.pricing.currency" == "GBP"
jsonpath "$.shared_data.booking.payment.method" == "credit_card"
jsonpath "$.shared_data.booking.payment.status" == "completed"
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_email_erase_demo"

# ── Shared data: plaintext person attributes survive erasure ─────────────
# passengerType is a plaintext attribute stored in shared_data, not in the
# encrypted person slot — erasing a DEK cannot touch it.
jsonpath "$.shared_data.persons.passenger_0.passengerType" == "adult"
jsonpath "$.shared_data.persons.passenger_1.passengerType" == "adult"

# ── Person identity slots (ordered by insertion) ──────────────────────────
# passenger_0 (Clara) — erased by email: all identity fields nulled
jsonpath "$.persons[0].person_ref" == "passenger_0"
jsonpath "$.persons[0].forgotten" == true
jsonpath "$.persons[0].name" == null
jsonpath "$.persons[0].email" == null
jsonpath "$.persons[0].phone" == null

# passenger_1 (Henri) — different subject, completely unaffected
jsonpath "$.persons[1].person_ref" == "passenger_1"
jsonpath "$.persons[1].forgotten" == false
jsonpath "$.persons[1].name" == "Henri Dubois"
jsonpath "$.persons[1].email" == "henri.dubois@example.com"


# ===========================================
# Step 13: Idempotency check — a repeat erasure request must succeed with 204.
# The subject is already forgotten, so find_subjects_by_email returns an empty
# list and nothing is re-shredded.
# ===========================================
DELETE {{host}}/subjects/by-email
Content-Type: application/json
{
    "email": "clara.dupont@example.com"
}

HTTP 204
