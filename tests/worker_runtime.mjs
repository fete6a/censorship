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

function fixture({ queueError = false, remove = false, dmFail = false, adminsA = [11], pauseMember, flipAfterClaim = false, timer = false, strikeCount = 0, score = 1, noticeFail = false, memberErrorCode = null, categoryChoice = null, categoryConfidence = 1, scores = null, digestFail = false, groupUsernames = {[groupA]:'group_a',[groupB]:'group_b'}, groupTitles = {[groupA]:'Group A',[groupB]:'Group B'}, groupInvite = '', groupInfoFail = false } = {}) {
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
            {if(digestFail && values[0]===groupA)throw new Error('digest read unavailable');return reviews.get(`${values[0]}:${values[1]}`) || null;}
          if(sql.includes('SELECT message_id FROM archived_messages')) {
            const ids=[...reviews].filter(([key,row])=>key.startsWith(`${values[0]}:`)&&!['pending','deleted','previously_deleted'].includes(row.last_status)).map(([key])=>Number(key.split(':')[1]));
            return ids.length?{message_id:Math.max(...ids)}:null;
          }
          if (sql.includes('group_settings')) return settings.get(values[0]) || null;
          if (sql.includes('managed_chats')) return { status: 'active', chat_type: 'supergroup' };
          throw new Error(`Unexpected D1 read: ${sql}`);
        },
        async all() {
          statements.push({sql,values,operation:'read'});
          if(!sql.includes('FROM archived_messages'))throw new Error(`Unexpected D1 all: ${sql}`);
          if(digestFail && values[0]===groupA)throw new Error('digest read unavailable');
          const ids=JSON.parse(values[1]);
          const results=[...new Set(ids)].sort((a,b)=>a-b).flatMap(message_id=>{
            const row=reviews.get(`${values[0]}:${message_id}`);
            return !row||row.last_status==='pending'?[]:[{message_id,status:row.last_status,
              category:row.last_category,probability:row.last_probability}];
          });
          return {success:true,results,meta:{duration:0,changes:0,last_row_id:0,
            changed_db:false,size_after:0,rows_read:results.length,rows_written:0}};
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
    if(method==='getChat' && groupInfoFail && telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===args.chat_id).length>1) return Response.json({ok:false},{status:503});
    if (method === 'getChat') return Response.json({ ok: true, result: {
      id: args.chat_id, type: 'supergroup',title:groupTitles[args.chat_id],username:groupUsernames[args.chat_id],invite_link:groupInvite, description: 'Do not post advertisements',
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
  assert.match(response?.args.text || '', /\/scan\\_status/);
  assert.match(response?.args.text || '', /GROUP\\_ID/);
  assert.equal(response?.args.parse_mode,'MarkdownV2');
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
await test('20 event window groups private reviews by chat without leaking to other admins',async()=>{
 const f=fixture();
 const inputs=[
  ...Array.from({length:14},(_,i)=>message(groupA,800+i)),
  ...Array.from({length:4},(_,i)=>message(groupB,900+i))
 ];
 const results=await f.consumeBatch(inputs);
 assert.ok(results.every(x=>x.acked&&!x.retried));
 assert.equal(f.aiCalls.length,inputs.length,'independent model inference per message');
 assert.equal(f.telegram.filter(x=>x.method==='getChatAdministrators').length,2,
  'one administrator lookup per group in each Queue batch');
 const messages=f.telegram.filter(x=>x.method==='sendMessage');
 assert.deepEqual(messages.map(x=>x.args.chat_id).sort((a,b)=>a-b),[11,21]);
 const a=messages.find(x=>x.args.chat_id===11)?.args.text;
 const b=messages.find(x=>x.args.chat_id===21)?.args.text;
 assert.match(a,/审核摘要/);assert.match(a,/14 条/);
 assert.ok(a.includes('t.me/c/100/800'));
 assert.ok(!a.includes('t.me/c/200/900'));
 assert.match(b,/4 条/);assert.ok(b.includes('t.me/c/200/900'));
 assert.ok(!b.includes('t.me/c/100/800'));
 assert.ok(messages.every(x=>x.args.parse_mode==='MarkdownV2'));
 assert.ok(messages.every(x=>x.args.link_preview_options?.is_disabled===true));
});

await test('Queue reuses group descriptions per group without changing per-item AI or D1 writes',async()=>{
 const f=fixture();
 const updates=[...Array.from({length:12},(_,i)=>message(groupA,1100+i)),
                ...Array.from({length:8},(_,i)=>message(groupB,1200+i))];
 const outcome=await f.consumeBatch(updates);
 assert.ok(outcome.every(o=>o.acked&&!o.retried));
 assert.equal(f.aiCalls.length,20,'model inference still happens per message');
 assert.equal(f.telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===groupA).length,2,
  'one policy read plus one metadata read for group A digest');
 assert.equal(f.telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===groupB).length,2,
  'no cross-group policy mixing');
 assert.equal(f.statements.filter(s=>s.sql.includes('UPDATE archived_messages')).length,20);
 assert.ok(f.aiCalls.every(call=>call.input.state.group_description==='Do not post advertisements'));
});

await test('policy snapshots are reset between Queue deliveries and when a command intervenes',async()=>{
 const f=fixture();
 await f.consumeBatch([message(groupA,1301),message(groupA,1302)]);
 assert.equal(f.telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===groupA).length,2);
 f.telegram.length=0;
 const cmd={...command('/help',{id:groupA,type:'supergroup'}),update_id:1303};
 cmd.message.message_id=1303;
 await f.consumeBatch([message(groupA,1304),cmd,message(groupA,1305)]);
 assert.equal(f.telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===groupA).length,3,
  'command invalidates the cached description and digest requests metadata');
});

