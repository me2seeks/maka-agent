import { RetryError } from 'ai';
import { sanitizeUnicodeText } from '@maka/core';
import { redactSecrets } from '@maka/core/redaction';
import type { ModelFailure, ModelFailureKind } from './model-protocol.js';

/**
 * Structured provider error identifiers that mean the INPUT exceeded the
 * model's context window. These come from the provider's error JSON and are
 * the ONLY unconditional overflow evidence: free-text signals are vetoable.
 */
const CONTEXT_OVERFLOW_PROVIDER_CODES: ReadonlySet<string> = new Set([
  'context_length_exceeded', // OpenAI & OpenAI-compatible: error.code
  'model_context_window_exceeded', // z.ai: error.code
  'request_too_large', // Anthropic byte-size overflow (HTTP 413): error.type
]);

/** Stable account-state meanings providers expose through error code/type fields. */
const PROVIDER_BILLING_CODES: ReadonlySet<string> = new Set([
  'insufficient_quota',
  'payment_required',
]);
const PROVIDER_AUTH_CODES: ReadonlySet<string> = new Set([
  'authentication',
  'authentication_error',
  'invalid_api_key',
]);
const PROVIDER_PERMISSION_CODES: ReadonlySet<string> = new Set(['permission_denied']);
const PROVIDER_USAGE_LIMIT_CODES: ReadonlySet<string> = new Set(['usage_limit_reached']);
const PROVIDER_RATE_LIMIT_CODES: ReadonlySet<string> = new Set([
  'rate_limit_error',
  'rate_limit_exceeded',
  'rate_limited',
]);
const PROVIDER_UNAVAILABLE_CODES: ReadonlySet<string> = new Set([
  'overloaded_error',
  'provider_overloaded',
  'provider_unavailable',
]);

/**
 * A provider failure normalized into classification evidence. classifyError's
 * real input domain is NOT just Error instances: a request-level failure is
 * an AI SDK `APICallError` (provider JSON parsed in `data`, raw in
 * `responseBody`; no top-level `.code`), while an in-stream error part
 * carries the provider's parsed error VALUE — OpenAI Chat emits the inner
 * `{message, type?, code?}` object, OpenAI Responses the whole
 * `{type:'error', error:{type, code, message}}` chunk, Anthropic the inner
 * `{type, message}` object, and openai-compatible a bare message string.
 * Shapes read from the provider sources, never invented.
 */
interface ProviderErrorEvidence {
  /** Lowercased composite of the textual fields, for pattern evidence. */
  text: string;
  /** Explicit HTTP status from a field ('' when absent) — never a substring. */
  statusCode: string;
  /** Top-level code field as a string ('' when absent). */
  code: string;
  /** Structured provider identifiers (code/type/error_type/provider_code), lowercased. */
  structuredCodes: string[];
}

export interface ProviderRetryMetadata {
  retryable: boolean;
  retryAfterMs?: number;
}

const PROVIDER_DIAGNOSTIC_MAX_LENGTH = 2_000;

const MAX_SAFE_TIMER_DELAY_MS = 2_147_483_647;
const OPENAI_RESPONSES_WEBSOCKET_TRANSPORT_ERROR = 'OPENAI_RESPONSES_WEBSOCKET_TRANSPORT_ERROR';
const RUNTIME_RETRYABLE_ERROR_CODES: ReadonlySet<string> = new Set([
  OPENAI_RESPONSES_WEBSOCKET_TRANSPORT_ERROR,
  'OPENAI_RESPONSES_CONTINUATION_UNAVAILABLE',
]);

function providerErrorTarget(error: unknown): unknown {
  return RetryError.isInstance(error) && error.lastError !== undefined && error.lastError !== error
    ? error.lastError
    : error;
}

function responseHeadersFromError(error: unknown): Record<string, string> | undefined {
  if (typeof error !== 'object' || error === null) return undefined;
  const value = (error as { responseHeaders?: unknown }).responseHeaders;
  if (typeof value !== 'object' || value === null) return undefined;
  const headers: Record<string, string> = {};
  for (const [key, header] of Object.entries(value)) {
    if (typeof header === 'string') headers[key.toLowerCase()] = header;
  }
  return headers;
}

