import { Check, LoaderCircle, TriangleAlert } from 'lucide-react';
import { useEffect, useMemo, useSyncExternalStore } from 'react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { EndpointValidator } from '@/utils/endpoint-validator';

const ENDPOINT_DEBOUNCE_MS = 1_500;

export type ApiEndpointAvailability = 'checking' | 'error' | 'healthy' | 'typing';

interface ApiEndpointFieldProps {
  activeEndpoint: string;
  pendingEndpoint: string | null;
  roomEndpoint: string | null;
  validationRevision: number;
  disabled: boolean;
  onValidated: (endpoint: string) => void;
  onReset: () => void;
}

export const ApiEndpointField = ({
  activeEndpoint,
  pendingEndpoint,
  roomEndpoint,
  validationRevision,
  disabled,
  onValidated,
  onReset,
}: ApiEndpointFieldProps): React.ReactNode => {
  const sourceEndpoint = pendingEndpoint ?? activeEndpoint;
  const validator = useMemo(() => new EndpointValidator(sourceEndpoint), [sourceEndpoint]);
  const { availability, draft, failure } = useSyncExternalStore(validator.subscribe, validator.getSnapshot);
  const value = draft ?? sourceEndpoint;

  useEffect(() => {
    validator.setOnValidated(onValidated);
  }, [onValidated, validator]);

  useEffect(() => {
    // A revision bump re-checks the endpoint even when the endpoint text is unchanged.
    validator.setRevision(validationRevision);
    void validator.validate(sourceEndpoint);
    return () => {
      validator.cancelPending();
    };
  }, [sourceEndpoint, validationRevision, validator]);

  const handleChange = (next: string): void => {
    validator.scheduleValidation(next, ENDPOINT_DEBOUNCE_MS);
  };

  const handleReset = (): void => {
    onReset();
    void validator.validate(activeEndpoint);
  };

  const isPending = pendingEndpoint !== null && value === pendingEndpoint;
  const canReset = failure !== null || isPending || value !== activeEndpoint;
  let statusMessage = 'Enter the base URL for your Slopcast API.';
  if (availability === 'checking') {
    statusMessage = 'Checking endpoint…';
  }
  if (availability === 'healthy') {
    statusMessage = 'Connected';
  }
  if (isPending) {
    statusMessage = 'Validated. Applies when this room closes.';
  }
  if (failure) {
    statusMessage = failure.message;
  }

  return (
    <div className="space-y-1.5">
      <label
        htmlFor="api-endpoint"
        className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
      >
        API Endpoint
      </label>
      <div className="relative">
        <Input
          id="api-endpoint"
          type="url"
          value={value}
          onChange={(event) => {
            handleChange(event.target.value);
          }}
          onBlur={() => void validator.validate(value)}
          onKeyDown={(event) => {
            if (event.key !== 'Enter') {
              return;
            }
            event.preventDefault();
            void validator.validate(value);
          }}
          disabled={disabled}
          aria-invalid={failure !== null}
          aria-describedby="api-endpoint-status"
          placeholder="http://localhost:3001"
          className={`w-full pr-10 text-sm text-foreground ${failure ? 'border-destructive focus-visible:ring-destructive' : ''}`}
        />
        <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center" aria-hidden="true">
          {availability === 'checking' && <LoaderCircle className="size-4 animate-spin text-muted-foreground" />}
          {availability === 'healthy' && !failure && <Check className="size-4 text-safelight" />}
          {failure && <TriangleAlert className="size-4 text-destructive" />}
        </span>
      </div>
      <div className="flex min-h-5 items-start justify-between gap-3">
        <output id="api-endpoint-status" aria-live="polite" className="block min-w-0 space-y-0.5">
          <span className={`block text-xs leading-relaxed ${failure ? 'text-destructive' : 'text-muted-foreground'}`}>
            {statusMessage}
          </span>
          {isPending && roomEndpoint !== null && roomEndpoint !== '' && (
            <span className="block break-all text-xs leading-relaxed text-caption-text">
              Current room: {roomEndpoint}
            </span>
          )}
        </output>
        {canReset && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={handleReset}
            disabled={disabled}
            className="shrink-0"
          >
            Use current endpoint
          </Button>
        )}
      </div>
    </div>
  );
};
