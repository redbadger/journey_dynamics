# Flight Booking Journey - JDM Models

This directory contains a comprehensive set of JSON Decision Models (JDM) for the GoRules ZEN engine that implement a dynamic flight booking journey orchestrator.

## Overview

The flight booking system is modeled as a graph-based decision process where each step captures data and dynamically determines the next available steps based on business rules, user context, and captured information.

## JDM Models

### 1. flight-booking-orchestrator.jdm.json
**Main orchestrator that manages the entire booking journey flow**

- **Input**: Journey state with current step and captured data
- **Processing**: Routes to appropriate step logic based on current state
- **Output**: Available next steps and primary next step recommendation

**Key Features:**
- Dynamic step routing based on current journey state
- Journey initialization for new bookings
- Step-specific business logic for 11+ booking stages
- Contextual next-step determination

**Usage:**
```javascript
const orchestratorEngine = new ZenEngine();
const decision = orchestratorEngine.createDecision(orchestratorJDM);

const result = await decision.evaluate({
  currentStep: 'search_criteria',
  userId: 'user123',
  capturedData: {
    origin: 'JFK',
    destination: 'LAX',
    departureDate: '2024-06-15',
    tripType: 'round-trip',
    passengers: { adults: 2, total: 2 }
  }
});

// Output: { availableNextSteps: ['flight_search_results'], primaryNextStep: 'flight_search_results' }
```

### 2. flight-validation-rules.jdm.json
**Comprehensive validation engine for all booking steps**

- **Input**: Step name and step data to validate
- **Processing**: Validates data against business rules using decision tables
- **Output**: Validation results with errors and field-level feedback

**Validation Capabilities:**
- Search criteria validation (airport codes, dates, passengers)
- Flight selection validation
- Passenger detail validation (names, documents, ages)
- Payment method validation
- Terms and conditions validation

**Usage:**
```javascript
const validationEngine = new ZenEngine();
const decision = validationEngine.createDecision(validationJDM);

const result = await decision.evaluate({
  stepName: 'search_criteria',
  stepData: {
    origin: 'INVALID',
    destination: '',
    departureDate: '2020-01-01',
    passengers: { total: 0 }
  }
});

// Output: { isValid: false, errors: ['Valid origin airport code required', ...] }
```

### 3. flight-pricing-calculator.jdm.json
**Dynamic pricing engine with complex fee calculations**

- **Input**: Booking details, selected services, and passenger information
- **Processing**: Calculates base prices, adjustments, taxes, and ancillary fees
- **Output**: Detailed pricing breakdown and total costs

**Pricing Components:**
- Base fare calculation with class multipliers
- Seasonal adjustments and advance booking discounts
- Baggage fee calculations based on class and route
- Ancillary service fees (seats, meals, priority boarding)
- Government taxes and fees by route
- Travel insurance pricing

**Usage:**
```javascript
const pricingEngine = new ZenEngine();
const decision = pricingEngine.createDecision(pricingJDM);

const result = await decision.evaluate({
  baseFare: 299,
  cabinClass: 'economy',
  departureDate: '2024-07-15',
  departureCountry: 'US',
  arrivalCountry: 'US',
  distance: 2500,
  selectedBaggage: { bags: 1, weight: 23 },
  priorityBoarding: true,
  passengerCount: 2
});

// Output: { pricing: { grandTotal: 758.20, breakdown: {...} } }
```

### 4. flight-error-handling.jdm.json
**Comprehensive error handling and recovery system**

- **Input**: Error type, details, current step, and retry count
- **Processing**: Classifies errors and determines appropriate recovery actions
- **Output**: Recovery instructions, user messages, and next steps

**Error Types Handled:**
- Validation errors with field-specific guidance
- Payment failures with retry strategies
- Availability issues with alternative suggestions
- System errors with escalation procedures
- Business rule violations with documentation requirements

**Usage:**
```javascript
const errorEngine = new ZenEngine();
const decision = errorEngine.createDecision(errorHandlingJDM);

const result = await decision.evaluate({
  errorType: 'payment',
  errorDetails: {
    paymentErrorCode: 'INSUFFICIENT_FUNDS',
    paymentMethodType: 'credit_card'
  },
  currentStep: 'payment',
  retryCount: 1
});

// Output: { recoveryAction: 'suggest_alternative_payment', userMessage: '...', holdBookingMinutes: 15 }
```

