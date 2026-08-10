import type { UiLocale } from '@maka/core';
import { getDesktopConversationCopy } from './locales/conversation-copy.js';

/**
 * Locale-aware allowlist for stable ErrorEvent.reason values emitted by the
 * runtime. Unknown reasons intentionally return undefined; their separately
 * normalized diagnostic remains presentation detail, not localization input.
 */
export function describeSessionErrorReason(reason: string | undefined, locale: UiLocale = 'zh'): string | undefined {
  const copy = getDesktopConversationCopy(locale).turnError;
  switch (reason?.toLowerCase()) {
    case 'context_overflow':
      return copy.contextOverflow;
    case 'timeout':
      return copy.timeout;
    case 'auth':
      return copy.auth;
    case 'provider_billing':
      return copy.providerBilling;
    case 'provider_permission':
      return copy.providerPermission;
    case 'provider_unavailable':
      return copy.provider;
    case 'rate_limit':
      return copy.rateLimit;
    case 'usage_limit':
      return copy.usageLimit;
    case 'network':
      return copy.network;
    default:
      return undefined;
  }
}