function parseRetryAfterMs(headers: Record<string, string>): number | null | undefined {
  const rawMilliseconds = headers['retry-after-ms'];
  const rawRetryAfter = headers['retry-after'];
  if (rawMilliseconds === undefined && rawRetryAfter === undefined) return undefined;

  let delayMs: number;
  if (rawMilliseconds !== undefined) {
    delayMs = Number(rawMilliseconds);
  } else {
    const seconds = Number(rawRetryAfter);
    delayMs = Number.isFinite(seconds) ? seconds * 1_000 : Date.parse(rawRetryAfter!) - Date.now();
  }
  if (!Number.isFinite(delayMs) || delayMs <= 0 || delayMs > MAX_SAFE_TIMER_DELAY_MS) return null;
  return Math.ceil(delayMs);
}

/**
 * Normalizes provider retry facts without leaking SDK error objects or raw
 * response headers across the ModelAdapter boundary.
 */
export function providerRetryMetadata(error: unknown): ProviderRetryMetadata {
  const target = providerErrorTarget(error);
  const evidence = normalizeErrorEvidence(target);
  if (!evidence) return { retryable: false };

  if (RUNTIME_RETRYABLE_ERROR_CODES.has(evidence.code)) return { retryable: true };

  const status = Number(evidence.statusCode || evidence.code);
  const errorClass = classifyError(target);
  // Account state cannot be repaired by immediately repeating the same
  // request, even when a provider reports it through HTTP 429.
  if (
    errorClass === 'Auth' ||
    errorClass === 'ProviderBilling' ||
    errorClass === 'ProviderPermission' ||
    errorClass === 'UsageLimit'
  ) {
    return { retryable: false };
  }
  const sdkRetryable =
    typeof target === 'object' &&
    target !== null &&
    typeof (target as { isRetryable?: unknown }).isRetryable === 'boolean'
      ? (target as { isRetryable: boolean }).isRetryable
      : undefined;
  const retryable =
    sdkRetryable ??
    (errorClass === 'Network' ||
      status === 408 ||
      status === 409 ||
      status === 429 ||
      (status >= 500 && status <= 599));
  if (!retryable) return { retryable: false };

  const retryAfterMs = parseRetryAfterMs(responseHeadersFromError(target) ?? {});
  if (retryAfterMs === null) return { retryable: false };
  return {
    retryable: true,
    ...(retryAfterMs !== undefined ? { retryAfterMs } : {}),
  };
}

/**
 * Closes the provider-error boundary in one place. Stable structured facts
 * choose Runtime policy; provider wording is only a redacted, bounded
 * diagnostic for the user and never feeds the failure kind or retry decision.
 */
export function normalizeProviderFailure(error: unknown): ModelFailure {
  const errorClass = classifyError(error);
  const retry = providerRetryMetadata(error);
  const target = providerErrorTarget(error);
  const code = target instanceof Error ? scalarString((target as { code?: unknown }).code) : '';
  return {
    type: 'model_failure',
    kind: modelFailureKind(errorClass),
    retryable: retry.retryable,
    ...(retry.retryAfterMs !== undefined ? { retryAfterMs: retry.retryAfterMs } : {}),
    ...(code ? { code } : {}),
    message: providerDiagnosticMessage(target) ?? defaultFailureMessage(errorClass),
  };
}

function providerDiagnosticMessage(error: unknown): string | undefined {
  const message = extractProviderMessage(error);
  if (!message) return undefined;
  // Run the shared redactor over both the whole message and whitespace-delimited
  // fragments. The second pass closes an overlap case such as
  // `failure: token=...`, where the generic `failure:` assignment can otherwise
  // consume the nested credential-shaped fragment before it is inspected.
  const redacted = redactSecrets(message)
    .split(/(\s+)/)
    .map((fragment) => (fragment.trim() ? redactSecrets(fragment) : fragment))
    .join('');
  const sanitized = sanitizeUnicodeText(redacted, {
    maxCodePoints: PROVIDER_DIAGNOSTIC_MAX_LENGTH,
  });
  return truncateUtf16Safely(sanitized, PROVIDER_DIAGNOSTIC_MAX_LENGTH) || undefined;
}

