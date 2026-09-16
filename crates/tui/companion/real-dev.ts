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

import { mkdir, lstat, readFile, realpath, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { Writable } from 'node:stream';
import { fileURLToPath } from 'node:url';
import {
  connectOrSpawnRuntimeHost,
  readRuntimeHostConnectionCatalog,
  type RuntimeHostConnection,
} from '@maka/runtime-host/client';
import type { SessionModelTarget } from '@maka/runtime-host/protocol';
import { HOST_BASELINE } from './host-baseline.ts';

const marker = 'maka-ratatui-owned-development-root-epoch154-v1\n';

/** Only this fixed, marked worktree directory may be created/activated. */
export async function prepareRealDevelopmentRoot(): Promise<string> {
  const parent = fileURLToPath(new URL('../.development/', import.meta.url));
  const root = join(parent, 'epoch154');
  await mkdir(parent, { recursive: true, mode: 0o700 });
  if ((await lstat(parent)).isSymbolicLink() || (await realpath(parent)) !== parent.slice(0, -1)) {
    throw new Error('开发目录不能是符号链接。');
  }
  try {
    await mkdir(root, { mode: 0o700 });
    await writeFile(join(root, '.tui-development-owner'), marker, { flag: 'wx', mode: 0o600 });
  } catch (error) {
    if (!(error instanceof Error && 'code' in error && error.code === 'EEXIST')) throw error;
  }
  if (
    (await lstat(root)).isSymbolicLink() ||
    (await readFile(join(root, '.tui-development-owner'), 'utf8')) !== marker
  ) {
    throw new Error('开发 State Root 所有权标记不匹配；不会启动 Host。');
  }
  return root;
}

export async function startRealDevelopmentHost(root: string) {
  const result = await connectOrSpawnRuntimeHost({
    rootPath: root,
    protocol: { min: HOST_BASELINE.protocolVersion, max: HOST_BASELINE.protocolVersion },
    compositionId: HOST_BASELINE.compositionId,
    candidateEntrypoint: new URL('./real-dev-candidate.mjs', import.meta.url),
    candidateExecutable: process.execPath,
    closeOnLauncherExit: true,
    electionDeadlineMs: 20_000,
  });
  if (result.kind !== 'connected') {
    const reason = result.kind === 'failed' ? result.reason : result.kind;
    const diagnostic = result.kind === 'failed' ? result.diagnostic : undefined;
    const details = diagnostic
      ? `，尝试 ${diagnostic.candidateLaunches} 次，耗时 ${Math.round(diagnostic.elapsedMs)}ms，exit=${diagnostic.latestCandidate?.exitCode ?? diagnostic.latestCandidate?.signal ?? 'unknown'}`
      : '';
    throw new Error(`独立 Host 启动失败（${reason}${details}）；未尝试升级或重启。`);
  }
  return result;
}

/** Credentials are entered locally without echo and sent only to this Host. */
export async function configureRealDevelopmentModel(
  connection: RuntimeHostConnection,
): Promise<SessionModelTarget> {
  const catalog = await readRuntimeHostConnectionCatalog(connection);
  const existing = catalog.connections.find(
    (entry) => entry.slug === 'ratatui-dev' && entry.enabled,
  );
  if (existing?.enabledModelIds[0]) {
    console.log(`使用独立模型连接：${existing.providerType} / ${existing.enabledModelIds[0]}`);
    return setDefaultModel(connection, {
      kind: 'explicit',
      connectionId: existing.connectionId,
      connectionSlug: existing.slug,
      model: existing.enabledModelIds[0],
    });
  }
  let hidden = false;
  const output = new Writable({
    write(chunk, _encoding, callback) {
      if (!hidden) process.stdout.write(chunk);
      callback();
    },
  });
  const prompt = createInterface({ input: process.stdin, output, terminal: true });
  const cancelled = new AbortController();
  prompt.once('SIGINT', () => cancelled.abort());
  prompt.once('close', () => cancelled.abort());
  const ask = (label: string) => prompt.question(label, { signal: cancelled.signal });
  try {
    console.log('首次配置独立真实模型。不会复用现用 Host 的连接或密钥。');
    console.log('1 OpenAI · 2 OpenAI 兼容 · 3 OpenAI Responses 兼容 · 4 Anthropic · 5 DeepSeek');
    const choice = await ask('服务类型（q 取消）：');
    const providers = [
      'openai',
      'openai-compatible',
      'openai-responses-compatible',
      'anthropic',
      'deepseek',
    ] as const;
    const providerType = providers[Number(choice.trim()) - 1];
    if (!providerType) throw new Error('已取消模型配置，未发送模型请求。');
    const baseUrl = (await ask('Base URL（留空使用服务商默认）：')).trim() || null;
    const model = (await ask('模型 ID：')).trim();
    if (!model || model.length > 512) throw new Error('需要有效模型 ID。');
    console.log('API key 仅发送至独立 Host 保存；不会写入命令行或输出。');
    process.stdout.write('API key（隐藏输入）：');
    hidden = true;
    let apiKey = await ask('');
    hidden = false;
    process.stdout.write('\n');
    console.log('正在保存连接；Host 可能访问所选服务商的模型目录，不会发送聊天提示词。');
    try {
      const result = await connection.request(
        'connection.onboarding.save',
        {
          target: {
            kind: 'create',
            providerType,
            slug: 'ratatui-dev',
            name: 'Ratatui development',
          },
          apiKey: apiKey.trim() || null,
          baseUrl,
          enabledModelIds: [model],
        },
        30_000,
      );
      if (result.kind !== 'saved') {
        throw new Error(`模型连接未保存（${result.kind}）；不会退回默认免费模型。`);
      }
      return setDefaultModel(connection, {
        kind: 'explicit',
        connectionId: result.connection.connectionId,
        connectionSlug: result.connection.slug,
        model,
      });
    } finally {
      apiKey = '';
    }
  } finally {
    hidden = false;
    prompt.close();
    output.end();
  }
}

async function setDefaultModel(
  connection: RuntimeHostConnection,
  target: Extract<SessionModelTarget, { kind: 'explicit' }>,
): Promise<SessionModelTarget> {
  const page = await connection.request('connection.catalog.query', { kind: 'start' }, 5_000);
  if (page.kind !== 'page') throw new Error('无法读取独立模型默认配置。');
  if (
    page.defaultTarget?.connectionId !== target.connectionId ||
    page.defaultTarget.modelId !== target.model
  ) {
    const result = await connection.request(
      'connection.catalog.set-default-target',
      {
        expectedCatalogRevision: page.revision,
        target: { connectionId: target.connectionId, modelId: target.model },
      },
      5_000,
    );
    if (result.kind !== 'committed') throw new Error('独立模型默认配置未确认；不会退回免费模型。');
  }
  return target;
}
