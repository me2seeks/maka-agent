import type { RuntimeEvent } from '@maka/core/runtime-event';
import { estimateRuntimeEventsTokens } from './context-budget-helpers.js';

// The single authority on what a history-compact checkpoint summary must look
// like (#3029). The summarization prompt is built from the same constants and
// the load side can consume the same predicate, so the mandated format and
// every validation site share one contract that cannot drift.

export type CheckpointSummaryDefect =
  | 'malformed_summary_missing_section'
  | 'malformed_summary_truncated'
  | 'malformed_summary_too_small_for_fold';

// The exact format the prompt mandates. REQUIRED_SUMMARY_SECTIONS is the
// ordered subsequence of its headings a completion must carry to be allowed
// to REPLACE folded history; Critical Context is required because it is
// exactly what the #3029 incident lost (files, commands, errors), and the
// template gives it an explicit "(none)" escape hatch.
export const SUMMARY_FORMAT_TEMPLATE = [
  '## Goal',
  '[What the user is trying to accomplish]',
  '',
  '## Progress',
  '### Done',
  '- [Completed work and changes]',
  '### In Progress',
  '- [Current work]',
  '',
  '## Key Decisions',
  '- **[Decision]**: [Brief rationale]',
  '',
  '## Next Steps',
  '1. [Ordered list of what should happen next]',
  '',
  '## Critical Context',
  '- [Files, commands/results, errors, anything needed to continue; or "(none)"]',
] as const;

export const REQUIRED_SUMMARY_SECTIONS = [
  '## Goal',
  '## Progress',
  '## Next Steps',
  '## Critical Context',
] as const;

// A verbatim echo of the mandated template carries no information: template
// lines never count as section content, fenced or not, so a degraded model
// parroting the format back cannot pass as a checkpoint.
const TEMPLATE_PLACEHOLDER_LINES: ReadonlySet<string> = new Set(
  SUMMARY_FORMAT_TEMPLATE.map((line) => line.trim()).filter((line) => line.length > 0),
);

const SECTION_HEAD_PATTERNS = REQUIRED_SUMMARY_SECTIONS.map(
  (section) => new RegExp(`^${section}\\b`),
);

// Floors for the incident's shape: folding a large span into a paragraph
// cannot be a faithful checkpoint. Folds above ~10k estimated tokens require
// at least ~200 estimated tokens of summary, both measured at the session's
// chars-per-token estimate so CJK-heavy sessions are floored consistently.
const LARGE_FOLD_ESTIMATED_TOKENS = 10_000;
const LARGE_FOLD_SUMMARY_TOKENS_FLOOR = 200;
const DEFAULT_CHARS_PER_TOKEN = 4;

export interface CheckpointSummaryFoldContext {
  /** The FULL covered span the checkpoint replaces — not the newly folded
   * increment, so rolling roll-forward compaction cannot slip a fragment
   * past the size floor. */
  coveredRuntimeEvents: readonly RuntimeEvent[];
  /** The session's estimate; defaults to the shared 4-chars/token. */
  charsPerToken?: number;
}

// #3029: a degraded provider completion — a 138-token free-form fragment
// ending mid-sentence, delivered with a stop finish reason — was accepted as
// the checkpoint for ~235k estimated tokens of history, and the continuation
// confabulated around the missing context. Anything that does not look like
// the checkpoint the prompt mandates is a defect the caller fails open on
// (history kept, compaction retried). Returns undefined for an EMPTY summary:
// the compaction layer's empty_summary gate owns that case.
export function findCheckpointSummaryDefect(
  summary: string,
  foldContext?: CheckpointSummaryFoldContext,
): CheckpointSummaryDefect | undefined {
  const trimmed = summary.trim();
  if (trimmed.length === 0) return undefined;
  const scan = scanSummaryStructure(trimmed);
  if (!scan.orderedSectionsPresent) {
    return 'malformed_summary_missing_section';
  }
  // A trailing colon or ending inside an open code fence marks output cut
  // mid-sentence — seen with partial completions the provider still finished
  // with 'stop'.
  if (/[:：]$/.test(trimmed) || scan.endsInsideOpenFence) {
    return 'malformed_summary_truncated';
  }
  if (foldContext !== undefined) {
    // Clamped so a zero/negative estimate cannot zero out the floor.
    const charsPerToken = Math.max(1, foldContext.charsPerToken ?? DEFAULT_CHARS_PER_TOKEN);
    if (
      trimmed.length < LARGE_FOLD_SUMMARY_TOKENS_FLOOR * charsPerToken &&
      estimateRuntimeEventsTokens(foldContext.coveredRuntimeEvents, charsPerToken) >
        LARGE_FOLD_ESTIMATED_TOKENS
    ) {
      return 'malformed_summary_too_small_for_fold';
    }
  }
  return undefined;
}

