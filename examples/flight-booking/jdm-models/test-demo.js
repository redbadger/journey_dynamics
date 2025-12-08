#!/usr/bin/env node

/**
 * Flight Booking JDM Models - Test and Demo Script
 * Demonstrates the complete flight booking journey using GoRules ZEN engine
 */

// Note: This is a demo script. To run with actual ZEN engine, install @gorules/zen-engine
// import { ZenEngine } from '@gorules/zen-engine';
import fs from 'fs/promises';
import path from 'path';

class FlightBookingJDMDemo {
  constructor() {
    this.engines = {};
    this.decisions = {};
  }

  async initialize() {
    console.log('🚀 Initializing Flight Booking JDM Demo...\n');

    // Load all JDM models
    const models = [
      'flight-booking-orchestrator',
      'flight-validation-rules',
      'flight-pricing-calculator',
      'flight-error-handling'
    ];

    for (const model of models) {
      try {
        const filePath = path.join(process.cwd(), 'zen', 'jdm-models', `${model}.jdm.json`);
        const content = await fs.readFile(filePath, 'utf8');
        const jdmContent = JSON.parse(content);

        // Mock ZEN engine for demo purposes
        this.engines[model] = { createDecision: () => ({ evaluate: this.mockEvaluate }) };
        this.decisions[model] = this.engines[model].createDecision(jdmContent);

        console.log(`✅ Loaded ${model}.jdm.json`);
      } catch (error) {
        console.error(`❌ Failed to load ${model}: ${error.message}`);
      }
    }

    // Mock evaluation function for demo purposes
    async mockEvaluate(input) {
      if (input.stepName === 'search_criteria') {
        return {
          isValid: true,
          errors: [],
          validatedFields: ['origin', 'destination', 'departureDate', 'passengers']
        };
      }

      if (input.currentStep === 'search_criteria') {
        return {
          availableNextSteps: ['flight_search_results'],
          primaryNextStep: 'flight_search_results'
        };
      }

      if (input.baseFare) {
        return {
          pricing: {
            baseFare: input.baseFare * (input.passengerCount || 1),
            baggageFees: 25,
            ancillaryFees: 20,
            taxesAndFees: 45,
            insuranceFee: 0,
            grandTotal: (input.baseFare * (input.passengerCount || 1)) + 90
          }
        };
      }

      return {
        availableNextSteps: ['next_step'],
        primaryNextStep: 'next_step',
        recoveryAction: 'continue',
        userMessage: 'Demo response',
        canRetry: true
      };
    }
    console.log();
  }

  async runCompleteJourneyDemo() {
    console.log('🎯 Demo: Complete Flight Booking Journey\n');
    console.log('=' .repeat(60));

    // Step 1: Start new journey
    console.log('\n📍 Step 1: Starting new booking journey...');
    let journeyState = {
      currentStep: null, // Will trigger initialization
      userId: 'demo_user_123',
      capturedData: {},
      stepHistory: []
    };

    let orchestrationResult = await this.decisions['flight-booking-orchestrator'].evaluate(journeyState);
    console.log('   Current step:', orchestrationResult.currentStep);
    console.log('   Available next steps:', orchestrationResult.availableNextSteps);
    console.log('   Primary next step:', orchestrationResult.primaryNextStep);

    // Update journey state
    journeyState = {
      ...journeyState,
      ...orchestrationResult,
      currentStep: 'search_criteria'
    };

    // Step 2: Enter search criteria
    console.log('\n📍 Step 2: Entering search criteria...');
    const searchData = {
      origin: 'JFK',
      destination: 'LAX',
      departureDate: '2024-08-15',
      returnDate: '2024-08-22',
      tripType: 'round-trip',
      passengers: { adults: 2, children: 0, infants: 0, total: 2 },
      cabinClass: 'economy'
    };

    // Validate search data
    let validationResult = await this.decisions['flight-validation-rules'].evaluate({
      stepName: 'search_criteria',
      stepData: searchData
    });

    console.log('   Validation result:', validationResult.isValid ? '✅ Valid' : '❌ Invalid');
    if (!validationResult.isValid) {
      console.log('   Errors:', validationResult.errors);
      return;
    }

    // Update captured data and determine next steps
    journeyState.capturedData = { ...journeyState.capturedData, ...searchData };
    orchestrationResult = await this.decisions['flight-booking-orchestrator'].evaluate(journeyState);
    console.log('   Next available steps:', orchestrationResult.availableNextSteps);

    // Step 3: Simulate flight search results
    console.log('\n📍 Step 3: Processing flight search results...');
    journeyState.currentStep = 'flight_search_results';
    journeyState.capturedData.searchResults = [
      { flightNumber: 'AA123', price: 299, departure: '08:00', arrival: '11:30' },
      { flightNumber: 'UA456', price: 329, departure: '14:00', arrival: '17:45' }
    ];

    orchestrationResult = await this.decisions['flight-booking-orchestrator'].evaluate(journeyState);
    console.log('   Flights found:', journeyState.capturedData.searchResults.length);
    console.log('   Next step:', orchestrationResult.primaryNextStep);

    // Step 4: Select outbound flight
    console.log('\n📍 Step 4: Selecting outbound flight...');
    journeyState.currentStep = 'outbound_flight_selection';
    journeyState.capturedData.selectedOutboundFlight = {
      flightNumber: 'AA123',
      price: 299,
      departure: '08:00',
      arrival: '11:30',
      aircraft: 'Boeing 737-800'
    };

    orchestrationResult = await this.decisions['flight-booking-orchestrator'].evaluate(journeyState);
    console.log('   Outbound flight selected:', journeyState.capturedData.selectedOutboundFlight.flightNumber);
    console.log('   Next step:', orchestrationResult.primaryNextStep);

    // Step 5: Select return flight
    console.log('\n📍 Step 5: Selecting return flight...');
    journeyState.currentStep = 'return_flight_selection';
    journeyState.capturedData.selectedReturnFlight = {
      flightNumber: 'AA789',
      price: 319,
      departure: '15:00',
      arrival: '23:15'
    };

    orchestrationResult = await this.decisions['flight-booking-orchestrator'].evaluate(journeyState);
    console.log('   Return flight selected:', journeyState.capturedData.selectedReturnFlight.flightNumber);
    console.log('   Next step:', orchestrationResult.primaryNextStep);

    // Step 6: Calculate pricing
    console.log('\n📍 Step 6: Calculating total pricing...');
    const pricingResult = await this.decisions['flight-pricing-calculator'].evaluate({
      baseFare: 299,
      cabinClass: 'economy',
      departureDate: '2024-08-15',
      departureCountry: 'US',
      arrivalCountry: 'US',
      distance: 2500,
      selectedBaggage: { bags: 1, weight: 23 },
      priorityBoarding: true,
      passengerCount: 2,
      travelInsurance: false
    });

    console.log('   Base fare (2 passengers):', `$${pricingResult.pricing.baseFare}`);
    console.log('   Baggage fees:', `$${pricingResult.pricing.baggageFees}`);
    console.log('   Ancillary fees:', `$${pricingResult.pricing.ancillaryFees}`);
    console.log('   Taxes and fees:', `$${pricingResult.pricing.taxesAndFees}`);
    console.log('   📊 Grand total:', `$${pricingResult.pricing.grandTotal}`);

    console.log('\n🎉 Journey demo completed successfully!');
  }

