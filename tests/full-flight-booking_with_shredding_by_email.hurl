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
    "Capture": {
        "step": "search_criteria",
        "data": {
            "search": {
                "origin": "LHR",
                "destination": "CDG",
                "tripType": "one-way",
                "departureDate": "2024-06-01",
                "passengers": {
                    "adults": 2,
                    "children": 0,
                    "infants": 0,
                    "total": 2
                }
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
    "Capture": {
        "step": "flight_selection",
        "data": {
            "booking": {
                "selectedFlight": {
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
# Step 5: Capture lead passenger PII details (encrypted at rest)
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePersonDetails": {
        "person_ref": "passenger_0",
        "data": {
            "firstName": "Clara",
            "lastName": "Dupont",
            "dateOfBirth": "1991-03-22",
            "passportNumber": "FR987654321",
            "passengerType": "adult"
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
# Step 7: Capture second passenger PII details
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePersonDetails": {
        "person_ref": "passenger_1",
        "data": {
            "firstName": "Henri",
            "lastName": "Dubois",
            "dateOfBirth": "1988-11-05",
            "passportNumber": "FR123456789",
            "passengerType": "adult"
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
    "Capture": {
        "step": "payment",
        "data": {
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
jsonpath "$.persons[0].forgotten" == false
jsonpath "$.persons[1].forgotten" == false


# ===========================================
# Step 11: GDPR erasure by email — support staff supplies the email address only.
#
# The query is case-insensitive: "Clara.Dupont@example.com" was stored at
# booking time but we send the address lowercased here to prove the match
# works regardless of case.
#
# The service:
#   1. SELECTs DISTINCT subject_id FROM journey_person
#      WHERE lower(email) = lower('clara.dupont@example.com') AND NOT forgotten
#   2. For each subject_id: deletes the DEK and emits SubjectForgotten on
#      every journey that referenced it.
# ===========================================
DELETE {{host}}/subjects/by-email
Content-Type: application/json
{
    "email": "clara.dupont@example.com"
}

HTTP 204


# ===========================================
# Step 12: Verify journey after erasure
#
# KEY ASSERTIONS:
#   • shared_data is completely intact — non-PII data is never touched.
#   • passenger_0 (Clara) is forgotten: identity fields nulled, forgotten=true.
#   • passenger_1 (Henri) is completely unaffected.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.shared_data.search.origin" == "LHR"
jsonpath "$.shared_data.search.destination" == "CDG"
jsonpath "$.shared_data.booking.pricing.totalPrice" == 474.98
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_email_erase_demo"
jsonpath "$.persons[0].forgotten" == true
jsonpath "$.persons[0].name" == null
jsonpath "$.persons[0].email" == null
jsonpath "$.persons[0].phone" == null
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
