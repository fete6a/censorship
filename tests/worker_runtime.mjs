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

function fixture({ queueError = false, remove = false, dmFail = false, adminsA = [11], pauseMember } = {}) {
  const settings = new Map();
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
          if (sql.includes('violations') && sql.includes('COUNT')) return {count:0};
          if (sql.includes('group_settings')) return settings.get(values[0]) || null;
          if (sql.includes('managed_chats')) return { status: 'active', chat_type: 'supergroup' };
          throw new Error(`Unexpected D1 read: ${sql}`);
        },
        async run() {
          statements.push({ sql, values, operation: 'write' });
          if (sql.includes('INSERT INTO group_settings')) {
            const row = settings.get(values[0]) || { dry_run: null, logs_enabled: 1 };
            row[sql.includes('(chat_id,dry_run)') ? 'dry_run' : 'logs_enabled'] = values[1];
            settings.set(values[0], row);
          }
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
        action: { type: 'choice', choice: remove ? 'remove' : 'allow', confidence: 1, probabilities: { allow: remove ? 0 : 1, review: 0, remove: remove ? 1 : 0 } },
        category: { type: 'choice', choice: remove ? 'spam' : 'none', confidence: 1, probabilities: { none: remove ? 0 : 1, spam: remove ? 1 : 0 } },
        severity: { type: 'choice', choice: 'none', confidence: 1, probabilities: { none: 1, critical: 0 } },
      } };
    }
  }
  context.fetch = async request => {
    const method = new URL(request.url).pathname.split('/').at(-1);
    const args = JSON.parse(await request.text());
    telegram.push({ method, args });
    if (method === 'getChatAdministrators') return Response.json({ ok: true, result: [
      ...(args.chat_id === groupA ? adminsA : [21]).map(id=>({status:'administrator',user:{id,is_bot:false}})),
      { status: 'administrator', user: { id: 99, is_bot: true } },
    ] });
    if (method === 'getChatMember' && args.user_id===22 && pauseMember) await pauseMember();
    if (method === 'getChatMember') return Response.json({ ok: true, result: {
      user: { id: args.user_id }, status: args.chat_id === groupA ? 'administrator' : 'member',
    } });
    if (method === 'deleteMessage') return Response.json({ok:true,result:true});
    if (method === 'sendMessage' && dmFail) return Response.json({ok:false},{status:403});
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
  return { worker, deliveries, statements, telegram, completed, errors, aiCalls, settings, consume };
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

function message(group, id = 456) {
  return { update_id: id, message: { message_id: id, date: Math.floor(Date.now()/1000), chat: { id: group, type: 'supergroup' }, from: { id: 22 }, text: 'A normal message' } };
}
await test('group administrator can turn dry-run off only for their group', async () => {
 const f = fixture(); await f.consume(command(`/dry-run ${groupA} off`));
 assert.equal(f.settings.get(groupA)?.dry_run, 0);
 assert.equal(f.settings.has(groupB), false);
});
await test('administrator of A cannot change dry-run or logs for B', async () => {
 for (const cmd of ['dry-run', 'logs']) { const f = fixture();
 await f.consume(command(`/${cmd} ${groupB} off`)); assert.equal(f.settings.size, 0); }
});
await test('review logs default on and go only to the reviewed group human administrators', async () => {
 const f = fixture(); await f.consume(message(groupA));
 const sends = f.telegram.filter(c => c.method === 'sendMessage');
 assert.equal(sends.length, 1); assert.equal(sends[0].args.chat_id, 11);
 assert.match(sends[0].args.text, /-100100/);
});
await test('logs off for group A does not disable group B logs or change dry-run', async () => {
 const f = fixture(); f.settings.set(groupA, {dry_run:0,logs_enabled:1});
 await f.consume(command(`/logs ${groupA} off`));
 assert.equal(f.settings.get(groupA).logs_enabled,0); assert.equal(f.settings.get(groupA).dry_run,0);
 f.telegram.length=0; await f.consume(message(groupA)); await f.consume(message(groupB,457));
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 assert.deepEqual(sends.map(c=>c.args.chat_id),[21]);
});
await test('dry_run on cancels pending bans in that group', async()=>{
 const f=fixture(); await f.consume(command(`/dry_run ${groupA} on`));
 assert.equal(f.settings.get(groupA)?.dry_run,1);
 const cancel=f.statements.find(s=>s.sql.includes('UPDATE pending_bans'));
 assert.equal(cancel?.values.at(-1),groupA);
});

await test('dry-run defaults safe and off enables deletion for only the chosen group',async()=>{
 const f=fixture({remove:true});
 await f.consume(message(groupA,501));
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
 await f.consume(command(`/dry-run ${groupA} off`));
 await f.consume(message(groupA,502)); await f.consume(message(groupB,503));
 assert.deepEqual(f.telegram.filter(c=>c.method==='deleteMessage').map(c=>c.args.chat_id),[groupA]);
});
await test('admin log delivery failure does not retry or roll back a review',async()=>{
 const f=fixture({dmFail:true}); await f.consume(message(groupA)); assert.equal(f.completed.has(456),true);
 assert.equal(f.statements.find(s=>s.sql.includes('UPDATE archived_messages'))?.values[0],'allow');
});
await test('log recipients are refreshed when group administrators change',async()=>{
 const admins=[11];const f=fixture({adminsA:admins});await f.consume(message(groupA,501));
 admins.splice(0,1,12);await f.consume(message(groupA,502));
 assert.deepEqual(f.telegram.filter(c=>c.method==='sendMessage').map(c=>c.args.chat_id),[11,12]);
});

await test('turning dry-run on stops a review waiting for a Telegram lookup',async()=>{
 let enter,release;const entered=new Promise(r=>enter=r),gate=new Promise(r=>release=r);
 const f=fixture({remove:true,pauseMember:async()=>{enter();await gate;}});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 const review=f.consume(message(groupA,510)); await entered;
 await f.consume({...command(`/dry_run ${groupA} on`),update_id:511});
 release();await review;
 assert.equal(f.telegram.some(c=>['deleteMessage','restrictChatMember','banChatMember'].includes(c.method)),false);
});
