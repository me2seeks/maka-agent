import type { RuntimeEvent } from '@maka/core/runtime-event';
import { rawFinishReasonString, type ModelMessage, type ToolCallPart } from './model-protocol.js';
import { buildRuntimeEventModelReplayPlan } from './model-history.js';
import { estimateRuntimeEventsTokens } from './context-budget-helpers.js';
import { toolResultOutput } from './tool-result-output.js';
import type { HistoryCompactSummaryInput } from './ai-sdk-compaction-contract.js';
import { HistoryCompactSummarizerError } from './history-compact-error.js';
import { isTextHistoryCompactCheckpoint } from './history-compact-checkpoint.js';
import { fitHistoryCompactMessages } from './history-compact-input-fit.js';
import type { AiSdkUsageLike } from './model-adapter.js';
import { withProviderGenerateTracking } from './provider-request-telemetry.js';

export { HistoryCompactSummarizerError } from './history-compact-error.js';

export interface AiSdkGenerateTextOptions {
  model: unknown;
  instructions: string;
  messages: ModelMessage[];
  providerOptions?: Record<string, unknown>;
  maxOutputTokens?: number;
  abortSignal?: AbortSignal;
}

export type AiSdkGenerateTextLike = (options: AiSdkGenerateTextOptions) => Promise<{
  text: string;
  finishReason?: unknown;
  usage?: AiSdkUsageLike;
}>;

export interface BuildLlmHistorySummarizerOptions {
  /** Resolve the AI SDK model used for summarization. Reuses the session model. */
  resolveModel: () => unknown;
  /** Session provider settings, including the selected reasoning level. */
  providerOptions?: Record<string, unknown>;
  /** Injectable `generateText` for tests; defaults to the real AI SDK export. */
  generateText?: AiSdkGenerateTextLike;
}

// The sections a completion must carry to be allowed to REPLACE folded
// history (#3029). The prompt below is built from the same constants so the
// mandated format and the validation can never drift apart.
const REQUIRED_SUMMARY_SECTIONS = ['## Goal', '## Progress', '## Next Steps'] as const;
const [GOAL_SECTION, PROGRESS_SECTION, NEXT_STEPS_SECTION] = REQUIRED_SUMMARY_SECTIONS;

// Conversation-summarization prompt (sectioned, modelled on pi/opencode):
// asks for a checkpoint another LLM can continue from. Tool calls and their
// results are part of the conversation sent to the summarizer, because the
// folded events are projected with the same policy the model would see them.
const SUMMARIZATION_SYSTEM_PROMPT = [
  'You are a context summarization assistant.',
  'Read the conversation between a user and an AI assistant, then produce a structured summary another LLM will use to continue the same task.',
  'Do NOT continue the conversation. Do NOT answer questions in it. ONLY output the structured summary.',
  '',
  'Use this exact format:',
  '',
  GOAL_SECTION,
  '[What the user is trying to accomplish]',
  '',
  PROGRESS_SECTION,
  '### Done',
  '- [Completed work and changes]',
  '### In Progress',
  '- [Current work]',
  '',
  '## Key Decisions',
  '- **[Decision]**: [Brief rationale]',
  '',
  NEXT_STEPS_SECTION,
  '1. [Ordered list of what should happen next]',
  '',
  '## Critical Context',
  '- [Files, commands/results, errors, anything needed to continue; or "(none)"]',
  '',
  'Keep each section concise. Preserve exact file paths, function names, commands, and error messages.',
].join('\n');

