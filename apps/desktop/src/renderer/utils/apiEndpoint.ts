export type ApiEndpointErrorKind = 'invalid' | 'timeout' | 'unreachable' | 'unexpected' | 'unhealthy';

export interface ApiEndpointCheckSuccess {
  ok: true;
  endpoint: string;
}

export interface ApiEndpointCheckFailure {
  ok: false;
  kind: ApiEndpointErrorKind;
  message: string;
}

export type ApiEndpointCheckResult = ApiEndpointCheckSuccess | ApiEndpointCheckFailure;

const failure = (kind: ApiEndpointErrorKind, message: string): ApiEndpointCheckFailure => ({
  ok: false,
  kind,
  message,
});

export function normalizeApiEndpoint(input: string): ApiEndpointCheckResult {
  const trimmed = input.trim();
  if (!trimmed) return failure('invalid', 'Enter an API endpoint.');

  let parsed: URL;
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

  parsed.pathname = parsed.pathname.replace(/\/+$/, '');
  return { ok: true, endpoint: parsed.toString().replace(/\/$/, '') };
}

export async function checkApiEndpoint(
  input: string,
  signal: AbortSignal,
  fetcher: typeof fetch = fetch,
): Promise<ApiEndpointCheckResult> {
  const normalized = normalizeApiEndpoint(input);
  if (!normalized.ok) return normalized;

  let response: Response;
  try {
    response = await fetcher(`${normalized.endpoint}/api/health`, { signal });
  } catch {
    if (signal.aborted) return failure('timeout', 'The endpoint did not respond within 5 seconds.');
    return failure('unreachable', 'The server could not be reached. Check the URL and your network.');
  }
  if (response.status === 503) {
    return failure('unhealthy', 'The API is online, but its LiveKit service is unavailable.');
  }
  if (response.status !== 200) {
    return failure('unexpected', `The endpoint returned HTTP ${response.status} instead of a healthy response.`);
  }

  try {
    // SAFETY: the Slopcast health endpoint uses this JSON contract.
    const body = (await response.json()) as { status?: string };
    if (body.status === 'ok') return normalized;
  } catch {
    return failure('unexpected', 'The endpoint returned an unreadable health response.');
  }

  return failure('unexpected', 'The endpoint did not identify itself as a healthy Slopcast API.');
}
