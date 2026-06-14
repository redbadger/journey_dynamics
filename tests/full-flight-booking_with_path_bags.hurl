# Complete Flight Booking Journey - Using Attribute Path Bags
#
# Demonstrates combining multiple SetAttributes calls into fewer requests
# using the canonical path-keyed `changes` form (attribute path bags).
# A single `changes` map can touch any number of attribute paths across
# different top-level sections in one HTTP request.
#
# Comparison against full-flight-booking.hurl:
#
#   Original  │  This file  │  Consolidation
#   ──────────┼─────────────┼──────────────────────────────────────
#   search      ╮            │
#   outbound    ┼──────────► │  Step 2: one SetAttributes (20 paths)
#   return      ╯            │
#   pax_0 dets  ╮            │
#   pax_1 dets  ┼──────────► │  Step 7: one SetAttributes (14 paths)
#   pax_2 dets  ╯            │
#   seats       ╮            │
#   insurance   ┼──────────► │  Step 8: one SetAttributes (mixed)
#   payment     ╯            │
#   ──────────┼─────────────┼
#   14 POSTs  │   8 POSTs   │
#
# Run: hurl --variable host=http://localhost:3030 --test full-flight-booking_with_path_bags.hurl


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
# Step 2: Search criteria + both flight selections in one request
#
# The canonical `changes` form lets a single SetAttributes touch any number
# of attribute paths regardless of their top-level section. Here we set the
# search criteria, the outbound flight, and the return flight all at once —
# three separate POSTs in the original, collapsed into one.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "changes": {
            "/search/tripType":                              "round-trip",
            "/search/origin":                               "JFK",
            "/search/destination":                          "LAX",
            "/search/departureDate":                        "2024-03-15",
            "/search/returnDate":                           "2024-03-22",
            "/search/passengers/adults":                    2,
            "/search/passengers/children":                  1,
            "/search/passengers/infants":                   0,
            "/booking/selectedOutboundFlight/flightId":     "AA1234",
            "/booking/selectedOutboundFlight/flightNumber": "AA1234",
            "/booking/selectedOutboundFlight/departure":    "2024-03-15T08:30:00",
            "/booking/selectedOutboundFlight/arrival":      "2024-03-15T11:45:00",
            "/booking/selectedOutboundFlight/price":        299.99,
            "/booking/selectedOutboundFlight/airline":      "American Airlines",
            "/booking/selectedReturnFlight/flightId":       "AA5678",
            "/booking/selectedReturnFlight/flightNumber":   "AA5678",
            "/booking/selectedReturnFlight/departure":      "2024-03-22T14:00:00",
            "/booking/selectedReturnFlight/arrival":        "2024-03-22T22:15:00",
            "/booking/selectedReturnFlight/price":          279.99,
            "/booking/selectedReturnFlight/airline":        "American Airlines"
        }
    }
}

HTTP 204


# ===========================================
# Step 3: Verify search + flight selections were captured in one shot.
#
# Because search, outbound, and return were all set atomically, the JDM
# skips straight to `collecting_passengers` (bypassing `selecting_outbound`
# and `selecting_return` entirely).
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "InProgress"
jsonpath "$.latest_workflow_decision.phase" == "collecting_passengers"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.booking.selectedOutboundFlight.flightId" == "AA1234"
jsonpath "$.shared_data.booking.selectedReturnFlight.flightId" == "AA5678"


# ===========================================
# Step 4: Register passenger 0 and bind to role path
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "RegisterAndBindSubject": {
        "role_path": "/persons/passenger_0",
        "subject_id": "{{newUuid}}",
        "email": "alice.johnson@email.com"
    }
}

HTTP 204


# ===========================================
# Step 5: Register passenger 1 and bind to role path
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
# Step 6: Register passenger 2 and bind to role path
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
# Step 7: All three passengers' details in one request
#
# A single SetAttributes can touch secret fields for multiple person slots.
# Secret fields (firstName, lastName, dateOfBirth, passportNumber) are
# encrypted under each passenger's individual DEK. passengerType is
# plaintext and lands in shared_data. This replaces six requests in the
# original (three RegisterAndBindSubject + three SetAttributes) with three
# RegisterAndBindSubject + one batched SetAttributes.
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "changes": {
            "/persons/passenger_0/firstName":      "Alice",
            "/persons/passenger_0/lastName":       "Johnson",
            "/persons/passenger_0/dateOfBirth":    "1985-06-15",
            "/persons/passenger_0/passportNumber": "AB123456",
            "/persons/passenger_0/passengerType":  "adult",
            "/persons/passenger_1/firstName":      "Bob",
            "/persons/passenger_1/lastName":       "Johnson",
            "/persons/passenger_1/dateOfBirth":    "1983-09-22",
            "/persons/passenger_1/passportNumber": "CD789012",
            "/persons/passenger_1/passengerType":  "adult",
            "/persons/passenger_2/firstName":      "Charlie",
            "/persons/passenger_2/lastName":       "Johnson",
            "/persons/passenger_2/dateOfBirth":    "2016-03-10",
            "/persons/passenger_2/passengerType":  "child"
        }
    }
}