function truncateUtf16Safely(value: string, maxLength: number): string {
  if (value.length <= maxLength) return value;
  const contentLimit = maxLength - 1;
  let content = '';
  for (const codePoint of value) {
    if (content.length + codePoint.length > contentLimit) break;
    content += codePoint;
  }
  return `${content}…`;
}

function extractProviderMessage(value: unknown, depth = 0): string | undefined {
  if (depth > 4) return undefined;
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (!trimmed) return undefined;
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (parsed !== trimmed) return extractProviderMessage(parsed, depth + 1);
    } catch {
      // A stream error may itself be the provider's message.
    }
    return trimmed;
  }
  if (typeof value !== 'object' || value === null) return undefined;

  const record = value as Record<string, unknown>;
  if (value instanceof Error) {
    const fromData = extractProviderMessage(record.data, depth + 1);
    if (fromData) return fromData;
    const responseBody = record.responseBody;
    if (typeof responseBody === 'string') {
      try {
        const fromBody = extractProviderMessage(JSON.parse(responseBody), depth + 1);
        if (fromBody) return fromBody;
      } catch {
        // A non-JSON response body is not a diagnostic string boundary.
      }
    }
    return typeof value.message === 'string' && value.message.trim() ? value.message : undefined;
  }

  const nestedError = extractProviderMessage(record.error, depth + 1);
  if (nestedError) return nestedError;
  const nestedResponse = extractProviderMessage(record.response, depth + 1);
  if (nestedResponse) return nestedResponse;
  const nestedData = extractProviderMessage(record.data, depth + 1);
  if (nestedData) return nestedData;
  return typeof record.message === 'string' && record.message.trim() ? record.message : undefined;
}

/** Collects stable identifiers from the provider envelopes Maka receives. */
function collectStructuredCodes(payload: unknown, out: string[]): void {
  const fromRecord = (record: unknown) => {
    if (typeof record !== 'object' || record === null) return;
    for (const key of ['code', 'type', 'error_type', 'provider_code'] as const) {
      const value = (record as Record<string, unknown>)[key];
      if (typeof value === 'string' && value) out.push(value.toLowerCase());
    }
  };
  fromRecord(payload);
  if (typeof payload === 'object' && payload !== null) {
    const record = payload as Record<string, unknown>;
    fromRecord(record.metadata);
    fromRecord(record.error);
    if (typeof record.error === 'object' && record.error !== null) {
      fromRecord((record.error as Record<string, unknown>).metadata);
    }
    // Open Responses failed events wrap the canonical response object once.
    fromRecord(record.response);
    if (typeof record.response === 'object' && record.response !== null) {
      const response = record.response as Record<string, unknown>;
      fromRecord(response.error);
      if (typeof response.error === 'object' && response.error !== null) {
        fromRecord((response.error as Record<string, unknown>).metadata);
      }
    }
  }
}

