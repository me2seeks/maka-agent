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

import { fork, spawn, type ChildProcess } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { isAbsolute, resolve } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { fileURLToPath } from 'node:url';
import {
  connectExistingRuntimeHost,
  readRuntimeHostSessionCatalogPage,
  type RuntimeHostConnection,
} from '@maka/runtime-host/client';
import { assertHostBaseline, HOST_BASELINE } from './host-baseline.ts';
import type { SessionModelTarget } from '@maka/runtime-host/protocol';
import {
  prepareRealDevelopmentRoot,
  startRealDevelopmentHost,
  configureRealDevelopmentModel,
} from './real-dev.ts';

const repository = fileURLToPath(new URL('../../../', import.meta.url));
const bridge = fileURLToPath(new URL('./session-host.ts', import.meta.url));
const usage = `用法：
  npm run dev:maka-tui
    默认进入下面的独立真实模型模式，不连接现用 Host。
  npm run dev:maka-tui -- --isolated-real
    持久化独立 epoch-154 Host；首次在本机配置真实模型，密钥隐藏输入。
  npm run dev:maka-tui -- --isolated
    临时 epoch-154 Host + FakeBackend；不调用真实模型，退出后删除测试数据。
  npm run dev:maka-tui -- --root /已有/state-root
    只连接已有 Host，在启动时选择会话；不会启动、升级或重启 Host。
  npm run dev:maka-tui -- --root /已有/state-root --session 会话ID
  npm run dev:maka-tui -- --root /已有/state-root --new
    使用当前工作目录和 Host 默认模型新建会话。
  npm run dev:maka-tui:demo
    不连接 Host 的界面演示。
`;

export type Options =
  | { kind: 'help' }
  | { kind: 'isolated' }
  | { kind: 'isolated-real' }
  | { kind: 'existing'; root: string; session?: string; create: boolean };

export function parseOptions(args: string[]): Options {
  if (args.length === 0) return { kind: 'isolated-real' };
  if (args.length === 1 && args[0] === '--help') return { kind: 'help' };
  if (args.length === 1 && args[0] === '--isolated') return { kind: 'isolated' };
  if (args.length === 1 && args[0] === '--isolated-real') return { kind: 'isolated-real' };
  let root: string | undefined;
  let session: string | undefined;
  let create = false;
  for (let i = 0; i < args.length; i++) {
    const flag = args[i];
    if (flag === '--root' && root === undefined) root = args[++i];
    else if (flag === '--session' && session === undefined) session = args[++i];
    else if (flag === '--new' && !create) create = true;
    else throw new Error('参数无效；使用 --help 查看用法。');
    if ((flag === '--root' || flag === '--session') && (!args[i] || args[i].startsWith('--'))) {
      throw new Error('参数缺少值。');
    }
  }
  if (
    !root ||
    !isAbsolute(root) ||
    (session && !/^[A-Za-z0-9_-]{1,128}$/.test(session)) ||
    (create && session)
  ) {
    throw new Error('必须指定绝对 --root；--new 与 --session 不能同时使用。');
  }
  return { kind: 'existing', root, session, create };
}

function safeLabel(text: string): string {
  return text.replace(/[\p{C}]/gu, '').slice(0, 100);
}

async function selectSession(
  connection: RuntimeHostConnection,
  modelTarget?: SessionModelTarget,
): Promise<string | undefined> {
  const prompt = createInterface({ input: process.stdin, output: process.stdout });
  const cancelled = new AbortController();
  prompt.once('SIGINT', () => cancelled.abort());
  prompt.once('close', () => cancelled.abort());
  let cursor;
  try {
    while (true) {
      const page = await readRuntimeHostSessionCatalogPage(connection, cursor);
      const sessions = page.sessions.flatMap((session) =>
        'kind' in session || session.isArchived ? [] : [session],
      );
      console.log('\n选择会话（启动阶段；聊天界面不会常驻侧栏）：');
      sessions.forEach((session, index) =>
        console.log(`${index + 1}. ${safeLabel(session.name ?? '未命名')} [${session.id}]`),
      );
      const choice = (
        await prompt.question(`输入序号，n 新建${page.nextCursor ? '，p 下一页' : ''}，q 退出：`, {
          signal: cancelled.signal,
        })
      ).trim();
      if (choice === 'q') return undefined;
      if (choice === 'n') return createSession(connection, modelTarget);
      if (choice === 'p' && page.nextCursor) {
        cursor = page.nextCursor;
        continue;
      }
      const index = Number(choice) - 1;
      if (Number.isInteger(index) && sessions[index]) return sessions[index].id;
      console.log('请选择列出的序号或操作。');
    }
  } finally {
    prompt.close();
  }
}

