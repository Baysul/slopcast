import { checkApiEndpoint } from './apiEndpoint';
import type { ApiEndpointCheckFailure } from './apiEndpoint';

const ENDPOINT_TIMEOUT_MS = 5_000;
const INITIAL_REQUEST_ID = 0;
const REQUEST_INCREMENT = 1;

type Listener = () => void;

export type EndpointValidatorAvailability = 'checking' | 'error' | 'healthy' | 'typing';

export interface EndpointValidatorState {
  readonly availability: EndpointValidatorAvailability;
  readonly draft: string | null;
  readonly failure: ApiEndpointCheckFailure | null;
}

/**
 * Runs the "is this API endpoint reachable?" check for the settings field.
 *
 * The check is an HTTP request with its own revision tracking and abort handling, so it lives
 * outside React; the field renders the state with `useSyncExternalStore`.
 */
export class EndpointValidator {
  private state: EndpointValidatorState;
  private readonly listeners = new Set<Listener>();
  private onValidated: ((endpoint: string) => void) | null = null;
  private controller: AbortController | null = null;
  private timeout: ReturnType<typeof setTimeout> | null = null;
  private debounce: ReturnType<typeof setTimeout> | null = null;
  private request = INITIAL_REQUEST_ID;
  private revision = INITIAL_REQUEST_ID;

  public constructor(initialDraft: string) {
    this.state = { availability: 'typing', draft: initialDraft, failure: null };
  }

  public setOnValidated(onValidated: (endpoint: string) => void): void {
    this.onValidated = onValidated;
  }

  public getSnapshot = (): EndpointValidatorState => this.state;

  public subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  public setRevision(revision: number): void {
    this.revision = revision;
  }

  /** Records in-progress editing without starting a check. */
  public markTyping = (draft: string): void => {
    this.cancelPending();
    this.emit({ availability: 'typing', draft, failure: null });
  };

  /** Records in-progress editing and checks the draft once the user stops typing. */
  public scheduleValidation = (draft: string, delayMs: number): void => {
    this.markTyping(draft);
    this.debounce = setTimeout(() => {
      this.debounce = null;
      void this.validate(draft);
    }, delayMs);
  };

  public validate = async (endpoint: string): Promise<void> => {
    const { revision } = this;
    this.cancelPending();
    const request = this.request + REQUEST_INCREMENT;
    const controller = new AbortController();
    this.request = request;
    this.controller = controller;
    this.emit({ availability: 'checking', draft: endpoint, failure: null });
    const timeout = setTimeout(() => {
      controller.abort();
    }, ENDPOINT_TIMEOUT_MS);
    this.timeout = timeout;

    const result = await checkApiEndpoint(endpoint, controller.signal);
    clearTimeout(timeout);
    if (this.timeout === timeout) {
      this.timeout = null;
    }
    if (this.request !== request || this.revision !== revision) {
      return;
    }
    this.controller = null;

    if (!result.ok) {
      this.emit({ availability: 'error', draft: this.state.draft, failure: result });
      return;
    }

    this.emit({ availability: 'healthy', draft: result.endpoint, failure: null });
    this.onValidated?.(result.endpoint);
  };

  public cancelPending = (): void => {
    if (this.debounce !== null) {
      clearTimeout(this.debounce);
      this.debounce = null;
    }
    this.controller?.abort();
    this.controller = null;
    this.clearTimeout();
    this.request += REQUEST_INCREMENT;
  };

  private clearTimeout(): void {
    if (this.timeout !== null) {
      clearTimeout(this.timeout);
      this.timeout = null;
    }
  }

  private emit(next: EndpointValidatorState): void {
    this.state = next;
    for (const listener of this.listeners) {
      listener();
    }
  }
}