function normalizeErrorEvidence(error: unknown): ProviderErrorEvidence | undefined {
  if (error instanceof Error) {
    const code = scalarString((error as { code?: unknown }).code);
    const statusCode =
      'statusCode' in error
        ? scalarString((error as { statusCode?: unknown }).statusCode)
        : 'status' in error
          ? scalarString((error as { status?: unknown }).status)
          : '';
    const rawBody = (error as { responseBody?: unknown }).responseBody;
    const body = typeof rawBody === 'string' ? rawBody : '';
    const structuredCodes: string[] = [];
    collectStructuredCodes(error, structuredCodes);
    collectStructuredCodes((error as { data?: unknown }).data, structuredCodes);
    if (body) {
      // The failed-response handler keeps the raw body even when the provider
      // JSON failed the schema (which is exactly when `data` is absent).
      try {
        collectStructuredCodes(JSON.parse(body), structuredCodes);
      } catch {
        // Not JSON — no structured evidence.
      }
    }
    return {
      // The raw body joins the text evidence: when the provider JSON fails
      // the error schema, `message` degrades to the statusText and the body
      // is the ONLY carrier of the provider's wording (e.g. an
      // OpenAI-compatible `{error: string}` overflow). Positives and vetoes
      // both run over the same full text.
      text: `${error.name} ${code} ${statusCode} ${error.message}${body ? ` ${body}` : ''}`.toLowerCase(),
      statusCode,
      code,
      structuredCodes,
    };
  }
  if (typeof error === 'string') {
    const structuredCodes: string[] = [];
    try {
      collectStructuredCodes(JSON.parse(error), structuredCodes);
    } catch {
      // A plain message string — text evidence only.
    }
    return { text: error.toLowerCase(), statusCode: '', code: '', structuredCodes };
  }
  if (typeof error === 'object' && error !== null) {
    const record = error as Record<string, unknown>;
    const field = (key: string): string => {
      const value = record[key];
      return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
    };
    const structuredCodes: string[] = [];
    collectStructuredCodes(record, structuredCodes);
    let text: string;
    try {
      // Serialize the whole value so message/code text is evidence no matter
      // which of the known provider shapes carried it.
      text = JSON.stringify(error).toLowerCase();
    } catch {
      text = String(error).toLowerCase();
    }
    return {
      text,
      statusCode: field('statusCode') || field('status'),
      code: field('code'),
      structuredCodes,
    };
  }
  return undefined;
}

function scalarString(value: unknown): string {
  return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
}

/**
 * Provider context-length overflow signatures. A request-level 400/413 whose
 * message matches one of these means the input exceeded the model's context
 * window — the reactive-recovery trigger (issue #882 PR 2). The set is ported
 * from pi's battle-tested table and covers the providers Maka ships in its
 * registry (Anthropic, OpenAI/-compatible, Google, xAI, Groq, OpenRouter,
 * Mistral, MiniMax, Kimi/Moonshot, Together, llama.cpp/LM Studio/Ollama, …).
 * Matched against the ORIGINAL error's composite fields (name, code, status,
 * message), never the generalized string. All of these are free-text evidence
 * and can be vetoed by NON_CONTEXT_OVERFLOW_PATTERNS: a capacity statement or
 * overflow phrase quoted inside a throttling/quota error must not trigger
 * recovery — only a structured provider code is unconditional.
 */
const CONTEXT_OVERFLOW_PATTERNS: readonly RegExp[] = [
  /prompt is too long/i, // Anthropic token overflow
  /request_too_large/i, // Anthropic request byte-size overflow (HTTP 413)
  /input is too long for requested model/i, // Amazon Bedrock
  /exceeds the context window/i, // OpenAI (Completions & Responses)
  /exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))?/i, // OpenAI-compatible proxies (LiteLLM)
  /input token count.*exceeds the maximum/i, // Google (Gemini)
  /maximum prompt length is \d+/i, // xAI (Grok)
  /reduce the length of the messages/i, // Groq
  /maximum context length is \d+ tokens/i, // OpenRouter (most backends)
  /exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i, // OpenRouter/Poolside
  /input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i, // Together AI
  // GitHub Copilot: "prompt token count of X exceeds the limit of Y". The INPUT
  // subject is required — a bare "token count of N exceeds the limit of M" also
  // matches output/completion caps, and a bare "exceeds the limit of N" matches
  // file-size and other quota errors; neither is fixable by history compaction.
  /(?:prompt|input|context|message)[^.]{0,80}token count of [\d,]+ exceeds the limit of [\d,]+/i,
  /exceeds the available context size/i, // llama.cpp server
  /greater than the context length/i, // LM Studio
  /context window exceeds limit/i, // MiniMax
  /exceeded model token limit/i, // Kimi For Coding
  /too large for model with \d+ maximum context length/i, // Mistral
  /prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i, // DS4 server
  /model_context_window_exceeded/i, // z.ai non-standard finish_reason surfaced as error text
  /prompt too long; exceeded (?:max )?context length/i, // Ollama explicit overflow error
  /context[_ ]length[_ ]exceeded/i, // OpenAI structured error code (also generic)
  // Ambiguous token-limit wording that is an input overflow only when an
  // input-like word is the subject. `request` is deliberately NOT in the
  // subject list: it appears in generic prefixes ("Invalid request: ...")
  // without saying anything about which side of the token budget overflowed.
  /(?:prompt|input|context|message)[^.]{0,80}too many tokens/i,
  /(?:prompt|input|context|message)[^.]{0,80}token limit exceeded/i,
];

