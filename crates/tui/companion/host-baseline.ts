/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import {
  RUNTIME_HOST_PROTOCOL_VERSION,
  RUNTIME_HOST_COMPATIBILITY_EPOCH,
  INTERACTIVE_RUNTIME_HOST_COMPOSITION_ID,
} from '@maka/runtime-host/protocol';

export const HOST_BASELINE = Object.freeze(
  JSON.parse(readFileSync(new URL('../host-baseline.json', import.meta.url), 'utf8')) as {
    sourceCommit: string;
    protocolVersion: number;
    compatibilityEpoch: number;
    compositionId: string;
  },
);

/** Source-prototype gate, not attestation of a remote Host's build SHA. */
export function assertHostBaseline(): void {
  if (
    RUNTIME_HOST_PROTOCOL_VERSION !== HOST_BASELINE.protocolVersion ||
    RUNTIME_HOST_COMPATIBILITY_EPOCH !== HOST_BASELINE.compatibilityEpoch ||
    INTERACTIVE_RUNTIME_HOST_COMPOSITION_ID !== HOST_BASELINE.compositionId
  ) {
    throw new Error('Host client build does not match the pinned protocol baseline');
  }
  execFileSync(
    'git',
    [
      'diff',
      '--quiet',
      HOST_BASELINE.sourceCommit,
      '--',
      'packages/core',
      'packages/storage',
      'packages/mcp',
      'packages/runtime',
      'packages/runtime-host',
      'package-lock.json',
      'tsconfig.base.json',
    ],
    { cwd: new URL('../../../', import.meta.url), stdio: 'ignore', timeout: 5_000 },
  );
}