  async runValidationDemo() {
    console.log('\n🔍 Demo: Validation Engine\n');
    console.log('=' .repeat(40));

    const testCases = [
     {
       name: 'Valid search criteria',
       stepName: 'search_criteria',
       stepData: {
         origin: 'JFK',
         destination: 'LAX',
         departureDate: '2024-12-01',
         passengers: { total: 2 }
       }
     },
     {
       name: 'Invalid airport codes',
       stepName: 'search_criteria',
       stepData: {
         origin: 'INVALID',
         destination: '',
         departureDate: '2024-12-01',
         passengers: { total: 2 }
       }
     },
      {
        name: 'Past departure date',
        stepName: 'search_criteria',
        stepData: {
          origin: 'JFK',
          destination: 'LAX',
          departureDate: '2020-01-01',
          passengers: { total: 2 }
        }
      },
      {
        name: 'Invalid passenger count',
        stepName: 'search_criteria',
        stepData: {
          origin: 'JFK',
          destination: 'LAX',
          departureDate: '2024-12-01',
          passengers: { total: 0 }
        }
      }
    ];

    for (const testCase of testCases) {
      console.log(`\n📋 Testing: ${testCase.name}`);

      const result = await this.decisions['flight-validation-rules'].evaluate({
        stepName: testCase.stepName,
        stepData: testCase.stepData
      });

      console.log(`   Result: ${result.isValid ? '✅ Valid' : '❌ Invalid'}`);
      if (!result.isValid && result.errors?.length > 0) {
        result.errors.forEach(error => console.log(`   - ${error}`));
      }
    }
  }

  async runPricingDemo() {
    console.log('\n💰 Demo: Pricing Calculator\n');
    console.log('=' .repeat(40));

    const pricingScenarios = [
      {
        name: 'Economy domestic flight',
        data: {
          baseFare: 199,
          cabinClass: 'economy',
          departureDate: '2024-08-15',
          departureCountry: 'US',
          arrivalCountry: 'US',
          distance: 1200,
          passengerCount: 1,
          selectedBaggage: { bags: 1, weight: 23 },
          priorityBoarding: false,
          travelInsurance: false
        }
      },
      {
        name: 'Business class international',
        data: {
          baseFare: 1299,
          cabinClass: 'business',
          departureDate: '2024-07-01', // Peak season
          departureCountry: 'US',
          arrivalCountry: 'UK',
          distance: 3500,
          passengerCount: 1,
          selectedBaggage: { bags: 2, weight: 23 },
          loungeAccess: true,
          travelInsurance: 'standard'
        }
      },
      {
        name: 'Family economy with extras',
        data: {
          baseFare: 299,
          cabinClass: 'economy',
          departureDate: '2024-03-15', // Low season
          departureCountry: 'US',
          arrivalCountry: 'US',
          distance: 2500,
          passengerCount: 4,
          selectedBaggage: { bags: 2, weight: 23 },
          selectedSeats: [{ type: 'premium' }, { type: 'standard' }],
          priorityBoarding: true,
          selectedMeals: ['vegetarian', 'standard'],
          travelInsurance: 'basic'
        }
      }
    ];

    for (const scenario of pricingScenarios) {
      console.log(`\n💸 Pricing: ${scenario.name}`);

      const result = await this.decisions['flight-pricing-calculator'].evaluate(scenario.data);

      console.log(`   Base fare: $${result.pricing.baseFare}`);
      console.log(`   Baggage fees: $${result.pricing.baggageFees}`);
      console.log(`   Ancillary fees: $${result.pricing.ancillaryFees}`);
      console.log(`   Taxes & fees: $${result.pricing.taxesAndFees}`);
      console.log(`   Insurance: $${result.pricing.insuranceFee}`);
      console.log(`   🎯 Total: $${result.pricing.grandTotal}`);
    }
  }