export function buildLlmHistorySummarizer(options: BuildLlmHistorySummarizerOptions) {
  return async (input: HistoryCompactSummaryInput): Promise<string | undefined> => {
    const previousCheckpoint =
      input.previousCheckpoint && isTextHistoryCompactCheckpoint(input.previousCheckpoint)
        ? input.previousCheckpoint
        : undefined;
    const newlyFoldedRuntimeEvents =
      input.previousCheckpoint && !previousCheckpoint
        ? input.source.foldedRuntimeEvents
        : (input.newlyFoldedRuntimeEvents ?? input.source.foldedRuntimeEvents);
    if (newlyFoldedRuntimeEvents.length === 0) return previousCheckpoint?.summary;
    try {
      const plan = buildRuntimeEventModelReplayPlan(newlyFoldedRuntimeEvents);
      const projectedMessages = replayPlanItemsToModelMessages(plan.items);
      if (previousCheckpoint) {
        projectedMessages.unshift({
          role: 'user',
          content: [
            {
              type: 'text',
              text: `Previous continuation summary:\n${previousCheckpoint.summary}\n\nUpdate it using the newer conversation events that follow.`,
            },
          ],
        });
      }
      const messages = fitHistoryCompactMessages(projectedMessages, {
        maxInputEstimatedTokens: input.inputBudget?.maxEstimatedTokens,
        charsPerToken: input.inputBudget?.charsPerToken,
        fixedInputChars: SUMMARIZATION_SYSTEM_PROMPT.length,
      });
      // Handed over whole by the backend, which owns every input a tracker
      // needs — including the run, which no summarizer wiring can know (#1679).
      const providerRequestTracker = input.providerRequestTracker;
      const ai =
        options.generateText && !providerRequestTracker ? undefined : await loadAiSdkTextModule();
      const generateText = options.generateText ?? ai!.generateText;
      const model = providerRequestTracker
        ? withProviderGenerateTracking({
            model: options.resolveModel(),
            wrapLanguageModel: ai!.wrapLanguageModel,
            tracker: providerRequestTracker,
            ...(input.abortSignal ? { abortSignal: input.abortSignal } : {}),
          })
        : options.resolveModel();
      const result = await generateText({
        model,
        instructions: SUMMARIZATION_SYSTEM_PROMPT,
        messages,
        ...(options.providerOptions !== undefined
          ? { providerOptions: options.providerOptions }
          : {}),
        ...(input.abortSignal ? { abortSignal: input.abortSignal } : {}),
      });
      if (rawFinishReasonString(result.finishReason) === 'length') {
        throw new HistoryCompactSummarizerError('output_length');
      }
      assertWellFormedCheckpointSummary(result.text, input.source.foldedRuntimeEvents);
      return result.text;
    } catch (error) {
      if (error instanceof HistoryCompactSummarizerError) throw error;
      throw new HistoryCompactSummarizerError('provider_error', { cause: error });
    }
  };
}

// Floors for the incident's shape: folding a large span into a paragraph
// cannot be a faithful checkpoint. Folds above ~10k estimated tokens (at the
// default 4-chars/token estimate) require at least ~200 estimated tokens of
// summary.
const LARGE_FOLD_ESTIMATED_TOKENS = 10_000;
const LARGE_FOLD_SUMMARY_CHARS_FLOOR = 800;

// #3029: a degraded provider completion — a 138-token free-form fragment
// ending mid-sentence, delivered with a stop finish reason — was accepted as
// the checkpoint for ~235k estimated tokens of history, and the continuation
// confabulated around the missing context. Anything that does not look like
// the checkpoint the prompt mandates fails open instead (history kept,
// compaction retried). The size floor is measured against the FULL covered
// span the checkpoint replaces, not the newly folded increment, so rolling
// roll-forward compaction cannot slip a fragment past it.
function assertWellFormedCheckpointSummary(
  summary: string,
  coveredRuntimeEvents: readonly RuntimeEvent[],
): void {
  const trimmed = summary.trim();
  // The compaction layer's empty_summary gate owns the empty case.
  if (trimmed.length === 0) return;
  const scan = scanSummaryStructure(trimmed);
  if (!scan.orderedSectionsPresent) {
    throw new HistoryCompactSummarizerError('malformed_summary_missing_section');
  }
  // A trailing colon or ending inside an open code fence marks output cut
  // mid-sentence — seen with partial completions the provider still finished
  // with 'stop'.
  if (/[:：]$/.test(trimmed) || scan.endsInsideOpenFence) {
    throw new HistoryCompactSummarizerError('malformed_summary_truncated');
  }
  if (
    trimmed.length < LARGE_FOLD_SUMMARY_CHARS_FLOOR &&
    estimateRuntimeEventsTokens(coveredRuntimeEvents) > LARGE_FOLD_ESTIMATED_TOKENS
  ) {
    throw new HistoryCompactSummarizerError('malformed_summary_too_small_for_fold');
  }
}

