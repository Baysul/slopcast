import { isJsonObject, readString } from '@slopcast/shared-types';

export type ApiEndpointErrorKind = 'invalid' | 'timeout' | 'unreachable' | 'unexpected' | 'unhealthy';

export interface ApiEndpointCheckSuccess {
  readonly ok: true;
  readonly endpoint: string;
}

export interface ApiEndpointCheckFailure {
  readonly ok: false;
  readonly kind: ApiEndpointErrorKind;
  readonly message: string;
}

export type ApiEndpointCheckResult = ApiEndpointCheckSuccess | ApiEndpointCheckFailure;

const HTTP_OK = 200;
const HTTP_SERVICE_UNAVAILABLE = 503;

const failure = (kind: ApiEndpointErrorKind, message: string): ApiEndpointCheckFailure => ({
  kind,
  message,
  ok: false,
});

export const normalizeApiEndpoint = (input: string): ApiEndpointCheckResult => {
  const trimmed = input.trim();
  if (!trimmed) {
    return failure('invalid', 'Enter an API endpoint.');
  }

  let parsed: URL | undefined = undefined;
  try {
    parsed = new URL(trimmed);
  } catch {
    return failure('invalid', 'Enter a valid URL, such as https://share.example.com.');
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    return failure('invalid', 'Use an http:// or https:// endpoint.');
  }
  if (!parsed.hostname || parsed.username || parsed.password || parsed.search || parsed.hash) {
    return failure('invalid', 'Use a base URL without credentials, a query, or a fragment.');
  }

  parsed.pathname = parsed.pathname.replace(/\/+$/u, '');
  return { endpoint: parsed.toString().replace(/\/$/u, ''), ok: true };
};

export const checkApiEndpoint = async (
  input: string,
  signal: AbortSignal,
  fetcher: typeof fetch = fetch,
): Promise<ApiEndpointCheckResult> => {
  const normalized = normalizeApiEndpoint(input);
  if (!normalized.ok) {
    return normalized;
  }

  let response: Response | undefined = undefined;
  try {
    response = await fetcher(`${normalized.endpoint}/api/health`, { signal });
  } catch {
    if (signal.aborted) {
      return failure('timeout', 'The endpoint did not respond within 5 seconds.');
    }
    return failure('unreachable', 'The server could not be reached. Check the URL and your network.');
  }
  if (response.status === HTTP_SERVICE_UNAVAILABLE) {
    return failure('unhealthy', 'The API is online, but its LiveKit service is unavailable.');
  }
  if (response.status !== HTTP_OK) {
    return failure('unexpected', `The endpoint returned HTTP ${response.status} instead of a healthy response.`);
  }

  try {
    const body: unknown = await response.json();
    if (isJsonObject(body) && readString(body, 'status') === 'ok') {
      return normalized;
    }
  } catch {
    return failure('unexpected', 'The endpoint returned an unreadable health response.');
  }

  return failure('unexpected', 'The endpoint did not identify itself as a healthy Slopcast API.');
};
