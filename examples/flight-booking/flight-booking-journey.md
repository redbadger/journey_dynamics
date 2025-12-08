# Flight Booking Journey Orchestrator

## Overview

This document models a dynamic flight booking journey as a graph of interconnected steps. Each step captures specific data and uses rules to determine available next steps based on the current state.

## Journey Steps (Nodes)

### 1. Journey Start
**Purpose**: Entry point for the booking process
**Data Captured**: 
- Session ID
- User ID (if logged in)
- Timestamp
- Channel (web, mobile app, etc.)

**Rules for Next Steps**:
- If user logged in → Go to "Search Criteria" or "Saved Searches"
- If guest user → Go to "Search Criteria" or "Account Creation"

### 2. Search Criteria
**Purpose**: Capture basic flight search parameters
**Data Captured**:
- Origin airport/city
- Destination airport/city
- Departure date
- Return date (optional for one-way)
- Number of passengers (adults, children, infants)
- Cabin class preference
- Trip type (one-way, round-trip, multi-city)

**Rules for Next Steps**:
- If multi-city selected → Go to "Multi-City Details"
- If all required fields completed → Go to "Flight Search Results"
- If passenger count > 9 → Go to "Group Booking"
- Always available → "Modify Search Criteria"

### 3. Multi-City Details
**Purpose**: Capture additional segments for complex itineraries
**Data Captured**:
- Additional origin/destination pairs
- Dates for each segment
- Segment order

**Rules for Next Steps**:
- When all segments defined → Go to "Flight Search Results"
- Always available → Back to "Search Criteria"

### 4. Flight Search Results
**Purpose**: Display available flights and capture user preferences
**Data Captured**:
- Search results displayed
- User sorting preferences
- Filter selections (price range, airlines, times, etc.)
- Viewed flight details

**Rules for Next Steps**:
- If flights available → Go to "Outbound Flight Selection"
- If no flights found → Go to "Alternative Search Suggestions"
- Always available → Back to "Search Criteria"
- If user logged in → "Save Search"

### 5. Alternative Search Suggestions
**Purpose**: Offer alternatives when no flights match criteria
**Data Captured**:
- Alternative dates suggested
- Nearby airports suggested
- User response to suggestions

**Rules for Next Steps**:
- If alternative accepted → Go to "Search Criteria" with new parameters
- Always available → Back to "Search Criteria"

### 6. Outbound Flight Selection
**Purpose**: Select the departing flight
**Data Captured**:
- Selected outbound flight details
- Fare type selected
- Flight duration and stops

**Rules for Next Steps**:
- If round-trip → Go to "Return Flight Selection"
- If one-way → Go to "Passenger Details"
- Always available → Back to "Flight Search Results"

### 7. Return Flight Selection
**Purpose**: Select the return flight for round-trip bookings
**Data Captured**:
- Selected return flight details
- Fare type selected
- Total journey duration

**Rules for Next Steps**:
- When return flight selected → Go to "Passenger Details"
- Always available → Back to "Outbound Flight Selection"

### 8. Passenger Details
**Purpose**: Capture required passenger information
**Data Captured**:
- Full names (as per passport/ID)
- Date of birth
- Gender
- Contact information
- Document details (passport/ID numbers)
- Special assistance requirements

**Rules for Next Steps**:
- If all passengers completed → Go to "Seat Selection"
- If passengers < 18 traveling alone → Go to "Unaccompanied Minor Services"
- Always available → Back to previous flight selection step

### 9. Unaccompanied Minor Services
**Purpose**: Handle special requirements for minors traveling alone
**Data Captured**:
- Guardian contact details
- Special service requests
- Additional fees acknowledgment

**Rules for Next Steps**:
- When completed → Go to "Seat Selection"

### 10. Seat Selection
**Purpose**: Allow passengers to choose seats
**Data Captured**:
- Seat assignments per passenger
- Seat upgrade fees (if applicable)
- Special seating requests

**Rules for Next Steps**:
- When seats selected or skipped → Go to "Ancillary Services"
- Always available → Back to "Passenger Details"

### 11. Ancillary Services
**Purpose**: Offer additional services and products
**Data Captured**:
- Baggage allowances selected
- Meal preferences
- Travel insurance
- Priority boarding
- Lounge access