// One line scan holding a single interpretation of the document for both
// checks: the mandated sections must appear in order, each with non-empty
// content, and only OUTSIDE fenced code blocks — a degraded model quoting the
// template inside a fence must not count as structure. Fences (backtick or
// tilde, line-opening only, so a verbatim ``` inside a preserved error
// message stays content) also report whether the document ends inside one.
function scanSummaryStructure(text: string): {
  orderedSectionsPresent: boolean;
  endsInsideOpenFence: boolean;
} {
  let fenceFamily: string | undefined;
  let matchedSections = 0;
  const sectionHasContent: boolean[] = REQUIRED_SUMMARY_SECTIONS.map(() => false);
  for (const line of text.split('\n')) {
    const fence = /^\s*(`{3,}|~{3,})/.exec(line);
    if (fence) {
      const family = fence[1]![0]!;
      if (fenceFamily === undefined) fenceFamily = family;
      else if (fenceFamily === family) fenceFamily = undefined;
      continue;
    }
    if (fenceFamily !== undefined) {
      // Fenced lines are content of the enclosing section, never headings.
      if (matchedSections > 0 && line.trim().length > 0) {
        sectionHasContent[matchedSections - 1] = true;
      }
      continue;
    }
    if (
      matchedSections < REQUIRED_SUMMARY_SECTIONS.length &&
      new RegExp(`^${REQUIRED_SUMMARY_SECTIONS[matchedSections]}\\b`).test(line)
    ) {
      matchedSections += 1;
      continue;
    }
    // Anything non-blank that is not itself a heading counts as content for
    // the most recently matched section (subheadings organize, lists carry).
    if (matchedSections > 0 && line.trim().length > 0 && !/^#{1,6}\s/.test(line)) {
      sectionHasContent[matchedSections - 1] = true;
    }
  }
  return {
    orderedSectionsPresent:
      matchedSections === REQUIRED_SUMMARY_SECTIONS.length &&
      sectionHasContent.every(Boolean),
    endsInsideOpenFence: fenceFamily !== undefined,
  };
}

interface AiSdkTextModule {
  generateText: AiSdkGenerateTextLike;
  wrapLanguageModel(input: Record<string, unknown>): unknown;
}

async function loadAiSdkTextModule(): Promise<AiSdkTextModule> {
  const ai = await import('ai').catch((err) => {
    throw new Error(
      `Failed to load 'ai' package for history summarization. Run \`npm install ai\`. Inner: ${(err as Error).message}`,
    );
  });
  return ai as unknown as AiSdkTextModule;
}

type ReplayPlanItems = ReturnType<typeof buildRuntimeEventModelReplayPlan>['items'];

interface OpenToolStep {
  stepId: string | undefined;
  calls: ToolCallPart[];
  callIds: Set<string>;
  settledCallIds: Set<string>;
  bufferedResults: ModelMessage[];
}

export function replayPlanItemsToModelMessages(items: ReplayPlanItems): ModelMessage[] {
  const out: ModelMessage[] = [];
  // One assistant step's tool calls share one assistant message and every
  // result is deferred to the step boundary: strict OpenAI-compatible
  // providers reject an assistant message that arrives while a previous
  // assistant message's tool calls are still unanswered, and Runtime history
  // can legitimately interleave a step's calls and results
  // (call A, call B, result A, call C, result B, result C). Step membership
  // follows the stamped stepId when both sides carry one; legacy items
  // without a stepId join while the open step still has unsettled calls,
  // which is exactly the interleaving case. This mirrors the primary replay
  // materializer's step merge; the primary path is untouched.
  let openStep: OpenToolStep | undefined;
  const flushOpenStep = () => {
    if (!openStep) return;
    out.push(...openStep.bufferedResults);
    openStep = undefined;
  };
  for (const item of items) {
    if (item.kind === 'text') {
      flushOpenStep();
      // Split on role so each push matches exactly one ModelMessage arm — no cast.
      const textPart = { type: 'text' as const, text: item.content };
      if (item.role === 'user') {
        out.push({ role: 'user', content: [textPart] });
      } else {
        out.push({ role: 'assistant', content: [textPart] });
      }
    } else if (item.kind === 'tool_call') {
      const part: ToolCallPart = {
        type: 'tool-call',
        toolCallId: item.toolCallId,
        toolName: item.toolName,
        input: item.input,
      };
      const joinsOpenStep =
        openStep !== undefined &&
        (openStep.stepId !== undefined && item.stepId !== undefined
          ? openStep.stepId === item.stepId
          : openStep.settledCallIds.size < openStep.callIds.size);
      if (openStep && joinsOpenStep) {
        openStep.calls.push(part);
        openStep.callIds.add(item.toolCallId);
      } else {
        flushOpenStep();
        const calls = [part];
        openStep = {
          stepId: item.stepId,
          calls,
          callIds: new Set([item.toolCallId]),
          settledCallIds: new Set(),
          bufferedResults: [],
        };
        out.push({ role: 'assistant', content: calls });
      }
    } else if (item.kind === 'tool_result') {
      const message: ModelMessage = {
        role: 'tool',
        content: [
          {
            type: 'tool-result',
            toolCallId: item.toolCallId,
            toolName: item.toolName,
            output: toolResultOutput(item.output, item.isError),
          },
        ],
      };
      if (openStep?.callIds.has(item.toolCallId)) {
        openStep.settledCallIds.add(item.toolCallId);
        openStep.bufferedResults.push(message);
      } else {
        // A result for a call outside the open step means that step's block
        // is complete; settle it before emitting the foreign result.
        flushOpenStep();
        out.push(message);
      }
    }
    // thinking entries are intentionally skipped for summarization; they do
    // not interrupt an open tool step.
  }
  flushOpenStep();
  return out;
}