/**
 * Wording that looks token-shaped but is NOT an input overflow: throttling /
 * quota / rate limiting, and complete OUTPUT-cap relations in every observed
 * permutation of role word (output/completion/max_tokens) and token
 * predicate — subject before predicate ("completion has too many tokens",
 * "max_tokens token limit exceeded"), predicate before subject ("too many
 * tokens were requested for the completion"), the count-of form ("output
 * token count of N exceeds"), the role word embedded inside the phrase
 * ("too many completion tokens were requested"), and the role-tokens-exceed
 * form ("Maximum completion tokens exceeded"). Noun phrases alone (e.g.
 * "completion token count") are not excluded: they also appear as usage
 * breakdowns inside genuine input-overflow messages, and "(prompt +
 * completion) exceed" combined-budget wording stays classifiable because the
 * role word is not adjacent to "tokens".
 */
const NON_CONTEXT_OVERFLOW_PATTERNS: readonly RegExp[] = [
  /rate limit/i,
  /too many requests/i,
  /throttl/i,
  /quota/i,
  /(?:output|completion|max_tokens)\b[^.]{0,60}(?:too many tokens|token limit exceeded)/i,
  /(?:too many tokens|token limit exceeded)[^.]{0,60}\b(?:output|completion|max_tokens)/i,
  /(?:output|completion)\s+token\s+(?:count|limit)[^.]{0,40}exceed/i,
  /too many (?:output|completion|max_tokens)[^.]{0,20}tokens/i,
  /\b(?:output|completion|max_tokens)\s+tokens?\b[^.]{0,20}exceed/i,
];

/**
 * Two-layer overflow detection on an error's raw text (the composite of its
 * original name/code/status/message). Triggering recovery requires positive
 * evidence of an INPUT overflow — the one class history compaction can fix:
 * 1. Vetoes first: throttling/quota wording and complete output-cap relations
 *    disqualify every free-text signal. Free text is never unconditional — a
 *    capacity statement quoted inside a throttle error is not an overflow.
 * 2. Positive overflow relations count only when nothing vetoed. Structured
 *    provider codes (the unconditional evidence) are classifyError's job,
 *    checked before this text layer ever runs.
 */
export function isContextOverflowErrorText(text: string): boolean {
  if (!text) return false;
  if (NON_CONTEXT_OVERFLOW_PATTERNS.some((pattern) => pattern.test(text))) return false;
  return CONTEXT_OVERFLOW_PATTERNS.some((pattern) => pattern.test(text));
}

/**
 * Classifies a provider error by DESCENDING evidence strength over the
 * normalized evidence (Error, string, or plain stream-error-part object):
 * abort → structured account state → 402 → 401 (numeric fields, never
 * substrings) → the provider's structured
 * overflow code → bare 413 (HTTP: request entity too
 * large — itself input-side evidence, Cerebras sends it with no body) →
 * vetoable free-text overflow relations → generic 5xx → weak word
 * heuristics. Specific overflow evidence outranks a generic 5xx because
 * proxies (LiteLLM) wrap provider overflows in 503s; the weak heuristics
 * rank last so "generate" can never become a rate limit.
 */
