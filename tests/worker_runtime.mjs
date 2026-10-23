import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import vm from 'node:vm';
import test from 'node:test';

// Run the built Rust/Wasm artifact. Only Cloudflare's external services are doubled.
const build = path.resolve(process.argv[2] || 'build');
const context = vm.createContext({
  console, TextEncoder, TextDecoder, URL, URLSearchParams, Request, Response,
  Headers, ReadableStream, WritableStream, TransformStream, AbortController,
  AbortSignal, WebAssembly, Uint8Array, ArrayBuffer, SharedArrayBuffer, DataView,
  Map, Set, WeakMap, WeakSet, FinalizationRegistry, crypto, fetch,
  setTimeout, clearTimeout, addEventListener() {},
});
class WorkerEntrypoint {
  constructor(ctx, env) { this.ctx = ctx; this.env = env; }
}
const runtime = new vm.SyntheticModule(['WorkerEntrypoint'], function () {
  this.setExport('WorkerEntrypoint', WorkerEntrypoint);
}, { context });
const wasm = new WebAssembly.Module(await readFile(path.join(build, 'index_bg.wasm')));
const wasmModule = new vm.SyntheticModule(['default'], function () {
  this.setExport('default', wasm);
}, { context });
const module = new vm.SourceTextModule(await readFile(path.join(build, 'index.js'), 'utf8'), { context });
await module.link(specifier => {
  if (specifier === 'cloudflare:workers') return runtime;
  if (specifier.endsWith('.wasm')) return wasmModule;
  throw new Error(`Unexpected module: ${specifier}`);
});
await module.evaluate();

const secret = 'runtime-test-webhook-secret';
const groupA = -100100;
const groupB = -100200;

function fixture({ queueError = false } = {}) {
  const deliveries = [], statements = [], telegram = [], completed = new Set(), errors = [], aiCalls = [];
  context.console = { ...console, error: (...args) => errors.push(args.join(' ')) };
  class WorkerQueue {
    async send(body, options) {
      if (queueError) throw new Error('queue unavailable');
      // Cloudflare's JSON content type uses JSON serialization, not structured clone.
      deliveries.push(options?.contentType === 'json' ? JSON.parse(JSON.stringify(body)) : body);
    }
  }
  class D1Database {
    prepare(sql) {
      let values = [];
      return {
        bind(...args) { values = args; return this; },
        async first() {
          statements.push({ sql, values, operation: 'read' });
          if (sql.includes('processed_updates')) return { count: completed.has(values[0]) ? 1 : 0 };
          if (sql.includes('managed_chats')) return { status: 'active', chat_type: 'supergroup' };
          throw new Error(`Unexpected D1 read: ${sql}`);
        },
        async run() {
          statements.push({ sql, values, operation: 'write' });
          if (sql.includes('INSERT OR IGNORE INTO processed_updates')) completed.add(values[0]);
          return { success: true, results: [], meta: { duration: 0, changes: 1, last_row_id: 0,
            changed_db: false, size_after: 0, rows_read: 0, rows_written: 1 } };
        },
      };
    }
  }
  class Ai {
    async run(model, input) {
      aiCalls.push({ model, input: JSON.parse(JSON.stringify(input)) });
      return { answers: {
        action: { type: 'choice', choice: 'allow', confidence: 1, probabilities: { allow: 1, review: 0, remove: 0 } },
        category: { type: 'choice', choice: 'none', confidence: 1, probabilities: { none: 1 } },
        severity: { type: 'choice', choice: 'none', confidence: 1, probabilities: { none: 1, critical: 0 } },
      } };
    }
  }
  context.fetch = async request => {
    const method = new URL(request.url).pathname.split('/').at(-1);
    const args = JSON.parse(await request.text());
    telegram.push({ method, args });
    if (method === 'getChatMember') return Response.json({ ok: true, result: {
      user: { id: args.user_id }, status: args.chat_id === groupA ? 'administrator' : 'member',
    } });
    if (method === 'sendMessage') return Response.json({ ok: true, result: { message_id: 1 } });
    if (method === 'getChat') return Response.json({ ok: true, result: {
      id: args.chat_id, type: 'supergroup', description: 'Do not post advertisements',
    } });
    throw new Error(`Unexpected Telegram method: ${method}`);
  };
  const env = { WEBHOOK_SECRET: secret, BOT_TOKEN: 'test-only-token', MODERATION_QUEUE: new WorkerQueue(),
    DB: new D1Database(), AI: new Ai(), DRY_RUN: 'true', BOT_USERNAME: 'example_bot' };
  const worker = new module.namespace.default({ waitUntil() {} }, env);
  async function consume(update, { retry = false } = {}) {
    let acked = false, retried = false;
    await worker.queue({ queue: 'moderation', messages: [{ id: 'test-message', timestamp: new Date(),
      body: update, attempts: 1, ack() { acked = true; }, retry() { retried = true; } }],
      ackAll() { acked = true; }, retryAll() { retried = true; } });
    assert.equal(retried, retry);
    assert.equal(acked, !retry);
  }
  return { worker, deliveries, statements, telegram, completed, errors, aiCalls, consume };
}