**Rules for Next Steps**:
- When ancillaries selected/skipped → Go to "Booking Summary"
- If travel insurance declined and international flight → Go to "Insurance Confirmation"

### 12. Insurance Confirmation
**Purpose**: Confirm travel insurance decision
**Data Captured**:
- Insurance declination confirmation
- Risk acknowledgment

**Rules for Next Steps**:
- When confirmed → Go to "Booking Summary"

### 13. Booking Summary
**Purpose**: Display complete booking details for review
**Data Captured**:
- Complete itinerary review
- Total price breakdown
- Terms and conditions acceptance
- Final modifications

**Rules for Next Steps**:
- When approved → Go to "Payment"
- If changes needed → Route back to appropriate modification step
- If user not logged in → Go to "Account Creation" (optional)

### 14. Account Creation
**Purpose**: Allow guest users to create an account
**Data Captured**:
- Account credentials
- Profile preferences
- Marketing consent

**Rules for Next Steps**:
- When completed or skipped → Continue to previous destination
- If account exists → Go to "Login"

### 15. Login
**Purpose**: Authenticate existing users
**Data Captured**:
- Login credentials
- Authentication status

**Rules for Next Steps**:
- When successful → Continue to previous destination
- If forgotten password → Go to "Password Reset"

### 16. Payment
**Purpose**: Process booking payment
**Data Captured**:
- Payment method details
- Billing address
- Payment authorization
- Transaction ID

**Rules for Next Steps**:
- If payment successful → Go to "Booking Confirmation"
- If payment failed → Go to "Payment Retry"
- If alternative payment needed → Go to "Alternative Payment Methods"

### 17. Payment Retry
**Purpose**: Handle failed payments
**Data Captured**:
- Failure reason
- Retry attempt count

**Rules for Next Steps**:
- If retry successful → Go to "Booking Confirmation"
- If multiple failures → Go to "Alternative Payment Methods"
- Always available → Go to "Customer Support"

### 18. Alternative Payment Methods
**Purpose**: Offer different payment options
**Data Captured**:
- Alternative payment selection
- Additional verification requirements

**Rules for Next Steps**:
- When method selected → Go to "Payment"

### 19. Booking Confirmation
**Purpose**: Confirm successful booking and provide details
**Data Captured**:
- Booking reference number
- E-ticket numbers
- Confirmation email sent
- Check-in information provided

**Rules for Next Steps**:
- Always available → Go to "Post-Booking Services"
- Go to "Journey End"

### 20. Post-Booking Services
**Purpose**: Offer additional post-booking options
**Data Captured**:
- Service selections (seat changes, meal updates, etc.)
- Check-in status
- Special requests

**Rules for Next Steps**:
- Various modification flows available
- Go to "Journey End" when complete

### 21. Journey End
**Purpose**: Completion of the booking journey
**Data Captured**:
- Journey completion timestamp
- Final booking status
- Customer satisfaction survey (optional)

## Dynamic Rules Engine

The journey orchestrator uses these rule types:

### Conditional Rules
- **Data Completeness**: Next steps available only when required data is captured
- **Business Logic**: Rules based on airline policies, regulations, or commercial decisions
- **User State**: Different paths for logged-in vs. guest users
- **Booking Complexity**: Different flows for simple vs. complex bookings

### Branching Rules
- **Trip Type**: One-way vs. round-trip vs. multi-city
- **Passenger Type**: Adults, children, infants, unaccompanied minors
- **Payment Status**: Success, failure, retry scenarios
- **User Preferences**: Account creation, service selections

### Validation Rules
- **Data Quality**: Ensure captured data meets requirements
- **Business Constraints**: Validate against airline and regulatory rules
- **Availability**: Real-time checks for seats, flights, services

### Recovery Rules
- **Error Handling**: Paths for when things go wrong
- **Alternative Options**: Fallback steps when primary paths aren't available
- **Support Integration**: Routes to human assistance when needed

## State Management

The orchestrator maintains:
- **Session State**: Current position in journey, captured data
- **User Context**: Preferences, history, authentication status
- **Booking State**: Selected flights, passengers, services, payment status
- **Business Context**: Available options, pricing, inventory

This model allows for a flexible, user-centric booking experience where the journey adapts based on user choices, data completeness, and business rules.