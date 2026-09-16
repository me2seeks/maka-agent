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

import {
  connectExistingRuntimeHost,
  readRuntimeHostSessionCatalogPage,
  type RuntimeHostSessionCatalogPageCursor,
} from '@maka/runtime-host/client';
import { HOST_BASELINE } from './host-baseline.ts';

type Connect = typeof connectExistingRuntimeHost;

/**
 * One bounded page, then disconnect. Never creates a root, starts a Host,
 * registers capabilities, submits a message or advances read markers.
 * Continuations carry the Host-owned revision; stale cursors are not retried.
 */
export async function readHostSessionPage(
  rootPath: string,
  cursor?: RuntimeHostSessionCatalogPageCursor,
  connect: Connect = connectExistingRuntimeHost,
) {
  if (rootPath.trim().length === 0) throw new Error('An explicit state-root path is required');
  const result = await connect({
    rootPath,
    protocol: { min: HOST_BASELINE.protocolVersion, max: HOST_BASELINE.protocolVersion },
    compositionId: HOST_BASELINE.compositionId,
  });
  if (result.kind !== 'connected') {
    return result.kind === 'unavailable'
      ? { kind: 'unavailable' as const, reason: result.reason }
      : { kind: result.kind };
  }
  try {
    const page = await readRuntimeHostSessionCatalogPage(
      { request: (operation, input) => result.connection.request(operation, input, 5_000) },
      cursor,
    );
    return {
      kind: 'page' as const,
      hostEpoch: result.connection.hostEpoch,
      revision: page.revision,
      sessions: page.sessions.map((session) =>
        'kind' in session
          ? {
              kind: session.kind,
              id: session.id,
            }
          : {
              kind: 'session' as const,
              id: session.id,
              name: session.name,
              isArchived: session.isArchived,
            },
      ),
      nextCursor: page.nextCursor,
    };
  } finally {
    await result.connection.close();
  }
}