HTTP 204


# ===========================================
# Step 8: Seat selection, insurance, and payment in one request
#
# Path bag values can themselves be objects or arrays — you are not limited
# to scalar leaves. Here seatUpgradeTotal and paymentStatus are scalar paths
# while selectedSeats, insurance, payment, and pricing are set as complete
# nested objects in a single changes map (three POSTs → one).
# ===========================================
POST {{host}}/journeys/{{journey_id}}
Content-Type: application/json
{
    "SetAttributes": {
        "changes": {
            "/booking/selectedSeats/outbound": ["12A", "12B", "12C"],
            "/booking/selectedSeats/return": ["14A", "14B", "14C"],
            "/booking/seatUpgradeTotal": 45.00,
            "/booking/insurance/selected": true,
            "/booking/insurance/insurance_type": "comprehensive",
            "/booking/insurance/price": 45.00,
            "/booking/paymentStatus": "completed",
            "/booking/payment/method": "credit_card",
            "/booking/payment/status": "completed",
            "/booking/payment/transaction_id": "txn_abc123xyz",
            "/booking/pricing/basePrice": 579.98,
            "/booking/pricing/taxes": 180.00,
            "/booking/pricing/totalPrice": 759.98,
            "/booking/pricing/currency": "USD"
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
# Step 10: Verify final state
# shared_data contains all non-PII journey data.
# PII lives in encrypted person slots (journey_person table) — not exposed here.
# ===========================================
GET {{host}}/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"

# ── Search criteria ───────────────────────────────────────────────────────
jsonpath "$.shared_data.search.tripType" == "round-trip"
jsonpath "$.shared_data.search.origin" == "JFK"
jsonpath "$.shared_data.search.destination" == "LAX"
jsonpath "$.shared_data.search.departureDate" == "2024-03-15"
jsonpath "$.shared_data.search.returnDate" == "2024-03-22"
jsonpath "$.shared_data.search.passengers.adults" == 2
jsonpath "$.shared_data.search.passengers.children" == 1
jsonpath "$.shared_data.search.passengers.infants" == 0

# ── Flight selections ─────────────────────────────────────────────────────
jsonpath "$.shared_data.booking.selectedOutboundFlight.flightId" == "AA1234"
jsonpath "$.shared_data.booking.selectedOutboundFlight.airline" == "American Airlines"
jsonpath "$.shared_data.booking.selectedOutboundFlight.price" == 299.99
jsonpath "$.shared_data.booking.selectedReturnFlight.flightId" == "AA5678"
jsonpath "$.shared_data.booking.selectedReturnFlight.price" == 279.99

# ── Seat selection ────────────────────────────────────────────────────────
jsonpath "$.shared_data.booking.selectedSeats.outbound[0]" == "12A"
jsonpath "$.shared_data.booking.selectedSeats.outbound[2]" == "12C"
jsonpath "$.shared_data.booking.selectedSeats.return[0]" == "14A"
jsonpath "$.shared_data.booking.seatUpgradeTotal" == 45.0

# ── Insurance ─────────────────────────────────────────────────────────────
jsonpath "$.shared_data.booking.insurance.selected" == true
jsonpath "$.shared_data.booking.insurance.insurance_type" == "comprehensive"
jsonpath "$.shared_data.booking.insurance.price" == 45.0

# ── Payment and pricing ───────────────────────────────────────────────────
jsonpath "$.shared_data.booking.paymentStatus" == "completed"
jsonpath "$.shared_data.booking.pricing.basePrice" == 579.98
jsonpath "$.shared_data.booking.pricing.taxes" == 180.0
jsonpath "$.shared_data.booking.pricing.totalPrice" == 759.98
jsonpath "$.shared_data.booking.pricing.currency" == "USD"
jsonpath "$.shared_data.booking.payment.method" == "credit_card"
jsonpath "$.shared_data.booking.payment.status" == "completed"
jsonpath "$.shared_data.booking.payment.transaction_id" == "txn_abc123xyz"

# ── Plaintext person attributes ───────────────────────────────────────────
jsonpath "$.shared_data.persons.passenger_0.passengerType" == "adult"
jsonpath "$.shared_data.persons.passenger_1.passengerType" == "adult"
jsonpath "$.shared_data.persons.passenger_2.passengerType" == "child"