export function classifyError(error: unknown): string {
  if (RetryError.isInstance(error) && error.reason === 'abort') return 'Abort';
  const classificationTarget = providerErrorTarget(error);
  const evidence = normalizeErrorEvidence(classificationTarget);
  if (!evidence) return 'Other';
  const { text, statusCode, code, structuredCodes } = evidence;
  if (text.includes('abort')) return 'Abort';
  if (code === OPENAI_RESPONSES_WEBSOCKET_TRANSPORT_ERROR) return 'Network';
  if (structuredCodes.some((c) => PROVIDER_AUTH_CODES.has(c))) return 'Auth';
  if (structuredCodes.some((c) => PROVIDER_BILLING_CODES.has(c))) return 'ProviderBilling';
  if (structuredCodes.some((c) => PROVIDER_PERMISSION_CODES.has(c))) return 'ProviderPermission';
  if (structuredCodes.some((c) => PROVIDER_USAGE_LIMIT_CODES.has(c))) return 'UsageLimit';
  if (structuredCodes.some((c) => PROVIDER_RATE_LIMIT_CODES.has(c))) return 'RateLimit';
  if (structuredCodes.some((c) => PROVIDER_UNAVAILABLE_CODES.has(c))) return 'ProviderUnavailable';
  if (statusCode === '402' || code === '402') return 'ProviderBilling';
  if (statusCode === '401' || code === '401') return 'Auth';
  // A bare 403 is intentionally not classified: providers use it for valid-key
  // permission failures, guardrails, subscription limits, and occasionally
  // authentication. Only structured or narrow semantic evidence may name it.
  // Structured provider evidence: the parsed error JSON's code/type is the
  // only unconditional signal for a context overflow.
  if (structuredCodes.some((c) => CONTEXT_OVERFLOW_PROVIDER_CODES.has(c))) return 'ContextLength';
  if (statusCode === '413' || code === '413') return 'ContextLength';
  // Free-text overflow relations on the composite text, veto-first inside.
  if (isContextOverflowErrorText(text)) return 'ContextLength';
  if (/^5\d\d$/.test(statusCode) || /^5\d\d$/.test(code)) return 'ProviderUnavailable';
  // Transport-shaped wording may still identify operational failures, but
  // account state never does: auth, billing, permission, usage, and rate-limit
  // meanings require the structured/status facts above.
  if (text.includes('timeout')) return 'Timeout';
  if (
    text.includes('network') ||
    text.includes('fetch') ||
    /\btypeerror\b.*\bterminated\b/.test(text)
  )
    return 'Network';
  return classificationTarget instanceof Error ? classificationTarget.name || 'Other' : 'Other';
}

function defaultFailureMessage(errorClass: string): string {
  switch (errorClass) {
    case 'ContextLength':
      return 'Context window exceeded';
    case 'Timeout':
      return 'Request timed out';
    case 'Auth':
      return 'Authentication failed';
    case 'ProviderBilling':
      return 'Provider billing required';
    case 'ProviderPermission':
      return 'Provider access denied';
    case 'ProviderUnavailable':
      return 'Provider returned an error';
    case 'RateLimit':
      return 'Rate limit exceeded';
    case 'UsageLimit':
      return 'Usage limit reached';
    case 'Network':
      return 'Network error';
    default:
      return 'Operation failed';
  }
}

function modelFailureKind(errorClass: string): ModelFailureKind {
  switch (errorClass) {
    case 'Abort':
      return 'abort';
    case 'Auth':
      return 'auth';
    case 'ContextLength':
      return 'context_overflow';
    case 'Network':
      return 'network';
    case 'ProviderBilling':
      return 'provider_billing';
    case 'ProviderPermission':
      return 'provider_permission';
    case 'ProviderUnavailable':
      return 'provider_unavailable';
    case 'RateLimit':
      return 'rate_limit';
    case 'Timeout':
      return 'timeout';
    case 'UsageLimit':
      return 'usage_limit';
    default:
      return 'unknown';
  }
}
