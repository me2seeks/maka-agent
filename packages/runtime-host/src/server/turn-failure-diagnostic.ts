import { TURN_FAILURE_MESSAGE_MAX_LENGTH } from '../protocol/turn.js';

export function projectTurnFailureMessage(message: string | undefined): string | undefined {
  if (!message) return undefined;
  if (message.length <= TURN_FAILURE_MESSAGE_MAX_LENGTH) return message;
  const contentLimit = TURN_FAILURE_MESSAGE_MAX_LENGTH - 1;
  let content = '';
  for (const codePoint of message) {
    if (content.length + codePoint.length > contentLimit) break;
    content += codePoint;
  }
  return `${content}…`;
}