## Integration Example

Here's how to integrate all JDM models into a complete flight booking system:

```javascript
import { ZenEngine } from '@gorules/zen-engine';
import fs from 'fs/promises';

class FlightBookingSystem {
  constructor() {
    this.engines = {};
    this.decisions = {};
  }

  async initialize() {
    // Load JDM models
    const models = [
      'flight-booking-orchestrator',
      'flight-validation-rules', 
      'flight-pricing-calculator',
      'flight-error-handling'
    ];

    for (const model of models) {
      const content = await fs.readFile(`./jdm-models/${model}.jdm.json`);
      this.engines[model] = new ZenEngine();
      this.decisions[model] = this.engines[model].createDecision(content);
    }
  }

  async processBookingStep(journeyState, stepData) {
    try {
      // 1. Validate step data
      const validation = await this.decisions['flight-validation-rules'].evaluate({
        stepName: journeyState.currentStep,
        stepData: stepData
      });

      if (!validation.isValid) {
        return {
          success: false,
          errors: validation.errors,
          step: journeyState.currentStep
        };
      }

      // 2. Update journey state
      const updatedState = {
        ...journeyState,
        capturedData: { ...journeyState.capturedData, ...stepData }
      };

      // 3. Determine next steps
      const orchestration = await this.decisions['flight-booking-orchestrator'].evaluate(updatedState);

      // 4. Calculate pricing if applicable
      let pricing = null;
      if (['booking_summary', 'payment'].includes(orchestration.primaryNextStep)) {
        pricing = await this.decisions['flight-pricing-calculator'].evaluate({
          ...updatedState.capturedData,
          baseFare: updatedState.capturedData.selectedOutboundFlight?.price || 0
        });
      }

      return {
        success: true,
        currentStep: journeyState.currentStep,
        nextSteps: orchestration.availableNextSteps,
        primaryNextStep: orchestration.primaryNextStep,
        capturedData: updatedState.capturedData,
        pricing: pricing?.pricing || null
      };

    } catch (error) {
      // Handle errors using error handling JDM
      const errorResponse = await this.decisions['flight-error-handling'].evaluate({
        errorType: 'system',
        errorDetails: { systemErrorType: 'PROCESSING_ERROR' },
        currentStep: journeyState.currentStep,
        retryCount: 0
      });

      return errorResponse;
    }
  }

  async startNewJourney(userId = null) {
    const initialState = {
      sessionId: `session_${Date.now()}`,
      currentStep: null, // Will trigger journey initialization
      userId: userId,
      capturedData: {},
      stepHistory: []
    };

    return await this.processBookingStep(initialState, {});
  }
}

// Usage
const bookingSystem = new FlightBookingSystem();
await bookingSystem.initialize();

// Start a new journey
const journey = await bookingSystem.startNewJourney('user123');
console.log('Journey started:', journey);

// Process search criteria
const searchResult = await bookingSystem.processBookingStep(journey, {
  origin: 'JFK',
  destination: 'LAX',
  departureDate: '2024-06-15',
  returnDate: '2024-06-22',
  tripType: 'round-trip',
  passengers: { adults: 2, children: 0, infants: 0, total: 2 },
  cabinClass: 'economy'
});
console.log('Search processed:', searchResult);
```

## Business Rules Highlights

### Dynamic Journey Flow
- **Conditional Routing**: Routes users to different steps based on trip type (one-way, round-trip, multi-city)
- **User Context**: Different flows for logged-in vs. guest users
- **Special Handling**: Automatic routing for group bookings, unaccompanied minors
- **Error Recovery**: Smart fallback to appropriate recovery steps

### Comprehensive Validation
- **Airport Codes**: 3-letter IATA code validation
- **Date Logic**: Future dates, return after departure
- **Passenger Limits**: 1-9 passengers for regular booking
- **Document Requirements**: Age-appropriate validation
- **Payment Security**: Card validation with fraud detection

