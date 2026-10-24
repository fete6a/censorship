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

function fixture({ queueError = false, remove = false, dmFail = false, adminsA = [11], pauseMember, flipAfterClaim = false, timer = false, strikeCount = 0, score = 1, noticeFail = false, memberErrorCode = null, categoryChoice = null, categoryConfidence = 1, scores = null } = {}) {
  const settings = new Map();
  const deliveries = [], statements = [], telegram = [], completed = new Set(), strikes = new Set(), reviews = new Map(), errors = [], aiCalls = [];
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
          if ((flipAfterClaim || timer) && sql.includes('pending_bans')) {
            if(flipAfterClaim && sql.startsWith('UPDATE')) settings.set(groupA,{dry_run:1,logs_enabled:0});
            return {chat_id:groupA,user_id:22,message_id:42,due_at:Math.floor(Date.now()/1000)-5};
          }
          if (sql.includes('processed_updates')) return { count: completed.has(values[0]) ? 1 : 0 };
          if (sql.includes('violations') && sql.includes('COUNT')) return {count: sql.includes('user_id') ? strikeCount : Number(strikes.has(`${values[0]}:${values[1]}`))};
          if (sql.includes('pending_bans')) return null;
          if (sql.includes('SELECT last_status,last_category,last_probability FROM archived_messages'))
            return reviews.get(`${values[0]}:${values[1]}`) || null;
          if (sql.includes('group_settings')) return settings.get(values[0]) || null;
          if (sql.includes('managed_chats')) return { status: 'active', chat_type: 'supergroup' };
          throw new Error(`Unexpected D1 read: ${sql}`);
        },
        async run() {
          statements.push({ sql, values, operation: 'write' });
          if (sql.includes('INSERT INTO group_settings')) {
            const row = settings.get(values[0]) || { dry_run: null, logs_enabled: 1 };
            row[sql.includes('(chat_id,dry_run)') ? 'dry_run' : sql.includes('(chat_id,remove_threshold)') ? 'remove_threshold' : 'logs_enabled'] = values[1];
            settings.set(values[0], row);
          }
          if (sql.includes('INSERT OR IGNORE INTO processed_updates')) completed.add(values[0]);
          if (sql.includes('INSERT OR IGNORE INTO violations')) strikes.add(`${values[0]}:${values[2]}`);
          if (sql.includes('INSERT INTO archived_messages'))
            reviews.set(`${values[0]}:${values[1]}`, {last_status:'pending',last_category:'none',last_probability:0});
          if (sql.includes('UPDATE archived_messages'))
            reviews.set(`${values[4]}:${values[5]}`, {last_status:values[0],last_category:values[1],last_probability:values[2]});
          return { success: true, results: [], meta: { duration: 0, changes: 1, last_row_id: 0,
            changed_db: false, size_after: 0, rows_read: 0, rows_written: 1 } };
        },
      };
    }
  }
  class Ai {
    async run(model, input) {
      aiCalls.push({ model, input: JSON.parse(JSON.stringify(input)) });
      const currentScore = remove ? (scores?.[aiCalls.length - 1] ?? score) : 0;
      return { answers: {
        violation: { type: 'noul', noul: currentScore },
        category: { type: 'choice', choice: categoryChoice ?? (remove ? 'spam' : 'none'), confidence: categoryConfidence, probabilities: { none: remove ? 0 : 1, spam: remove ? 1 : 0 } },
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
    if (method === 'getChatMember' && args.chat_id===groupB && memberErrorCode !== null) return Response.json({ok:false},{status:memberErrorCode});
    if (method === 'getChatMember') return Response.json({ ok: true, result: {
      user: { id: args.user_id }, status: args.chat_id === groupA && args.user_id===11 ? 'administrator' : 'member',
    } });
    if (['restrictChatMember','banChatMember'].includes(method)) return Response.json({ok:true,result:true});
    if (method === 'deleteMessage') return Response.json({ok:true,result:true});
    if (method === 'sendMessage' && ((dmFail && args.chat_id>0)||(noticeFail && args.chat_id<0))) return Response.json({ok:false},{status:403});
    if (method === 'sendMessage') return Response.json({ ok: true, result: { message_id: 1 } });
    if (method === 'getChat') return Response.json({ ok: true, result: {
      id: args.chat_id, type: 'supergroup', description: 'Do not post advertisements',
    } });
    throw new Error(`Unexpected Telegram method: ${method}`);
  };
  const env = { WEBHOOK_SECRET: secret, BOT_TOKEN: 'test-only-token', MODERATION_QUEUE: new WorkerQueue(),
    DB: new D1Database(), AI: new Ai(), DRY_RUN: 'true', BOT_USERNAME: 'example_bot' };
  const worker = new module.namespace.default({ waitUntil() {} }, env);
  async function consumeBatch(updates) {
    const outcomes=updates.map(()=>({acked:false,retried:false}));
    await worker.queue({ queue: 'moderation', messages: updates.map((update,i)=>({
      id: `test-message-${i}`, timestamp: new Date(), body:update, attempts:1,
      ack(){outcomes[i].acked=true;},retry(){outcomes[i].retried=true;}
    })),
      ackAll(){ for(const o of outcomes)o.acked=true; },
      retryAll(){for(const o of outcomes)o.retried=true;}
    });
    return outcomes;
  }
  async function consume(update, { retry = false } = {}) {
    const [outcome]=await consumeBatch([update]);
    assert.equal(outcome.retried,retry);
    assert.equal(outcome.acked,!retry);
  }
  return { worker, deliveries, statements, telegram, completed, errors, aiCalls,
    settings, reviews, consume, consumeBatch, env };
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
  assert.equal(input.questions?.violation?.type, 'noul');
  assert.equal(input.questions?.action, undefined, 'model never chooses allow/remove');
  assert.deepEqual(Object.keys(input.questions.violation.criteria).sort(), ['false', 'true']);
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

await test('private command to inaccessible group acknowledges terminal errors but retries transient errors',async()=>{
  for(const code of [400,403]){
    const f=fixture({memberErrorCode:code});
    await f.consume(command(`/scan_stop ${groupB}`));
    assert.equal(f.completed.has(789),true);
    assert.equal(f.statements.some(s=>s.sql.includes('managed_chats')),false);
  }
  for(const code of [429,500,503]){
    const f=fixture({memberErrorCode:code});
    await f.consume(command(`/scan_stop ${groupB}`),{retry:true});
    assert.equal(f.completed.has(789),false);
  }
});
await test('group help commands cannot bypass archiving or model review',async()=>{
  for(const cmd of ['/start bad content','/help bad content']){
    const f=fixture(),m=message(groupA,991);
    m.message.text=cmd;m.message.from={id:11};
    await f.consume(m);
    assert.equal(f.aiCalls.length,1);
    assert.ok(f.statements.some(s=>s.sql.includes('INSERT INTO archived_messages')));
  }
});
await test('anonymous or unauthorized group administrative command cannot bypass review',async()=>{
  for(const anonymous of [true,false]){
    const f=fixture(),m=message(groupA,992);
    m.message.text='/scan_stop bad content';
    m.message.from={id:22,is_bot:false};
    if(anonymous)m.message.sender_chat={id:groupA};
    await f.consume(m);
    assert.equal(f.aiCalls.length,1);
  }
});

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

await test('dry-run enabled after claim cancels processing case without banning or retrying',async()=>{
 const f=fixture({flipAfterClaim:true});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume({kind:'ban_check',chat_id:groupA,user_id:22,message_id:42});
 const done=f.statements.find(s=>s.sql.includes("status='processing'")&&s.operation==='write');
 assert.equal(done?.values[0],'canceled');
 assert.equal(f.telegram.some(c=>c.method==='banChatMember'),false);
});

await test('threshold command is group scoped and cannot be used by another group administrator',async()=>{
 const f=fixture();
 await f.consume(command(`/threshold ${groupB} 0.75`));
 assert.equal(f.settings.size,0);
 assert.equal(f.statements.some(s=>s.sql.includes('group_settings')),false);
 await f.consume({...command(`/threshold ${groupA} 0.75`),update_id:790});
 assert.equal(f.settings.get(groupA)?.remove_threshold,0.75);
 assert.equal(f.settings.has(groupB),false);
});
await test('threshold confirmation and updates in a group cannot retarget another group',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(command('/threshold 75%',{id:groupA,type:'supergroup'}));
 assert.equal(f.settings.get(groupA)?.remove_threshold,0.75);
 assert.equal(f.settings.get(groupA)?.dry_run,0);assert.equal(f.settings.get(groupA)?.logs_enabled,0);
 assert.equal(f.telegram.find(c=>c.method==='sendMessage')?.args.chat_id,groupA);
 await f.consume({...command(`/threshold ${groupB} 0.5`,{id:groupA,type:'supergroup'}),update_id:790});
 assert.equal(f.settings.get(groupA)?.remove_threshold,0.75);assert.equal(f.settings.has(groupB),false);
});
await test('invalid thresholds do not overwrite the last valid setting',async()=>{
 for(const arg of ['NaN','Infinity','-0.1','1.01','101%','75','0.75 extra']){
  const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.85});
  await f.consume(command(`/threshold ${groupA} ${arg}`));
  assert.equal(f.settings.get(groupA)?.remove_threshold,0.85);
  assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/0.*1|%/);
 }
});