await test('webhook preserves nested payload and IDs through JSON queue delivery', async () => {
  const f = fixture();
  const update = { update_id: 123456, message: { message_id: 42,
    chat: { id: -1001234567890, type: 'supergroup' }, text: 'queue contract probe' } };
  const response = await f.worker.fetch(new Request('https://bot.example/telegram', {
    method: 'POST', headers: { 'X-Telegram-Bot-Api-Secret-Token': secret }, body: JSON.stringify(update),
  }));
  assert.equal(response.status, 200);
  assert.deepEqual(f.deliveries, [update]);
});

await test('unauthenticated webhook is rejected before reading or enqueueing its body', async () => {
  const f = fixture();
  const response = await f.worker.fetch(new Request('https://bot.example/telegram', {
    method: 'POST', headers: { 'X-Telegram-Bot-Api-Secret-Token': 'wrong', 'Content-Length': '2000000' },
    body: 'not JSON',
  }));
  assert.equal(response.status, 403);
  assert.deepEqual(f.deliveries, []);
});

await test('queue failure prevents the webhook from acknowledging delivery', async () => {
  const f = fixture({ queueError: true });
  const response = await f.worker.fetch(new Request('https://bot.example/telegram', {
    method: 'POST', headers: { 'X-Telegram-Bot-Api-Secret-Token': secret }, body: '{"update_id":123}',
  }));
  assert.equal(response.status, 500);
});

await test('consumer retries malformed queued updates instead of acknowledging them', async () => {
  const f = fixture();
  await f.consume({}, { retry: true });
  assert.equal(f.completed.size, 0);
  assert.match(f.errors.join(' '), /Telegram update lacks update_id/);
});

await test('AI binding receives plain JSON state and nested questions for a group message', async () => {
  const f = fixture();
  await f.consume({ update_id: 456, message: { message_id: 43, from: { id: 11 },
    chat: { id: groupA, type: 'supergroup' }, text: 'A normal message' } });
  assert.equal(f.aiCalls.length, 1);
  assert.equal(f.aiCalls[0].model, '@cf/cloudflare/clef-omni');
  const input = f.aiCalls[0].input;
  assert.equal(input.state?.group_description, 'Do not post advertisements');
  assert.equal(input.state?.message_text, 'A normal message');
  assert.equal(input.questions?.action?.type, 'choice');
  assert.deepEqual(Object.keys(input.questions.action.criteria).sort(), ['allow', 'remove', 'review']);
  for (const field of ['images', 'audio', 'videos']) assert.deepEqual(input[field], []);
  const reviewed = f.statements.find(s => s.sql.includes('UPDATE archived_messages'));
  assert.equal(reviewed?.values[0], 'allow');
  assert.equal(f.errors.length, 0);
});

function command(text, chat = { id: 11, type: 'private' }) {
  return { update_id: 789, message: { message_id: 42, chat, from: { id: 11 }, text } };
}

await test('group A administrator cannot execute a private command against group B', async () => {
  const f = fixture();
  await f.consume(command(`/scan_stop ${groupB}`));
  assert.deepEqual(f.telegram, [{ method: 'getChatMember', args: { chat_id: groupB, user_id: 11 } }]);
  assert.equal(f.statements.some(s => s.sql.includes('managed_chats')), false);
  assert.equal(f.statements.some(s => s.sql.includes('UPDATE scan_jobs')), false);
  assert.equal(f.completed.has(789), true);
});

await test('group A administrator can execute a private command against group A', async () => {
  const f = fixture();
  await f.consume(command(`/scan_stop ${groupA}`));
  const changes = f.statements.filter(s => s.sql.includes('UPDATE scan_jobs'));
  assert.equal(changes.length, 1);
  assert.equal(changes[0].values.at(-1), groupA);
  assert.equal(f.telegram[0].args.chat_id, groupA);
});

await test('a command sent inside group A stays scoped to group A', async () => {
  const f = fixture();
  await f.consume(command(`/scan_stop ${groupB}`, { id: groupA, type: 'supergroup' }));
  const changes = f.statements.filter(s => s.sql.includes('UPDATE scan_jobs'));
  assert.equal(changes.length, 1);
  assert.equal(changes[0].values.at(-1), groupA);
  assert.equal(f.telegram[0].args.chat_id, groupA);
  assert.equal(f.telegram.some(c => c.method === 'sendMessage'), false, 'commands remain silent in groups');
});

await test('private commands reject positive IDs before reading any target group data', async () => {
  const f = fixture();
  await f.consume(command('/scan_stop 100200'));
  assert.equal(f.statements.some(s => s.sql.includes('managed_chats')), false);
  assert.equal(f.telegram.some(c => c.method === 'getChatMember'), false);
});

await test('start command returns command usage privately', async () => {
  const f = fixture();
  await f.consume(command('/start'));
  const response = f.telegram.find(c => c.method === 'sendMessage');
  assert.equal(response?.args.chat_id, 11);
  assert.match(response?.args.text || '', /\/scan_status/);
  assert.match(response?.args.text || '', /GROUP_ID/);
});