### Intelligent Pricing
- **Dynamic Base Pricing**: Class-based multipliers and seasonal adjustments
- **Advance Booking Discounts**: Early booking incentives
- **Route-Based Fees**: Different pricing for domestic vs. international
- **Service Bundling**: Smart fee calculation for combined services
- **Tax Compliance**: Accurate government fee calculation

### Robust Error Handling
- **Retry Strategies**: Intelligent retry with exponential backoff
- **User Communication**: Clear, actionable error messages
- **Booking Preservation**: Hold bookings during error recovery
- **Escalation Paths**: Automatic routing to human assistance when needed
- **Alternative Offerings**: Suggest alternatives when primary options fail

## Configuration

### Environment Variables
```bash
# ZEN Engine Configuration
ZEN_ENGINE_TIMEOUT=30000
ZEN_ENGINE_MEMORY_LIMIT=512MB

# Booking Configuration
BOOKING_HOLD_TIME_MINUTES=15
MAX_RETRY_ATTEMPTS=3
PAYMENT_TIMEOUT_SECONDS=120

# Feature Flags
ENABLE_DYNAMIC_PRICING=true
ENABLE_ADVANCED_ERROR_RECOVERY=true
ENABLE_ALTERNATIVE_SUGGESTIONS=true
```

### JDM Model Customization

Each JDM model can be customized by modifying the decision tables and rules:

1. **Business Rules**: Update decision table conditions and outputs
2. **Pricing Logic**: Modify multipliers, fees, and tax calculations  
3. **Validation Rules**: Add new validation criteria or modify existing ones
4. **Error Messages**: Customize user-facing messages and recovery actions

### Testing

```bash
# Run JDM validation tests
npm test jdm-validation

# Test complete journey flows  
npm test journey-flows

# Validate business rules
npm test business-rules

# Performance testing
npm test jdm-performance
```

## Extending the System

### Adding New Steps
1. Add step routing condition to the main orchestrator switch node
2. Create decision logic node for the new step
3. Add validation rules to the validation JDM
4. Update error handling for step-specific errors

### Custom Business Rules
1. Add new decision table rows for additional conditions
2. Create custom expression nodes for complex calculations
3. Use function nodes for advanced JavaScript logic
4. Implement custom node types for specialized processing

### Integration Points
- **External APIs**: Flight search, payment processing, airline systems
- **Databases**: User profiles, booking history, inventory
- **Services**: Email, SMS, analytics, fraud detection
- **Third-party**: GDS systems, hotel bookings, car rentals

## ZEN Expression Language Corrections

The JDM files have been corrected to use proper ZEN expression syntax:

### Key Corrections Made:
1. **Null Checking**: Replaced `exists(field)` with `field != null`
2. **Array Length**: Replaced `length(array)` with `len(array)`  
3. **Boolean Values**: Used direct `true`/`false` instead of string literals
4. **Logical Operators**: Replaced `&&` with `and` and `||` with `or`
5. **Date Functions**: Fixed date operations:
   - `date(now()).year` instead of `now().year`
   - `month(date(field))` for month extraction
   - `weekday(date(field))` for weekday extraction
6. **Array Functions**: 
   - Replaced `any()` with `some()`
   - Kept `all()` as is (correct ZEN syntax)
7. **String Functions**: Used `len()` for string length
8. **Date Arithmetic**: Used date subtraction `date(field1) - date(field2)` instead of `daysBetween()`

### Common ZEN Expression Patterns:
```zen
// Null checking
field != null                    // Instead of exists(field)

// Logical operators
field1 != null and field2 != null  // Instead of field1 != null && field2 != null
condition1 or condition2            // Instead of condition1 || condition2

// Array operations  
some(array, $.prop == value)     // Instead of any()
all(array, $.prop != null)       // Correct syntax
len(array) > 0                   // Instead of length()

// Date operations
date(now())                      // Current date
month(date(field))               // Extract month
date(field1) - date(field2)      // Date difference

// String operations
len(string) >= 3                 // String length check
```

This JDM-based approach provides a flexible, maintainable, and business-friendly way to manage complex flight booking workflows while ensuring consistency, accuracy, and excellent user experience.