await test('noul violation score drives automatic deletion without an allow choice',async()=>{
 const f=fixture({remove:true,score:0.916,categoryChoice:'harassment',categoryConfidence:0.2});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
 await f.consume(message(groupA,704));
 assert.deepEqual(f.telegram.filter(c=>c.method==='deleteMessage').map(c=>c.args.message_id),[704]);
 assert.equal(f.statements.find(s=>s.sql.includes('UPDATE archived_messages'))?.values[0],'deleted');
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT OR IGNORE INTO violations')).length,1);
 assert.equal(f.aiCalls[0].input.questions.action,undefined);
 assert.equal(f.aiCalls[0].input.questions.violation.type,'noul');
});
await test('violation score threshold is inclusive and default remains 0.93',async()=>{
 for(const [score,shouldDelete] of [[0.7999,false],[0.8,true],[0.916,true]]){
  const f=fixture({remove:true,score,categoryChoice:'harassment'});
  f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
  await f.consume(message(groupA,705));
  assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),shouldDelete);
 }
 const f=fixture({remove:true,score:0.916,categoryChoice:'harassment'});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,706));
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
});
await test('high violation score with category none is needs_review instead of allow',async()=>{
 const f=fixture({remove:true,score:0.99,categoryChoice:'none'});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
 await f.consume(message(groupA,707));
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
 assert.equal(f.statements.find(s=>s.sql.includes('UPDATE archived_messages'))?.values[0],'needs_review');
});
await test('score below threshold is allow as computed by Rust, not a model label',async()=>{
 const f=fixture({remove:true,score:0.6,categoryChoice:'harassment'});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
 await f.consume(message(groupA,708));
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
 assert.equal(f.statements.find(s=>s.sql.includes('UPDATE archived_messages'))?.values[0],'allow');
});
await test('edited message crossing threshold creates only one deletion and strike',async()=>{
 const f=fixture({remove:true,scores:[0.75,0.916,0.99],categoryChoice:'harassment'});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
 const original=message(groupA,710);
 await f.consume(original);
 assert.equal(f.statements.find(s=>s.sql.includes('UPDATE archived_messages'))?.values[0],'allow');
 assert.equal(f.telegram.filter(c=>c.method==='deleteMessage').length,0);
 await f.consume({update_id:711,edited_message:{...original.message,text:'Edited text'}});
 assert.equal(f.telegram.filter(c=>c.method==='deleteMessage').length,1);
 assert.equal(f.telegram.find(c=>c.method==='deleteMessage').args.message_id,710);
 await f.consume({update_id:712,edited_message:{...original.message,text:'Edited text again'}});
 assert.equal(f.aiCalls.length,3);
 assert.equal(f.telegram.filter(c=>c.method==='deleteMessage').length,1);
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT OR IGNORE INTO violations')).length,1);
 assert.equal(f.statements.filter(s=>s.sql.includes('UPDATE archived_messages')).at(-1)?.values[0],'previously_deleted');
});