  async runErrorHandlingDemo() {
    console.log('\n🚨 Demo: Error Handling\n');
    console.log('=' .repeat(40));

    const errorScenarios = [
      {
        name: 'Payment failure - Insufficient funds',
        data: {
          errorType: 'payment',
          errorDetails: {
            paymentErrorCode: 'INSUFFICIENT_FUNDS',
            paymentMethodType: 'credit_card'
          },
          currentStep: 'payment',
          retryCount: 1
        }
      },
      {
        name: 'Flight sold out during booking',
        data: {
          errorType: 'availability',
          errorDetails: {
            availabilityIssue: 'FLIGHT_SOLD_OUT'
          },
          currentStep: 'outbound_flight_selection',
          retryCount: 0
        }
      },
      {
        name: 'System error - Database timeout',
        data: {
          errorType: 'system',
          errorDetails: {
            systemErrorType: 'DATABASE_CONNECTION',
            severity: 'high'
          },
          currentStep: 'passenger_details',
          retryCount: 0
        }
      },
      {
        name: 'Business rule violation - Unaccompanied minor',
        data: {
          errorType: 'business_rule',
          errorDetails: {
            ruleViolation: 'UNACCOMPANIED_MINOR_POLICY',
            passengerType: 'minor'
          },
          currentStep: 'passenger_details',
          retryCount: 0
        }
      }
    ];

    for (const scenario of errorScenarios) {
      console.log(`\n⚠️  Error: ${scenario.name}`);

      const result = await this.decisions['flight-error-handling'].evaluate(scenario.data);

      console.log(`   Recovery action: ${result.recoveryAction}`);
      console.log(`   User message: "${result.userMessage}"`);
      console.log(`   Suggested next step: ${result.suggestedNextStep}`);
      console.log(`   Can retry: ${result.canRetry ? 'Yes' : 'No'}`);
      if (result.booking.held) {
        console.log(`   Booking held until: ${result.booking.expiresAt}`);
      }
      if (result.assistance.required) {
        console.log(`   🆘 Human assistance required (Priority: ${result.assistance.priority})`);
      }
    }
  }

  async runPerformanceTest() {
    console.log('\n⚡ Performance Test\n');
    console.log('=' .repeat(30));

    const testData = {
      currentStep: 'search_criteria',
      userId: 'perf_test_user',
      capturedData: {
        origin: 'JFK',
        destination: 'LAX',
        departureDate: '2024-06-15',
        tripType: 'round-trip',
        passengers: { adults: 2, total: 2 }
      }
    };

    const iterations = 1000;
    console.log(`Running ${iterations} orchestrator evaluations...`);

    const startTime = Date.now();

    for (let i = 0; i < iterations; i++) {
      await this.decisions['flight-booking-orchestrator'].evaluate(testData);
    }

    const endTime = Date.now();
    const totalTime = endTime - startTime;
    const avgTime = totalTime / iterations;

    console.log(`✅ Completed ${iterations} evaluations`);
    console.log(`   Total time: ${totalTime}ms`);
    console.log(`   Average time per evaluation: ${avgTime.toFixed(2)}ms`);
    console.log(`   Evaluations per second: ${(1000 / avgTime).toFixed(0)}`);
  }

  async runAllDemos() {
    try {
      await this.initialize();
      await this.runCompleteJourneyDemo();
      await this.runValidationDemo();
      await this.runPricingDemo();
      await this.runErrorHandlingDemo();
      await this.runPerformanceTest();

      console.log('\n🎊 All demos completed successfully!');
      console.log('\n📚 Next steps:');
      console.log('   • Explore individual JDM files in detail');
      console.log('   • Customize business rules for your requirements');
      console.log('   • Integrate with your existing systems');
      console.log('   • Deploy using GoRules BRMS for production use');

    } catch (error) {
      console.error('\n💥 Demo failed:', error.message);
      console.error(error.stack);
    }
  }
}

// Handle both direct execution and module import
if (process.argv[1] === new URL(import.meta.url).pathname) {
  const demo = new FlightBookingJDMDemo();
  demo.runAllDemos();
}

export default FlightBookingJDMDemo;