await test('a malformed update in the same batch retries independently of successful reviews',async()=>{
 const f=fixture();
 const result=await f.consumeBatch([message(groupA,880),{},message(groupB,881)]);
 assert.deepEqual(result.map(x=>[x.acked,x.retried]),[[true,false],[false,true],[true,false]]);
 assert.equal(f.aiCalls.length,2);
 assert.deepEqual(f.telegram.filter(x=>x.method==='sendMessage').map(x=>x.args.chat_id).sort(),[11,21]);
});

await test('edits of the same message inside one window are deduplicated in the digest',async()=>{
 const f=fixture({remove:true,scores:[0.75,0.916],categoryChoice:'harassment'});
 f.settings.set(groupA,{dry_run:0,logs_enabled:1,remove_threshold:0.8});
 const original=message(groupA,870);
 const edited={update_id:871,edited_message:{...original.message,text:'edited'}};
 const outcomes=await f.consumeBatch([original,edited]);
 assert.ok(outcomes.every(o=>o.acked));
 const summary=f.telegram.filter(x=>x.method==='sendMessage'&&x.args.chat_id===11&&
    x.args.text.includes('审核摘要'));
 assert.equal(summary.length,1);
 assert.match(summary[0].args.text,/1 条/);
 assert.equal(f.telegram.filter(x=>x.method==='deleteMessage').length,1);
});

await test('MarkdownV2 action notice uses numeric Telegram links without exposing deleted message link',async()=>{
 const f=fixture({remove:true,score:0.99,strikeCount:1});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,889));
 const notice=f.telegram.find(x=>x.method==='sendMessage'&&x.args.chat_id===groupA)?.args;
 assert.equal(notice?.parse_mode,'MarkdownV2');
 assert.ok(notice.text.includes('tg://user?id=22'));
 assert.match(notice.text,/已删除/);
 assert.ok(!notice.text.includes('t.me/c/100/889'),'deleted content is not linked');
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