async function createSession(
  connection: RuntimeHostConnection,
  modelTarget?: SessionModelTarget,
): Promise<string> {
  const sessionId = randomUUID();
  console.log('正在创建会话；若请求中断，不会自动重试，请在会话列表确认结果。');
  const session = await connection.request(
    'session.create',
    {
      sessionId,
      workspace: { kind: 'host_path', path: process.cwd() },
      modelTarget: modelTarget ?? { kind: 'default' },
      permissionMode: 'ask',
    },
    10_000,
  );
  return session.id;
}

async function runTui(root: string, session: string): Promise<void> {
  const binary = resolve(
    repository,
    'target/debug',
    process.platform === 'win32' ? 'maka-tui.exe' : 'maka-tui',
  );
  const child = spawn(binary, ['--connect', process.execPath, bridge, root, session], {
    stdio: 'inherit',
  });
  const forward = () => {
    child.kill('SIGTERM');
  };
  process.once('SIGTERM', forward);
  // The foreground terminal sends Ctrl+C to Rust while in raw mode, not to Node.
  process.once('SIGINT', forward);
  try {
    const [code, signal] = await once(child, 'exit');
    if (code !== 0) throw new Error(`TUI 已结束（${signal ?? code}）。`);
  } finally {
    process.off('SIGTERM', forward);
    process.off('SIGINT', forward);
  }
}

async function stopWorker(worker: ChildProcess, exited: Promise<unknown>): Promise<void> {
  if (worker.connected) worker.send({ kind: 'close' }, () => undefined);
  const timer = setTimeout(() => worker.kill('SIGTERM'), 15_000);
  const kill = setTimeout(() => worker.kill('SIGKILL'), 18_000);
  try {
    const result = await exited;
    if (!Array.isArray(result) || result[0] !== 0) {
      throw new Error('隔离 Host 未正常关闭，无法确认临时数据已清理。');
    }
  } finally {
    clearTimeout(timer);
    clearTimeout(kill);
  }
}

