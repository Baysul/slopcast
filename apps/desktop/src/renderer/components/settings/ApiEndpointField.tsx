import { Check, LoaderCircle, TriangleAlert } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { type ApiEndpointCheckFailure, checkApiEndpoint } from '@/utils/apiEndpoint';

const ENDPOINT_DEBOUNCE_MS = 1_500;
const ENDPOINT_TIMEOUT_MS = 5_000;

export type ApiEndpointAvailability = 'checking' | 'error' | 'healthy' | 'typing';

interface ApiEndpointFieldProps {
  activeEndpoint: string;
  pendingEndpoint: string | null;
  roomEndpoint: string | null;
  validationRevision: number;
  disabled: boolean;
  onAvailabilityChange: (availability: ApiEndpointAvailability, endpoint: string) => void;
  onValidated: (endpoint: string) => void;
  onReset: () => void;
}

export function ApiEndpointField({
  activeEndpoint,
  pendingEndpoint,
  roomEndpoint,
  validationRevision,
  disabled,
  onAvailabilityChange,
  onValidated,
  onReset,
}: ApiEndpointFieldProps): React.ReactNode {
  const sourceEndpoint = pendingEndpoint ?? activeEndpoint;
  const [draft, setDraft] = useState(sourceEndpoint);
  const [availability, setAvailability] = useState<ApiEndpointAvailability>('checking');
  const [failure, setFailure] = useState<ApiEndpointCheckFailure | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const requestRef = useRef(0);

  const clearPendingWork = useCallback((): void => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = null;
    abortRef.current?.abort();
    abortRef.current = null;
    requestRef.current += 1;
  }, []);

  const validate = useCallback(
    async (endpoint: string): Promise<void> => {
      clearPendingWork();
      const request = requestRef.current + 1;
      const controller = new AbortController();
      requestRef.current = request;
      abortRef.current = controller;
      setFailure(null);
      setAvailability('checking');
      onAvailabilityChange('checking', endpoint);

      const timeout = setTimeout(() => controller.abort(), ENDPOINT_TIMEOUT_MS);
      const result = await checkApiEndpoint(endpoint, controller.signal);
      clearTimeout(timeout);
      if (requestRef.current !== request) return;
      abortRef.current = null;

      if (!result.ok) {
        setFailure(result);
        setAvailability('error');
        onAvailabilityChange('error', endpoint);
        return;
      }

      setDraft(result.endpoint);
      setAvailability('healthy');
      onAvailabilityChange('healthy', result.endpoint);
      onValidated(result.endpoint);
    },
    [clearPendingWork, onAvailabilityChange, onValidated],
  );

  useEffect(() => {
    void validationRevision;
    setDraft(sourceEndpoint);
    void validate(sourceEndpoint);
    return clearPendingWork;
  }, [sourceEndpoint, validationRevision, validate, clearPendingWork]);

  const handleChange = (value: string): void => {
    clearPendingWork();
    setDraft(value);
    setFailure(null);
    setAvailability('typing');
    onAvailabilityChange('typing', value);
    debounceRef.current = setTimeout(() => void validate(value), ENDPOINT_DEBOUNCE_MS);
  };

  const handleReset = (): void => {
    onReset();
    setDraft(activeEndpoint);
    void validate(activeEndpoint);
  };

  const isPending = pendingEndpoint !== null && draft === pendingEndpoint;
  const canReset = failure !== null || isPending || draft !== activeEndpoint;
  let statusMessage = 'Enter the base URL for your Slopcast API.';
  if (availability === 'checking') statusMessage = 'Checking endpoint…';
  if (availability === 'healthy') statusMessage = 'Connected';
  if (isPending) statusMessage = 'Validated. Applies when this room closes.';
  if (failure) statusMessage = failure.message;

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
          value={draft}
          onChange={(event) => handleChange(event.target.value)}
          onBlur={() => void validate(draft)}
          onKeyDown={(event) => {
            if (event.key !== 'Enter') return;
            event.preventDefault();
            void validate(draft);
          }}
          disabled={disabled}
          aria-invalid={failure !== null}
          aria-describedby="api-endpoint-status"
          placeholder="http://localhost:3001"
          className={`w-full bg-secondary pr-10 text-sm text-foreground ${failure ? 'border-destructive focus-visible:ring-destructive' : ''}`}
        />
        <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center" aria-hidden="true">
          {availability === 'checking' && <LoaderCircle className="size-4 animate-spin text-muted-foreground" />}
          {availability === 'healthy' && !failure && <Check className="size-4 text-safelight" />}
          {failure && <TriangleAlert className="size-4 text-destructive" />}
        </span>
      </div>
      <div className="flex min-h-5 items-start justify-between gap-3">
        <div id="api-endpoint-status" role="status" aria-live="polite" className="min-w-0 space-y-0.5">
          <p className={`text-xs leading-relaxed ${failure ? 'text-destructive' : 'text-muted-foreground'}`}>
            {statusMessage}
          </p>
          {isPending && roomEndpoint && (
            <p className="break-all text-xs leading-relaxed text-caption-text">Current room: {roomEndpoint}</p>
          )}
        </div>
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
}
