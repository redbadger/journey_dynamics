# Complete Flight Booking Journey
# Run: hurl --test full-flight-booking.hurl

# ===========================================
# Step 1: Create a new journey (UUID auto-generated)
# ===========================================
POST http://localhost:3030/journeys

HTTP 201
[Captures]
journey_id: header "Location" regex "/journeys/([a-f0-9-]+)"
[Asserts]
header "Location" matches "/journeys/[a-f0-9-]+"


# ===========================================
# Step 2: Capture search criteria
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "search_criteria",
        "data": {
            "origin": "JFK",
            "destination": "LAX",
            "tripType": "round-trip",
            "departureDate": "2024-03-15",
            "returnDate": "2024-03-22",
            "passengers": {
                "adults": 2,
                "children": 1,
                "infants": 0
            },
            "cabinClass": "economy"
        }
    }
}

HTTP 204


# ===========================================
# Step 3: Verify search criteria captured
# ===========================================
GET http://localhost:3030/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "InProgress"
jsonpath "$.current_step" == "search_criteria"
jsonpath "$.accumulated_data.origin" == "JFK"
jsonpath "$.accumulated_data.destination" == "LAX"


# ===========================================
# Step 4: Select outbound flight
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "outbound_selection",
        "data": {
            "selectedOutbound": {
                "flightNumber": "AA1234",
                "departureTime": "08:30",
                "arrivalTime": "11:45",
                "price": 299.99,
                "airline": "American Airlines"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 5: Select return flight
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "return_selection",
        "data": {
            "selectedReturn": {
                "flightNumber": "AA5678",
                "departureTime": "14:00",
                "arrivalTime": "22:15",
                "price": 279.99,
                "airline": "American Airlines"
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 6: Capture passenger details
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "passenger_details",
        "data": {
            "passengers": [
                {
                    "type": "adult",
                    "firstName": "Alice",
                    "lastName": "Johnson",
                    "dateOfBirth": "1985-06-15",
                    "passportNumber": "AB123456"
                },
                {
                    "type": "adult",
                    "firstName": "Bob",
                    "lastName": "Johnson",
                    "dateOfBirth": "1983-09-22",
                    "passportNumber": "CD789012"
                },
                {
                    "type": "child",
                    "firstName": "Charlie",
                    "lastName": "Johnson",
                    "dateOfBirth": "2016-03-10"
                }
            ]
        }
    }
}

HTTP 204


# ===========================================
# Step 7: Capture primary contact person
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "CapturePerson": {
        "name": "Alice Johnson",
        "email": "alice.johnson@email.com",
        "phone": "+1-555-123-4567"
    }
}

HTTP 204


# ===========================================
# Step 8: Select seats
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "seat_selection",
        "data": {
            "outboundSeats": ["12A", "12B", "12C"],
            "returnSeats": ["14A", "14B", "14C"],
            "seatUpgradeTotal": 45.00
        }
    }
}

HTTP 204


# ===========================================
# Step 9: Add extras/ancillaries
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "ancillaries",
        "data": {
            "baggage": {
                "checkedBags": 2,
                "baggageTotal": 60.00
            },
            "meals": {
                "selected": true,
                "mealsTotal": 30.00
            },
            "insurance": {
                "selected": true,
                "insuranceTotal": 45.00
            }
        }
    }
}

HTTP 204


# ===========================================
# Step 10: Payment
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Capture": {
        "step": "payment",
        "data": {
            "paymentMethod": "credit_card",
            "cardType": "visa",
            "lastFourDigits": "4242",
            "billingAddress": {
                "street": "123 Main St",
                "city": "New York",
                "state": "NY",
                "zipCode": "10001",
                "country": "USA"
            },
            "totalAmount": 759.98,
            "currency": "USD",
            "transactionId": "txn_abc123xyz"
        }
    }
}

HTTP 204


# ===========================================
# Step 11: Complete the journey
# ===========================================
POST http://localhost:3030/journeys/{{journey_id}}
Content-Type: application/json
{
    "Complete": null
}

HTTP 204


# ===========================================
# Step 12: Verify final state
# ===========================================
GET http://localhost:3030/journeys/{{journey_id}}

HTTP 200
[Asserts]
jsonpath "$.state" == "Complete"
jsonpath "$.accumulated_data.origin" == "JFK"
jsonpath "$.accumulated_data.destination" == "LAX"
jsonpath "$.accumulated_data.totalAmount" == 759.98
jsonpath "$.accumulated_data.transactionId" == "txn_abc123xyz"