async function runIsolated(): Promise<void> {
  console.log(
    '隔离试用：真实 epoch-154 TS Host + FakeBackend（不是实际模型）。退出后测试会话删除。',
  );
  const worker = fork(new URL('./isolated-dev-host.mjs', import.meta.url), [], {
    stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
  });
  let diagnosticTail = '';
  let startupTimedOut = false;
  worker.stderr?.on('data', (chunk: Buffer) => {
    diagnosticTail = (diagnosticTail + chunk.toString('utf8')).slice(-4096);
  });
  // Report only stable error codes, never raw logs, paths or provider output.
  const diagnostic = () => {
    const codes = [
      ...new Set(
        diagnosticTail.match(/\b(?:ERR_[A-Z_]+|SQLITE_[A-Z_]+|EACCES|ENOENT|ENOMEM|ENOSPC)\b/g) ??
          [],
      ),
    ];
    return `exit=${worker.exitCode ?? worker.signalCode ?? 'running'}${startupTimedOut ? ', startup_timeout' : ''}${codes.length ? ', ' + codes.join(', ') : ''}`;
  };
  const exited = once(worker, 'exit');
  // Attach rejection handling immediately, including spawn errors.
  void exited.catch(() => undefined);
  const ready = new Promise<{ root: string; sessionId: string }>((accept, reject) => {
    worker.once('message', (message) => {
      if (
        !message ||
        typeof message !== 'object' ||
        !('root' in message) ||
        typeof message.root !== 'string' ||
        !isAbsolute(message.root) ||
        !('sessionId' in message) ||
        typeof message.sessionId !== 'string' ||
        !/^[A-Za-z0-9_-]{1,128}$/.test(message.sessionId)
      ) {
        reject(new Error('隔离 Host 返回无效启动信息。'));
      } else accept({ root: message.root, sessionId: message.sessionId });
    });
    worker.once('error', reject);
    worker.once('exit', () =>
      reject(new Error(`隔离 Host 启动失败（${diagnostic()}）；请检查固定 head 的 TS 构建。`)),
    );
  });
  const timer = setTimeout(() => {
    startupTimedOut = true;
    worker.kill('SIGTERM');
  }, 20_000);
  const startupKill = setTimeout(() => worker.kill('SIGKILL'), 25_000);
  const interrupt = () => {
    worker.kill('SIGTERM');
  };
  process.once('SIGINT', interrupt);
  process.once('SIGTERM', interrupt);
  try {
    const { root, sessionId } = await ready;
    clearTimeout(timer);
    clearTimeout(startupKill);
    process.off('SIGINT', interrupt);
    process.off('SIGTERM', interrupt);
    await runTui(root, sessionId);
  } finally {
    clearTimeout(timer);
    clearTimeout(startupKill);
    process.off('SIGINT', interrupt);
    process.off('SIGTERM', interrupt);
    await stopWorker(worker, exited).catch((error: unknown) => {
      throw new Error(
        `${error instanceof Error ? error.message : '隔离 Host 清理失败'}（${diagnostic()}）`,
      );
    });
    console.log('隔离 Host 已关闭，临时测试数据已清理。');
  }
}

export async function main(args: string[]): Promise<void> {
  const options = parseOptions(args);
  if (options.kind === 'help') {
    console.log(usage);
    return;
  }
  if (!process.stdin.isTTY || !process.stdout.isTTY) throw new Error('请在交互式终端运行。');
  assertHostBaseline();
  if (options.kind === 'isolated-real') {
    const root = await prepareRealDevelopmentRoot();
    console.log(
      '独立真实 Host：crates/tui/.development/epoch154；数据保留，不使用现用 State Root。',
    );
    const host = await startRealDevelopmentHost(root);
    try {
      const modelTarget = await configureRealDevelopmentModel(host.connection);
      const session = await selectSession(host.connection, modelTarget);
      if (session) await runTui(root, session);
    } finally {
      await host.connection.close();
      console.log('独立数据已保留；本次启动的 Host 随 launcher 退出关闭。');
    }
    return;
  }
  if (options.kind === 'isolated') {
    await runIsolated();
    return;
  }
  const result = await connectExistingRuntimeHost({
    rootPath: options.root,
    protocol: { min: HOST_BASELINE.protocolVersion, max: HOST_BASELINE.protocolVersion },
    compositionId: HOST_BASELINE.compositionId,
  });
  if (result.kind !== 'connected') {
    if (result.kind === 'incompatible' || result.kind === 'upgrade_required') {
      const actual = result.handshake?.compatibilityEpoch ?? '未知';
      throw new Error(
        `Host 不兼容：本 TUI 需要 epoch 154，对端 epoch ${actual}。未升级或重启 Host。请用 --isolated 测试，不要让旧后端打开新版数据目录。`,
      );
    }
    throw new Error(`Host 未就绪（${result.kind}）；未启动或修改该 Host。可用 --isolated 测试。`);
  }
  let session: string | undefined;
  try {
    session =
      options.session ??
      (options.create
        ? await createSession(result.connection)
        : await selectSession(result.connection));
  } finally {
    await result.connection.close();
  }
  if (session) await runTui(options.root, session);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error: unknown) => {
    console.error(
      error instanceof Error ? error.message.replace(/[\p{C}]/gu, '').slice(0, 1000) : '启动失败。',
    );
    process.exitCode = 1;
  });
}
