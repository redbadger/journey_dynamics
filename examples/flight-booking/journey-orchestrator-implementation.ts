// Journey Orchestrator Implementation
// A dynamic workflow engine for flight booking process

export interface JourneyStep {
  id: string;
  name: string;
  description: string;
  requiredData: string[];
  optionalData: string[];
  validationRules: ValidationRule[];
  nextStepRules: NextStepRule[];
}

export interface ValidationRule {
  field: string;
  type: 'required' | 'format' | 'range' | 'custom';
  validator: (value: any) => boolean;
  errorMessage: string;
}

export interface NextStepRule {
  condition: (journeyState: JourneyState) => boolean;
  nextStepId: string;
  priority: number;
}

export interface JourneyState {
  sessionId: string;
  currentStepId: string;
  userId?: string;
  capturedData: Record<string, any>;
  stepHistory: string[];
  timestamp: Date;
  metadata: Record<string, any>;
}

export interface StepResult {
  success: boolean;
  capturedData: Record<string, any>;
  validationErrors: string[];
  nextSteps: string[];
}

export class JourneyOrchestrator {
  private steps: Map<string, JourneyStep> = new Map();
  private journeyStates: Map<string, JourneyState> = new Map();

  constructor() {
    this.initializeFlightBookingSteps();
  }

