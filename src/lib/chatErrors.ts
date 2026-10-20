// chatErrors.ts
//
// Wording for request failures (src-tauri/src/chat_request.rs): the status
// line while retrying, and the final message when a request gives up.
import type { ChatError, StoredFailure } from '../types';

/** "45 s", "12 min", "4 h 12 min" */
export function formatDuration(ms: number): string {
  const secs = Math.max(0, Math.ceil(ms / 1000));
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.ceil(secs / 60)} min`;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return m ? `${h} h ${m} min` : `${h} h`;
}

/** What is stored when a request fails for good. */
export function toStoredFailure(error: ChatError, now = Date.now()): StoredFailure {
  return {
    kind: error.kind,
    message: error.message,
    attempts: error.attempts,
    resetAt: error.retryAfterMs ? now + error.retryAfterMs : undefined,
    details: error.details ?? undefined,
  };
}

/** Short reason for the status line while retrying. */
export function retryReason(error: ChatError): string {
  switch (error.kind) {
    case 'empty_response':
      return 'Empty response from the model.';
    case 'stalled':
      return 'No response from the model.';
    case 'timeout':
      return 'The reply took too long.';
    case 'rate_limited':
      return 'The model is busy (rate limited).';
    case 'server_error':
    case 'provider':
      return error.status ? `Provider error (${error.status}).` : 'Provider error.';
    case 'network':
      return 'Connection problem.';
    default:
      return 'The request failed.';
  }
}

/** The sentence shown for a failed request. */
export function describeFailure(failure: StoredFailure, now = Date.now()): string {
  const after = failure.attempts > 1 ? ` (after ${failure.attempts} attempts)` : '';
  switch (failure.kind) {
    case 'rate_limited': {
      if (failure.resetAt && failure.resetAt > now) {
        const at = new Date(failure.resetAt).toLocaleTimeString([], {
          hour: '2-digit',
          minute: '2-digit',
        });
        return `Rate limit reached. Try again at ${at} (in ${formatDuration(failure.resetAt - now)}).`;
      }
      return `The provider is rate limiting requests${after}. Try again shortly.`;
    }
    case 'empty_response':
      return `The model returned an empty response${after}.`;
    case 'stalled':
      return `The model stopped responding${after}.`;
    case 'timeout':
      return `The reply took too long${after}.`;
    case 'server_error':
      return `The provider had a server error${after}.`;
    case 'network':
      return `Couldn't reach the provider${after}.`;
    case 'auth':
      return 'The API key was rejected. Check it in Model Settings.';
    case 'not_found':
      return "The model or endpoint wasn't found. Check the model ID in Model Settings.";
    case 'context_length':
      return 'The conversation is too long for this model. Start a new chat, or set a smaller context window for it in Model Settings.';
    case 'length':
      return 'The model reached its output limit before writing anything.';
    case 'cancelled':
      return 'Stopped.';
    case 'restarted':
      return 'The server restarted while the reply was being written.';
    case 'bad_request':
      return 'The provider rejected the request.';
    case 'refused':
      return failure.message || 'The model declined this request.';
    case 'provider':
    default:
      return `The provider reported an error${after}.`;
  }
}

/** Whether "Retry" makes sense for this failure. */
export function canRetry(failure: StoredFailure): boolean {
  return !['auth', 'not_found', 'bad_request', 'context_length'].includes(failure.kind);
}