await test('20 reviewed updates use one final-status read per group',async()=>{
 const f=fixture();const outcomes=await f.consumeBatch(Array.from({length:20},(_,i)=>message(groupA,1000+i)));
 assert.ok(outcomes.every(x=>x.acked&&!x.retried));
 const reads=f.statements.filter(s=>s.operation==='read'&&s.sql.includes('FROM archived_messages'));
 assert.equal(reads.length,1,'read the final committed statuses together');
 assert.equal(f.telegram.filter(c=>c.method==='sendMessage').length,1);
 assert.match(f.telegram.find(c=>c.method==='sendMessage').args.text,/20 条/);
});
await test('logs off skips all passive status reads and admin lookups',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consumeBatch(Array.from({length:20},(_,i)=>message(groupA,1100+i)));
 assert.equal(f.statements.filter(s=>s.operation==='read'&&s.sql.includes('FROM archived_messages')).length,0);
 assert.equal(f.telegram.filter(c=>c.method==='getChatAdministrators').length,0);
 assert.equal(f.aiCalls.length,20);
});
await test('multi-chunk digest fetches the current group administrators once',async()=>{
 const f=fixture({adminsA:[11,12]});
 await f.consumeBatch(Array.from({length:80},(_,i)=>message(groupA,1200+i)));
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 assert.ok(sends.filter(c=>c.args.chat_id===11).length>1,'test spans multiple chunks');
 assert.equal(f.telegram.filter(c=>c.method==='getChatAdministrators').length,1);
 for(const id of [11,12])assert.ok(sends.filter(c=>c.args.chat_id===id).every(c=>c.args.text.length<4096));
 assert.ok(sends.every(c=>[11,12].includes(c.args.chat_id)));
});
await test('a failed digest read cannot retry completed reviews or suppress another group digest',async()=>{
 const f=fixture({digestFail:true});
 const outcomes=await f.consumeBatch([message(groupA,1400),message(groupB,1401)]);
 assert.ok(outcomes.every(x=>x.acked&&!x.retried));
 assert.equal(f.completed.size,2);
 assert.deepEqual(f.telegram.filter(c=>c.method==='sendMessage').map(c=>c.args.chat_id),[21]);
});

await test('private digests identify each group by its own clickable name and numeric ID',async()=>{
 const f=fixture();await f.consumeBatch([message(groupA,1500),message(groupB,1501)]);
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 const a=sends.find(c=>c.args.chat_id===11)?.args.text||'';
 const b=sends.find(c=>c.args.chat_id===21)?.args.text||'';
 assert.ok(a.includes('[Group A](https://t.me/group_a)'));assert.ok(a.includes('-100100'));
 assert.ok(b.includes('[Group B](https://t.me/group_b)'));assert.ok(!b.includes('Group A'));
 assert.ok(!a.includes('group_b'));
});
await test('private supergroup name links to a surviving reviewed message without creating invitations',async()=>{
 const f=fixture({groupUsernames:{}});await f.consume(message(groupA,1502));
 const text=f.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.ok(text.includes('[Group A](https://t.me/c/100/1502)'));
 assert.equal(f.telegram.some(c=>c.method.includes('InviteLink')),false);
});
await test('private enforcement logs use an existing group invite link and escape its name',async()=>{
 const f=fixture({remove:true,groupUsernames:{},groupInvite:'https://t.me/+Existing_Invite-1',groupTitles:{[groupA]:'Group *A* [x]'}});
 f.settings.set(groupA,{dry_run:0,logs_enabled:1});await f.consume(message(groupA,1503));
 const sends=f.telegram.filter(c=>c.method==='sendMessage'&&c.args.chat_id===11);
 assert.ok(sends.length>0);
 for(const c of sends)assert.ok(c.args.text.includes('[Group \\*A\\* \\[x\\]](https://t.me/+Existing_Invite-1)'));
});
await test('group metadata failure keeps committed reviews and delivers a safe ID-based link',async()=>{
 const f=fixture({groupInfoFail:true,groupUsernames:{}});await f.consume(message(groupA,1504));
 assert.equal(f.completed.has(1504),true);
 const text=f.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.ok(text.includes('(https://t.me/c/100/1504)'));
 assert.ok(text.includes('-100100'));
});

await test('group metadata cannot inject an external URL into private logs',async()=>{
 const f=fixture({groupUsernames:{[groupA]:'x](https://evil.example)'},groupInvite:'https://evil.example/invite'});
 await f.consume(message(groupA,1505));
 const text=f.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.ok(text.includes('[Group A](https://t.me/c/100/1505)'));
 assert.ok(!text.includes('evil.example'));
});