// One line scan holding a single interpretation of the document for both
// checks: the mandated sections must appear in order, each with non-empty
// content, and only OUTSIDE fenced code blocks — a degraded model quoting the
// template inside a fence must not count as structure. Fences follow the
// Markdown closing rule: same character family AND a run at least as long as
// the opener (a shorter run stays fenced content), tracked line-opening only
// so a verbatim ``` inside a preserved error message stays content. The scan
// also reports whether the document ends inside an open fence.
function scanSummaryStructure(text: string): {
  orderedSectionsPresent: boolean;
  endsInsideOpenFence: boolean;
} {
  let openFence: { family: string; width: number } | undefined;
  let matchedSections = 0;
  // Content attribution target: content lines satisfy the most recently
  // matched REQUIRED section, but an intervening unmatched H2 (e.g. the
  // template's "## Key Decisions") opens its own section — its content must
  // not satisfy the previous required section's non-empty requirement.
  let attributesToRequiredSection = false;
  const sectionHasContent: boolean[] = REQUIRED_SUMMARY_SECTIONS.map(() => false);
  const countContent = (line: string) => {
    if (matchedSections === 0 || !attributesToRequiredSection) return;
    const trimmedLine = line.trim();
    if (trimmedLine.length === 0) return;
    if (TEMPLATE_PLACEHOLDER_LINES.has(trimmedLine)) return;
    sectionHasContent[matchedSections - 1] = true;
  };
  // Normalized so CRLF input cannot smuggle a trailing \r past any line check.
  for (const line of text.split(/\r?\n/)) {
    const fence = /^\s*(`{3,}|~{3,})/.exec(line);
    if (fence) {
      const family = fence[1]![0]!;
      const width = fence[1]!.length;
      if (openFence === undefined) {
        openFence = { family, width };
      } else if (
        openFence.family === family &&
        width >= openFence.width &&
        line.trim() === fence[1]
      ) {
        // A closer must be a bare run of the same family, at least as long
        // as the opener (an opener may carry an info string; a closer may
        // not).
        openFence = undefined;
      } else {
        // Anything else — a shorter run, a different family, or trailing
        // text — is fenced content of the enclosing section.
        countContent(line);
      }
      continue;
    }
    if (openFence !== undefined) {
      // Fenced lines are content of the enclosing section, never headings.
      countContent(line);
      continue;
    }
    if (
      matchedSections < REQUIRED_SUMMARY_SECTIONS.length &&
      SECTION_HEAD_PATTERNS[matchedSections]!.test(line)
    ) {
      matchedSections += 1;
      attributesToRequiredSection = true;
      continue;
    }
    // Any other H2 opens its own (non-required) section; deeper headings
    // organize within the current one.
    if (/^##\s/.test(line)) {
      attributesToRequiredSection = false;
      continue;
    }
    // Anything non-blank that is not itself a heading, a thematic break, or
    // a verbatim template line counts as content for the most recently
    // matched section (subheadings organize, lists carry, horizontal rules
    // separate).
    if (!/^#{1,6}\s/.test(line) && !/^\s*([-*_])\s*(?:\1\s*){2,}$/.test(line)) {
      countContent(line);
    }
  }
  return {
    orderedSectionsPresent:
      matchedSections === REQUIRED_SUMMARY_SECTIONS.length && sectionHasContent.every(Boolean),
    endsInsideOpenFence: openFence !== undefined,
  };
}