  // Initialize all flight booking steps
  private initializeFlightBookingSteps(): void {
    // Step 1: Journey Start
    this.addStep({
      id: 'journey_start',
      name: 'Journey Start',
      description: 'Entry point for booking process',
      requiredData: [],
      optionalData: ['userId', 'channel'],
      validationRules: [],
      nextStepRules: [
        {
          condition: (state) => !!state.capturedData.userId,
          nextStepId: 'search_criteria',
          priority: 1
        },
        {
          condition: (state) => !state.capturedData.userId,
          nextStepId: 'search_criteria',
          priority: 2
        }
      ]
    });

    // Step 2: Search Criteria
    this.addStep({
      id: 'search_criteria',
      name: 'Search Criteria',
      description: 'Capture flight search parameters',
      requiredData: ['origin', 'destination', 'departureDate', 'passengers', 'tripType'],
      optionalData: ['returnDate', 'cabinClass'],
      validationRules: [
        {
          field: 'origin',
          type: 'required',
          validator: (value) => !!value && value.length === 3,
          errorMessage: 'Valid origin airport code required'
        },
        {
          field: 'destination',
          type: 'required',
          validator: (value) => !!value && value.length === 3,
          errorMessage: 'Valid destination airport code required'
        },
        {
          field: 'departureDate',
          type: 'format',
          validator: (value) => new Date(value) > new Date(),
          errorMessage: 'Departure date must be in the future'
        },
        {
          field: 'passengers',
          type: 'range',
          validator: (value) => value.total > 0 && value.total <= 9,
          errorMessage: 'Number of passengers must be between 1 and 9'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => state.capturedData.tripType === 'multi-city',
          nextStepId: 'multi_city_details',
          priority: 1
        },
        {
          condition: (state) => state.capturedData.passengers?.total > 9,
          nextStepId: 'group_booking',
          priority: 1
        },
        {
          condition: (state) => this.isSearchCriteriaComplete(state),
          nextStepId: 'flight_search_results',
          priority: 2
        }
      ]
    });

    // Step 3: Flight Search Results
    this.addStep({
      id: 'flight_search_results',
      name: 'Flight Search Results',
      description: 'Display available flights',
      requiredData: [],
      optionalData: ['filters', 'sortPreference'],
      validationRules: [],
      nextStepRules: [
        {
          condition: (state) => (state.capturedData.searchResults?.length || 0) > 0,
          nextStepId: 'outbound_flight_selection',
          priority: 1
        },
        {
          condition: (state) => (state.capturedData.searchResults?.length || 0) === 0,
          nextStepId: 'alternative_search_suggestions',
          priority: 1
        }
      ]
    });

    // Step 4: Outbound Flight Selection
    this.addStep({
      id: 'outbound_flight_selection',
      name: 'Outbound Flight Selection',
      description: 'Select departing flight',
      requiredData: ['selectedOutboundFlight', 'fareType'],
      optionalData: [],
      validationRules: [
        {
          field: 'selectedOutboundFlight',
          type: 'required',
          validator: (value) => !!value && !!value.flightNumber,
          errorMessage: 'Please select an outbound flight'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => state.capturedData.tripType === 'round-trip',
          nextStepId: 'return_flight_selection',
          priority: 1
        },
        {
          condition: (state) => state.capturedData.tripType === 'one-way',
          nextStepId: 'passenger_details',
          priority: 1
        }
      ]
    });

    // Step 5: Return Flight Selection
    this.addStep({
      id: 'return_flight_selection',
      name: 'Return Flight Selection',
      description: 'Select return flight',
      requiredData: ['selectedReturnFlight'],
      optionalData: [],
      validationRules: [
        {
          field: 'selectedReturnFlight',
          type: 'required',
          validator: (value) => !!value && !!value.flightNumber,
          errorMessage: 'Please select a return flight'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => !!state.capturedData.selectedReturnFlight,
          nextStepId: 'passenger_details',
          priority: 1
        }
      ]
    });

    // Step 6: Passenger Details
    this.addStep({
      id: 'passenger_details',
      name: 'Passenger Details',
      description: 'Capture passenger information',
      requiredData: ['passengers'],
      optionalData: ['specialRequests'],
      validationRules: [
        {
          field: 'passengers',
          type: 'custom',
          validator: (value) => this.validatePassengerDetails(value),
          errorMessage: 'Complete passenger details required for all travelers'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => this.hasUnaccompaniedMinors(state),
          nextStepId: 'unaccompanied_minor_services',
          priority: 1
        },
        {
          condition: (state) => this.arePassengerDetailsComplete(state),
          nextStepId: 'seat_selection',
          priority: 2
        }
      ]
    });

    // Step 7: Seat Selection
    this.addStep({
      id: 'seat_selection',
      name: 'Seat Selection',
      description: 'Choose seats for passengers',
      requiredData: [],
      optionalData: ['seatAssignments', 'seatUpgrades'],
      validationRules: [],
      nextStepRules: [
        {
          condition: () => true,
          nextStepId: 'ancillary_services',
          priority: 1
        }
      ]
    });

    // Step 8: Ancillary Services
    this.addStep({
      id: 'ancillary_services',
      name: 'Ancillary Services',
      description: 'Additional services and products',
      requiredData: [],
      optionalData: ['baggage', 'meals', 'insurance', 'priorityBoarding', 'loungeAccess'],
      validationRules: [],
      nextStepRules: [
        {
          condition: (state) => this.shouldConfirmInsurance(state),
          nextStepId: 'insurance_confirmation',
          priority: 1
        },
        {
          condition: () => true,
          nextStepId: 'booking_summary',
          priority: 2
        }
      ]
    });

    // Step 9: Booking Summary
    this.addStep({
      id: 'booking_summary',
      name: 'Booking Summary',
      description: 'Review complete booking',
      requiredData: ['termsAccepted'],
      optionalData: [],
      validationRules: [
        {
          field: 'termsAccepted',
          type: 'required',
          validator: (value) => value === true,
          errorMessage: 'You must accept terms and conditions'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => !state.capturedData.userId,
          nextStepId: 'account_creation',
          priority: 1
        },
        {
          condition: (state) => state.capturedData.termsAccepted,
          nextStepId: 'payment',
          priority: 2
        }
      ]
    });

    // Step 10: Payment
    this.addStep({
      id: 'payment',
      name: 'Payment',
      description: 'Process booking payment',
      requiredData: ['paymentMethod', 'billingAddress'],
      optionalData: [],
      validationRules: [
        {
          field: 'paymentMethod',
          type: 'custom',
          validator: (value) => this.validatePaymentMethod(value),
          errorMessage: 'Valid payment method required'
        }
      ],
      nextStepRules: [
        {
          condition: (state) => state.capturedData.paymentStatus === 'success',
          nextStepId: 'booking_confirmation',
          priority: 1
        },
        {
          condition: (state) => state.capturedData.paymentStatus === 'failed',
          nextStepId: 'payment_retry',
          priority: 1
        }
      ]
    });

    // Step 11: Booking Confirmation
    this.addStep({
      id: 'booking_confirmation',
      name: 'Booking Confirmation',
      description: 'Confirm successful booking',
      requiredData: [],
      optionalData: [],
      validationRules: [],
      nextStepRules: [
        {
          condition: () => true,
          nextStepId: 'journey_end',
          priority: 1
        }
      ]
    });
  }

  // Add a step to the orchestrator
  addStep(step: JourneyStep): void {
    this.steps.set(step.id, step);
  }

  // Start a new journey
  startJourney(sessionId: string, initialData: Record<string, any> = {}): JourneyState {
    const journeyState: JourneyState = {
      sessionId,
      currentStepId: 'journey_start',
      capturedData: { ...initialData, timestamp: new Date() },
      stepHistory: ['journey_start'],
      timestamp: new Date(),
      metadata: {}
    };

    this.journeyStates.set(sessionId, journeyState);
    return journeyState;
  }

  // Process a step with captured data
  processStep(sessionId: string, stepData: Record<string, any>): StepResult {
    const journeyState = this.journeyStates.get(sessionId);
    if (!journeyState) {
      throw new Error(`Journey state not found for session: ${sessionId}`);
    }

    const currentStep = this.steps.get(journeyState.currentStepId);
    if (!currentStep) {
      throw new Error(`Step not found: ${journeyState.currentStepId}`);
    }

    // Validate the step data
    const validationErrors = this.validateStepData(currentStep, stepData);
    if (validationErrors.length > 0) {
      return {
        success: false,
        capturedData: {},
        validationErrors,
        nextSteps: []
      };
    }

    // Update journey state with captured data
    journeyState.capturedData = { ...journeyState.capturedData, ...stepData };
    journeyState.timestamp = new Date();

    // Determine next available steps
    const nextSteps = this.getNextSteps(journeyState, currentStep);

    return {
      success: true,
      capturedData: stepData,
      validationErrors: [],
      nextSteps
    };
  }

