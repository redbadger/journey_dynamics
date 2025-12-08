// Journey Orchestrator Usage Example
// Demonstrates how the flight booking orchestrator works in practice

import { FlightBookingService, JourneyState, StepResult } from './journey-orchestrator-implementation';

// Example: Complete flight booking flow
async function demonstrateFlightBookingJourney() {
  const bookingService = new FlightBookingService();
  const sessionId = 'session_' + Date.now();

  console.log('=== Flight Booking Journey Demo ===\n');

  try {
    // Step 1: Start the journey
    console.log('1. Starting booking journey...');
    let journeyState = await bookingService.startBooking(sessionId);
    console.log(`Current step: ${journeyState.currentStepId}`);
    console.log(`Available data: ${JSON.stringify(journeyState.capturedData, null, 2)}\n`);

    // Step 2: Enter search criteria
    console.log('2. Entering search criteria...');
    let stepResult = await bookingService.processUserInput(sessionId, {
      origin: 'JFK',
      destination: 'LAX',
      departureDate: '2024-06-15',
      returnDate: '2024-06-22',
      tripType: 'round-trip',
      passengers: {
        adults: 2,
        children: 0,
        infants: 0,
        total: 2
      },
      cabinClass: 'economy'
    });

    if (stepResult.success) {
      console.log('✅ Search criteria captured successfully');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to flight search results
      journeyState = await bookingService.navigateToStep(sessionId, 'flight_search_results');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    } else {
      console.log('❌ Validation errors:', stepResult.validationErrors);
      return;
    }

    // Step 3: Simulate flight search results
    console.log('3. Processing search results...');
    stepResult = await bookingService.processUserInput(sessionId, {
      searchResults: [
        {
          flightNumber: 'AA123',
          departure: '08:00',
          arrival: '11:30',
          price: 299,
          duration: '5h 30m'
        },
        {
          flightNumber: 'UA456',
          departure: '14:00',
          arrival: '17:45',
          price: 329,
          duration: '5h 45m'
        }
      ],
      filters: { maxPrice: 400, preferredTime: 'morning' }
    });

    if (stepResult.success) {
      console.log('✅ Flight results loaded');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to outbound flight selection
      journeyState = await bookingService.navigateToStep(sessionId, 'outbound_flight_selection');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 4: Select outbound flight
    console.log('4. Selecting outbound flight...');
    stepResult = await bookingService.processUserInput(sessionId, {
      selectedOutboundFlight: {
        flightNumber: 'AA123',
        departure: '08:00',
        arrival: '11:30',
        price: 299,
        aircraft: 'Boeing 737-800'
      },
      fareType: 'main-cabin'
    });

    if (stepResult.success) {
      console.log('✅ Outbound flight selected: AA123');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to return flight selection (since it's round-trip)
      journeyState = await bookingService.navigateToStep(sessionId, 'return_flight_selection');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 5: Select return flight
    console.log('5. Selecting return flight...');
    stepResult = await bookingService.processUserInput(sessionId, {
      selectedReturnFlight: {
        flightNumber: 'AA789',
        departure: '15:00',
        arrival: '23:15',
        price: 319,
        aircraft: 'Boeing 737-900'
      }
    });

    if (stepResult.success) {
      console.log('✅ Return flight selected: AA789');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to passenger details
      journeyState = await bookingService.navigateToStep(sessionId, 'passenger_details');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 6: Enter passenger details
    console.log('6. Entering passenger details...');
    stepResult = await bookingService.processUserInput(sessionId, {
      passengers: [
        {
          firstName: 'John',
          lastName: 'Doe',
          dateOfBirth: '1985-03-15',
          gender: 'M',
          documentType: 'passport',
          documentNumber: 'A1234567',
          email: 'john.doe@example.com',
          phone: '+1-555-0123'
        },
        {
          firstName: 'Jane',
          lastName: 'Doe',
          dateOfBirth: '1987-08-22',
          gender: 'F',
          documentType: 'passport',
          documentNumber: 'B7654321',
          email: 'jane.doe@example.com',
          phone: '+1-555-0124'
        }
      ]
    });

    if (stepResult.success) {
      console.log('✅ Passenger details captured for 2 travelers');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to seat selection
      journeyState = await bookingService.navigateToStep(sessionId, 'seat_selection');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    } else {
      console.log('❌ Passenger validation errors:', stepResult.validationErrors);
    }

    // Step 7: Seat selection (optional)
    console.log('7. Selecting seats...');
    stepResult = await bookingService.processUserInput(sessionId, {
      seatAssignments: [
        { passengerId: 0, outboundSeat: '12A', returnSeat: '15A' },
        { passengerId: 1, outboundSeat: '12B', returnSeat: '15B' }
      ],
      seatUpgrades: []
    });

    if (stepResult.success) {
      console.log('✅ Seats selected: 12A/12B (outbound), 15A/15B (return)');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to ancillary services
      journeyState = await bookingService.navigateToStep(sessionId, 'ancillary_services');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 8: Add ancillary services
    console.log('8. Adding ancillary services...');
    stepResult = await bookingService.processUserInput(sessionId, {
      baggage: [
        { passengerId: 0, bags: 1, weight: '23kg' },
        { passengerId: 1, bags: 1, weight: '23kg' }
      ],
      meals: [
        { passengerId: 0, preference: 'vegetarian' },
        { passengerId: 1, preference: 'standard' }
      ],
      insurance: false, // Will trigger insurance confirmation
      priorityBoarding: true
    });

    if (stepResult.success) {
      console.log('✅ Ancillary services selected');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Insurance confirmation step will be triggered
      journeyState = await bookingService.navigateToStep(sessionId, 'insurance_confirmation');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 9: Insurance confirmation
    console.log('9. Confirming insurance decision...');
    stepResult = await bookingService.processUserInput(sessionId, {
      insuranceDeclined: true,
      riskAcknowledgment: true
    });

    if (stepResult.success) {
      console.log('✅ Insurance declination confirmed');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to booking summary
      journeyState = await bookingService.navigateToStep(sessionId, 'booking_summary');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 10: Review booking summary
    console.log('10. Reviewing booking summary...');
    const totalPrice = 299 + 319 + 50 + 25; // flights + baggage + meals + priority boarding
    stepResult = await bookingService.processUserInput(sessionId, {
      reviewedItinerary: true,
      totalPrice: totalPrice,
      termsAccepted: true
    });

    if (stepResult.success) {
      console.log(`✅ Booking summary reviewed - Total: $${totalPrice}`);
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to payment (assuming user doesn't want to create account)
      journeyState = await bookingService.navigateToStep(sessionId, 'payment');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 11: Process payment
    console.log('11. Processing payment...');
    stepResult = await bookingService.processUserInput(sessionId, {
      paymentMethod: {
        type: 'credit_card',
        details: {
          cardNumber: '**** **** **** 1234',
          expiryDate: '12/26',
          cvv: '123',
          cardholderName: 'John Doe'
        }
      },
      billingAddress: {
        street: '123 Main St',
        city: 'New York',
        state: 'NY',
        zipCode: '10001',
        country: 'US'
      },
      paymentStatus: 'success', // Simulated successful payment
      transactionId: 'txn_' + Date.now()
    });

    if (stepResult.success) {
      console.log('✅ Payment processed successfully');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Move to booking confirmation
      journeyState = await bookingService.navigateToStep(sessionId, 'booking_confirmation');
      console.log(`Moved to: ${journeyState.currentStepId}\n`);
    }

    // Step 12: Booking confirmation
    console.log('12. Generating booking confirmation...');
    stepResult = await bookingService.processUserInput(sessionId, {
      bookingReference: 'ABC123',
      eTicketNumbers: ['1234567890123', '1234567890124'],
      confirmationEmailSent: true,
      checkInAvailable: '2024-06-14T08:00:00Z' // 24 hours before departure
    });

    if (stepResult.success) {
      console.log('✅ Booking confirmed! Reference: ABC123');
      console.log('✅ E-tickets issued and confirmation email sent');
      console.log(`Next available steps: ${stepResult.nextSteps.join(', ')}`);

      // Complete the journey
      journeyState = await bookingService.navigateToStep(sessionId, 'journey_end');
      console.log(`Journey completed: ${journeyState.currentStepId}\n`);
    }

    // Final journey state
    const finalState = await bookingService.getJourneyStatus(sessionId);
    console.log('=== Final Journey State ===');
    console.log(`Steps completed: ${finalState?.stepHistory.join(' → ')}`);
    console.log(`Total duration: ${new Date().getTime() - new Date(finalState?.capturedData.timestamp).getTime()}ms`);
    console.log('Journey completed successfully! ✈️\n');

  } catch (error) {
    console.error('Journey failed:', error);
  }
}

// Example: Error handling and validation
async function demonstrateErrorHandling() {
  const bookingService = new FlightBookingService();
  const sessionId = 'error_session_' + Date.now();

  console.log('=== Error Handling Demo ===\n');

  // Start journey
  await bookingService.startBooking(sessionId);

  // Try to submit invalid search criteria
  console.log('Attempting invalid search criteria...');
  const result = await bookingService.processUserInput(sessionId, {
    origin: 'INVALID', // Invalid airport code
    destination: '', // Missing destination
    departureDate: '2020-01-01', // Date in the past
    passengers: { total: 0 } // No passengers
  });

  if (!result.success) {
    console.log('❌ Validation caught errors:');
    result.validationErrors.forEach(error => console.log(`  - ${error}`));
  }

  console.log('\nNow fixing the errors...');
  const correctedResult = await bookingService.processUserInput(sessionId, {
    origin: 'JFK',
    destination: 'LAX',
    departureDate: '2024-07-01',
    tripType: 'one-way',
    passengers: { adults: 1, children: 0, infants: 0, total: 1 }
  });

  if (correctedResult.success) {
    console.log('✅ Fixed validation errors successfully');
    console.log(`Available next steps: ${correctedResult.nextSteps.join(', ')}`);
  }
}

// Example: Dynamic step routing based on conditions
async function demonstrateDynamicRouting() {
  const bookingService = new FlightBookingService();
  const sessionId = 'dynamic_session_' + Date.now();

  console.log('=== Dynamic Routing Demo ===\n');

  // Start journey as logged-in user
  console.log('Starting journey as logged-in user...');
  let journeyState = await bookingService.startBooking(sessionId, 'user123');
  console.log(`User context affects available paths: ${journeyState.capturedData.userId ? 'Logged in' : 'Guest'}\n`);

  // Set up multi-city trip to show different routing
  console.log('Setting up multi-city trip...');
  let stepResult = await bookingService.processUserInput(sessionId, {
    origin: 'JFK',
    destination: 'LAX',
    departureDate: '2024-08-01',
    tripType: 'multi-city',
    passengers: { adults: 1, total: 1 }
  });

  if (stepResult.success) {
    console.log('✅ Multi-city trip detected');
    console.log(`Next steps routed to: ${stepResult.nextSteps[0]}`);
    console.log('Normal round-trip would go to flight_search_results');
    console.log('Multi-city goes to multi_city_details first\n');
  }

  // Demonstrate unaccompanied minor routing
  console.log('Demonstrating unaccompanied minor routing...');
  journeyState = await bookingService.navigateToStep(sessionId, 'passenger_details');

  stepResult = await bookingService.processUserInput(sessionId, {
    passengers: [
      {
        firstName: 'Emma',
        lastName: 'Smith',
        dateOfBirth: '2010-05-15', // 14 years old
        gender: 'F',
        documentType: 'passport',
        documentNumber: 'C9876543',
        travelingAlone: true // Unaccompanied minor
      }
    ]
  });

  if (stepResult.success) {
    console.log('✅ Unaccompanied minor detected');
    console.log(`Special routing to: ${stepResult.nextSteps[0]}`);
    console.log('Additional services will be required for supervision\n');
  }
}

// Run all demonstrations
async function runAllDemos() {
  await demonstrateFlightBookingJourney();
  console.log('\n' + '='.repeat(50) + '\n');

  await demonstrateErrorHandling();
  console.log('\n' + '='.repeat(50) + '\n');

  await demonstrateDynamicRouting();
}

// Export for use in other modules
export {
  demonstrateFlightBookingJourney,
  demonstrateErrorHandling,
  demonstrateDynamicRouting,
  runAllDemos
};

// Run demo if this file is executed directly
if (require.main === module) {
  runAllDemos().catch(console.error);
}
