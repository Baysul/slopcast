type Listener = () => void;

export type EndpointHealthAvailability = 'checking' | 'error' | 'healthy';

export interface ApiEndpointHealthState {
  readonly availability: EndpointHealthAvailability;
  readonly endpoint: string | null;
  readonly healthy: boolean;
  readonly revision: number;
}

const initialHealthState: ApiEndpointHealthState = {
  availability: 'checking',
  endpoint: null,
  healthy: false,
  revision: 0,
};

/**
 * Health of the API endpoint the app is validating.
 *
 * The check itself is an HTTP request, so its outcome lives outside React and components read it
 * with `useSyncExternalStore`.
 */
class ApiEndpointHealthStore {
  private state: ApiEndpointHealthState = initialHealthState;
  private readonly listeners = new Set<Listener>();

  public getSnapshot = (): ApiEndpointHealthState => this.state;

  public subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  public markChecking = (endpoint: string, revision: number): void => {
    this.emit({ availability: 'checking', endpoint, healthy: false, revision });
  };

  public markResult = (endpoint: string, revision: number, ok: boolean): void => {
    if (revision !== this.state.revision) {
      return;
    }
    let availability: EndpointHealthAvailability = 'error';
    if (ok) {
      availability = 'healthy';
    }
    this.emit({ availability, endpoint, healthy: ok, revision });
  };

  public markHealthy = (endpoint: string): void => {
    this.emit({
      ...this.state,
      availability: 'healthy',
      endpoint,
      healthy: true,
    });
  };

  private emit(next: ApiEndpointHealthState): void {
    this.state = next;
    for (const listener of this.listeners) {
      listener();
    }
  }
}

export const apiEndpointHealthStore = new ApiEndpointHealthStore();