  // Move to the next step
  moveToStep(sessionId: string, nextStepId: string): JourneyState {
    const journeyState = this.journeyStates.get(sessionId);
    if (!journeyState) {
      throw new Error(`Journey state not found for session: ${sessionId}`);
    }

    const nextStep = this.steps.get(nextStepId);
    if (!nextStep) {
      throw new Error(`Step not found: ${nextStepId}`);
    }

    journeyState.currentStepId = nextStepId;
    journeyState.stepHistory.push(nextStepId);
    journeyState.timestamp = new Date();

    this.journeyStates.set(sessionId, journeyState);
    return journeyState;
  }

  // Get current journey state
  getJourneyState(sessionId: string): JourneyState | undefined {
    return this.journeyStates.get(sessionId);
  }

  // Validate step data against rules
  private validateStepData(step: JourneyStep, data: Record<string, any>): string[] {
    const errors: string[] = [];

    // Check required fields
    for (const requiredField of step.requiredData) {
      if (!data[requiredField]) {
        errors.push(`${requiredField} is required`);
      }
    }

    // Run validation rules
    for (const rule of step.validationRules) {
      const value = data[rule.field];
      if (value !== undefined && !rule.validator(value)) {
        errors.push(rule.errorMessage);
      }
    }

    return errors;
  }

  // Determine next available steps based on rules
  private getNextSteps(journeyState: JourneyState, currentStep: JourneyStep): string[] {
    const availableSteps: Array<{stepId: string, priority: number}> = [];

    for (const rule of currentStep.nextStepRules) {
      if (rule.condition(journeyState)) {
        availableSteps.push({
          stepId: rule.nextStepId,
          priority: rule.priority
        });
      }
    }

    // Sort by priority and return step IDs
    return availableSteps
      .sort((a, b) => a.priority - b.priority)
      .map(step => step.stepId);
  }

  // Helper validation methods
  private isSearchCriteriaComplete(state: JourneyState): boolean {
    const data = state.capturedData;
    return !!(data.origin && data.destination && data.departureDate && data.passengers);
  }

  private validatePassengerDetails(passengers: any[]): boolean {
    if (!Array.isArray(passengers) || passengers.length === 0) {
      return false;
    }

    return passengers.every(passenger =>
      passenger.firstName &&
      passenger.lastName &&
      passenger.dateOfBirth &&
      passenger.documentNumber
    );
  }

  private hasUnaccompaniedMinors(state: JourneyState): boolean {
    const passengers = state.capturedData.passengers || [];
    return passengers.some((p: any) =>
      this.calculateAge(p.dateOfBirth) < 18 && p.travelingAlone
    );
  }

  private arePassengerDetailsComplete(state: JourneyState): boolean {
    return this.validatePassengerDetails(state.capturedData.passengers);
  }

  private shouldConfirmInsurance(state: JourneyState): boolean {
    return !state.capturedData.insurance && this.isInternationalFlight(state);
  }

  private isInternationalFlight(state: JourneyState): boolean {
    // Simple check - in real implementation, would check airport countries
    const origin = state.capturedData.origin;
    const destination = state.capturedData.destination;
    return origin && destination && origin !== destination;
  }

  private validatePaymentMethod(paymentMethod: any): boolean {
    return !!(paymentMethod && paymentMethod.type && paymentMethod.details);
  }

  private calculateAge(dateOfBirth: string): number {
    const today = new Date();
    const birthDate = new Date(dateOfBirth);
    let age = today.getFullYear() - birthDate.getFullYear();
    const monthDiff = today.getMonth() - birthDate.getMonth();

    if (monthDiff < 0 || (monthDiff === 0 && today.getDate() < birthDate.getDate())) {
      age--;
    }

    return age;
  }
}

// Usage example
export class FlightBookingService {
  private orchestrator: JourneyOrchestrator;

  constructor() {
    this.orchestrator = new JourneyOrchestrator();
  }

  // Start a new booking journey
  async startBooking(sessionId: string, userId?: string): Promise<JourneyState> {
    const initialData = userId ? { userId } : {};
    return this.orchestrator.startJourney(sessionId, initialData);
  }

  // Process user input for current step
  async processUserInput(sessionId: string, userInput: Record<string, any>): Promise<StepResult> {
    return this.orchestrator.processStep(sessionId, userInput);
  }

  // Move to specific step
  async navigateToStep(sessionId: string, stepId: string): Promise<JourneyState> {
    return this.orchestrator.moveToStep(sessionId, stepId);
  }

  // Get current journey status
  async getJourneyStatus(sessionId: string): Promise<JourneyState | null> {
    return this.orchestrator.getJourneyState(sessionId) || null;
  }
}