await test('a score below the default threshold is deleted only after an admin lowers that group threshold',async()=>{
 const f=fixture({remove:true,score:0.7956});f.env.DRY_RUN='false';
 await f.consume(message(groupA,600));
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
 await f.consume(command(`/threshold ${groupA} 0.75`));
 await f.consume(message(groupA,601));await f.consume(message(groupB,602));
 assert.deepEqual(f.telegram.filter(c=>c.method==='deleteMessage').map(c=>[c.args.chat_id,c.args.message_id]),[[groupA,601]]);
});
await test('deletion and warning are announced only in the affected group even with private logs off',async()=>{
 const f=fixture({remove:true,strikeCount:1});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,610));
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 assert.ok(sends.length>0);assert.ok(sends.every(c=>c.args.chat_id===groupA));
 assert.match(sends.map(c=>c.args.text).join(' '),/610.*已删除/);
 assert.match(sends.map(c=>c.args.text).join(' '),/警告.*22|22.*警告/);
 assert.ok(f.telegram.findIndex(c=>c.method==='deleteMessage')<f.telegram.findIndex(c=>c.method==='sendMessage'));
});
await test('third strike announces a cancellable pending ban without claiming the ban happened',async()=>{
 const f=fixture({remove:true,strikeCount:3});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,611));
 const text=f.telegram.filter(c=>c.method==='sendMessage').map(c=>c.args.text).join(' ');
 assert.match(text,/611.*已删除/);assert.match(text,/60.*封禁/);assert.match(text,/\/cancel 22/);
 assert.equal(f.telegram.some(c=>c.method==='banChatMember'),false);
});
await test('completed ban sends a notice in the same group only',async()=>{
 const f=fixture({timer:true});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume({kind:'ban_check',chat_id:groupA,user_id:22,message_id:42});
 assert.equal(f.telegram.filter(c=>c.method==='banChatMember').length,1);
 const sends=f.telegram.filter(c=>c.method==='sendMessage');assert.equal(sends.length,1);
 assert.equal(sends[0].args.chat_id,groupA);assert.match(sends[0].args.text,/22.*封禁/);
});
await test('failed public notification does not retry a completed deletion',async()=>{
 const f=fixture({remove:true,noticeFail:true,strikeCount:1});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,612));assert.equal(f.completed.has(612),true);
 assert.equal(f.telegram.filter(c=>c.method==='deleteMessage').length,1);
 assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===groupA));
});
await test('dry-run does not announce deletion or ban in the group',async()=>{
 const f=fixture({remove:true});await f.consume(message(groupA,613));
 assert.equal(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id<0),false);
});
