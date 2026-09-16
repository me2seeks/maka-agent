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

// Developer-only fixture owner. Never imported by the production bridge.
import { withExecutionRoot } from '../../../packages/runtime-host/dist/__tests__/fixtures/execution-host-suite.js';

if (!process.send) throw new Error('Isolated Host requires an owning launcher');
let finish;
const stopped = new Promise((resolve) => {
  finish = resolve;
});
process.once('message', () => finish());
process.once('disconnect', () => finish());
process.once('SIGTERM', () => finish());
process.once('SIGINT', () => finish());

try {
  await withExecutionRoot(async (fixture) => {
    const host = await fixture.startHost();
    try {
      if (process.connected) process.send({ root: fixture.root, sessionId: fixture.sessionId });
      await Promise.race([stopped, new Promise((resolve) => host.child.once('exit', resolve))]);
    } finally {
      await fixture.stopHost(host);
    }
  });
} finally {
  if (process.connected) process.disconnect();
}
