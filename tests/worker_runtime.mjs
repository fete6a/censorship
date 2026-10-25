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

function fixture({ pageFetch = null, pauseAdmins = null, pauseAi = null, pauseDelete = null, pauseDeleteTarget = null, deleteError = false, queueError = false, remove = false, dmFail = false, adminsA = [11], pauseMember, flipAfterClaim = false, timer = false, strikeCount = 0, score = 1, noticeFail = false, memberErrorCode = null, categoryChoice = null, categoryConfidence = 1, scores = null, digestFail = false, groupUsernames = {[groupA]:'group_a',[groupB]:'group_b'}, groupTitles = {[groupA]:'Group A',[groupB]:'Group B'}, groupInvite = '', groupInfoFail = false, owners = {[groupA]:{id:11,first_name:'Alice',last_name:'Owner'},[groupB]:{id:21,first_name:'Bob'}}, aiFail = false, rules = 'Do not post advertisements', policyFail = false, managedStatus = 'active', missingWarningTable = false, warningReadFailsAfterDelete = false, clock = null, onPendingRun = null, onCleanupDelete = null, failCleanup = null } = {}) {
  context.Date = clock ? class FakeDate extends Date { static now() { return clock.value; } } : Date;
  const settings = new Map(), archives = new Map(), jobs = new Map();
  const cleanupState = { last_success_day: -1, lease_id: 0, lease_until: 0 };
  const deliveries = [], statements = [], telegram = [], completed = new Set(), strikes = new Set(), reviews = new Map(), errors = [], aiCalls = [];
  const pageRequests = [];
  const warningAdjustments = new Map(), strikeEvents = [], warningReceiptIds = new Set();
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
          if (sql.startsWith('INSERT INTO cleanup_state')) {
            const now = Math.floor(context.Date.now() / 1000);
            if (cleanupState.last_success_day >= values[0] || cleanupState.lease_until > now) return null;
            cleanupState.lease_id += 1;
            cleanupState.lease_until = now + 1200;
            return {lease_id: cleanupState.lease_id};
          }
          if (sql.startsWith('UPDATE cleanup_state SET last_success_day=')) {
            const now = Math.floor(context.Date.now() / 1000);
            if (cleanupState.lease_id !== values[1] || cleanupState.lease_until <= now) return null;
            cleanupState.last_success_day = Math.max(cleanupState.last_success_day, values[0]);
            cleanupState.lease_until = 0;
            return {lease_id: cleanupState.lease_id};
          }
          if ((flipAfterClaim || timer) && sql.includes('pending_bans')) {
            if(flipAfterClaim && sql.startsWith('UPDATE')) settings.set(groupA,{dry_run:1,logs_enabled:0});
            return {chat_id:groupA,user_id:22,message_id:42,due_at:Math.floor(Date.now()/1000)-5};
          }
          if (sql.includes('INSERT INTO warning_edit_receipts')) {
             if (missingWarningTable) throw new Error('D1_ERROR: no such table: warning_edit_receipts');
             if (warningReceiptIds.has(values[0])) return null;
             warningReceiptIds.add(values[0]);
             warningAdjustments.set(`${values[1]}:${values[2]}`,{count:values[3],start:strikeEvents.length,actor:values[4]});
             return {count:values[0]};
           }
           if (sql.includes('INSERT INTO pending_bans') && sql.includes('SELECT ?1,?2,?3')) {
             const entry=warningAdjustments.get(`${values[0]}:${values[1]}`);
             const idx=strikeEvents.findIndex(x=>x.key===`${values[0]}:${values[1]}` && x.messageId===values[2]);
             if (idx<0 || (entry && idx<entry.start)) return null;
             const job=jobs.get(values[0]);
             if(values[5]!=null && (!job || job.generation!==values[5] || job.lease_id!==values[6] || job.status!=='running' || !job.claimed)) return null;
             return {count:values[0]};
           }
          if (sql.includes('SELECT message_id,payload FROM archived_messages')) {
            const [chat,cursor,max]=values;
            const next=[...archives.entries()].filter(([k])=>k.startsWith(chat+':'))
              .map(([k,p])=>({message_id:Number(k.split(':')[1]),payload:JSON.stringify(p)}))
              .filter(x=>x.message_id>cursor&&x.message_id<=max)
              .sort((a,b)=>a.message_id-b.message_id)[0];
            return next||null;
          }
          if (sql.includes('SELECT payload FROM archived_messages')) {
            const item=archives.get(`${values[0]}:${values[1]}`);
            return item?{payload:JSON.stringify(item)}:null;
          }
          if (sql.includes('COALESCE(MAX(message_id)')) {
            const ids=[...archives.keys()].filter(key=>key.startsWith(`${values[0]}:`)).map(key=>Number(key.split(':')[1]));
            return {max_id:ids.length?Math.max(...ids):0};
          }
          if (sql.startsWith('UPDATE scan_jobs SET lease_id=lease_id+1')) {
            const job=[...jobs.values()].find(x=>x.status==='running' && !x.claimed);
            if(!job)return null;
            job.claimed=true;job.lease_id=(job.lease_id||0)+1;
            return {...job};
          }
          if (sql.startsWith('UPDATE scan_jobs SET cursor_message_id=')) {
            const job=jobs.get(values[2]);
            if(!job||job.generation!==values[3]||job.lease_id!==values[4]||job.status!=='running'||!job.claimed||job.cursor_message_id!==values[5])return null;
            job.cursor_message_id=values[0];job.scanned++;job.flagged+=values[1];
            return {count:job.chat_id};
          }
          if (sql.startsWith("UPDATE scan_jobs SET status='done'")) {
            const job=jobs.get(values[0]);
            if(!job||job.generation!==values[1]||job.lease_id!==values[2]||job.status!=='running'||!job.claimed||job.cursor_message_id!==values[3])return null;
            job.status='done';job.claimed=false;
            return {count:job.chat_id};
          }
          if (sql.includes('FROM scan_jobs') || sql.startsWith('SELECT COUNT(*) AS count FROM scan_jobs')) {
            if(sql.includes('COUNT(*)')) {
              const j=jobs.get(values[0]);
              return {count:Number(Boolean(j&&j.generation===values[1]&&j.lease_id===values[2]&&j.status==='running'&&j.claimed))};
            }
            if(sql.includes("status='running'")) return [...jobs.values()].find(j=>j.status==='running') || null;
            return jobs.get(values[0]) || null;
          }
          if (sql.includes('INSERT INTO scan_jobs') && sql.includes('RETURNING max_message_id')) {
            if(jobs.get(values[0])?.status==='running') return null;
            const prior=jobs.get(values[0]);
            const job={chat_id:values[0],cursor_message_id:0,max_message_id:values[1],scanned:0,flagged:0,enforce:values[2],status:'running',generation:(prior?.generation||0)+1,lease_id:prior?.lease_id||0,claimed:false};
            jobs.set(values[0],job);
            return {max_id:job.max_message_id};
          }
          if (sql.includes('processed_updates')) return { count: completed.has(values[0]) ? 1 : 0 };
          if (sql.includes('SELECT COALESCE((SELECT base_count FROM warning_adjustments')) {
            if (missingWarningTable) throw new Error('D1_ERROR: no such table: warning_adjustments');
            if (warningReadFailsAfterDelete && telegram.some(call=>call.method==='deleteMessage'))
              throw new Error('D1_ERROR: transient failure after Telegram deletion');
            const key=`${values[0]}:${values[1]}`,entry=warningAdjustments.get(key);
            const newer=strikeEvents.filter((x,i)=>x.key===key && (!entry || i>=entry.start)).length;
            // Existing runtime fixtures supply strikeCount as the effective total.
            // After a manual baseline, model genuinely new strikes separately.
            return {count:entry ? entry.count+newer : Math.max(strikeCount,newer)};
          }
          if (sql.includes('violations') && sql.includes('COUNT'))
            return {count: sql.includes('user_id') ? strikeCount : Number(strikes.has(`${values[0]}:${values[1]}`))};
          if (sql.includes('pending_bans')) return null;
          if (sql.includes('SELECT last_status,last_category,last_probability FROM archived_messages'))
            {if(digestFail && values[0]===groupA)throw new Error('digest read unavailable');return reviews.get(`${values[0]}:${values[1]}`) || null;}
          if(sql.includes('SELECT message_id FROM archived_messages')) {
            const ids=[...reviews].filter(([key,row])=>key.startsWith(`${values[0]}:`)&&!['pending','deleted','previously_deleted'].includes(row.last_status)).map(([key])=>Number(key.split(':')[1]));
            return ids.length?{message_id:Math.max(...ids)}:null;
          }
          if (sql.includes('group_settings')) return settings.get(values[0]) || null;
          if (sql.includes('managed_chats')) return { status: managedStatus, chat_type: 'supergroup' };
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
          if (onPendingRun && sql.startsWith("UPDATE pending_bans SET status='canceled'")) await onPendingRun();
          if (sql.startsWith('UPDATE cleanup_state SET lease_until=')) {
            const now = Math.floor(context.Date.now() / 1000);
            if (cleanupState.lease_id === values[0] && cleanupState.lease_until > now)
              cleanupState.lease_until = now + 300;
          }
          if (sql.startsWith('DELETE FROM processed_updates')) {
            if (onCleanupDelete) await onCleanupDelete();
            if (failCleanup?.value) throw new Error('synthetic D1 cleanup failure');
          }
          if (sql.includes('INSERT INTO group_settings')) {
            const row = settings.get(values[0]) || { dry_run: null, logs_enabled: 1 };
            row[sql.includes('(chat_id,dry_run)') ? 'dry_run' : sql.includes('(chat_id,remove_threshold)') ? 'remove_threshold' : 'logs_enabled'] = values[1];
            if (sql.includes('logs_warn_only')) row.logs_warn_only = values[2];
            settings.set(values[0], row);
          }
          if (sql.includes('INSERT INTO scan_jobs')) throw new Error('Scan start must use atomic RETURNING');
          if (sql.includes("UPDATE scan_jobs SET status='stopped'") && jobs.has(values[sql.includes('lease_id=?')?0:1])) {
            const j=jobs.get(values[sql.includes('lease_id=?')?0:1]);
            if(!sql.includes('lease_id=?')||(j.generation===values[1]&&j.lease_id===values[2])) {
              j.status='stopped';j.generation++;j.claimed=false;
            }
          }
          if (sql.startsWith('UPDATE scan_jobs SET lease_until=0')) {
            const j=jobs.get(values[0]);
            if(j&&j.generation===values[1]&&j.lease_id===values[2])j.claimed=false;
          }
          if (sql.includes('INSERT OR IGNORE INTO processed_updates')) completed.add(values[0]);
          if (sql.includes('INSERT INTO violations')) {
            const key=`${values[0]}:${values[2]}`;
            if(!strikes.has(key))strikeEvents.push({key:`${values[0]}:${values[1]}`,messageId:values[2]});
            strikes.add(key);
          }
          if (sql.includes('INSERT INTO warning_edit_receipts'))
            warningAdjustments.set(`${values[0]}:${values[1]}`,{count:values[2],start:strikeEvents.length,actor:values[4]});
          if (sql.includes('INSERT INTO archived_messages')) {
            archives.set(`${values[0]}:${values[1]}`,JSON.parse(values[5]));
            reviews.set(`${values[0]}:${values[1]}`, {last_status:'pending',last_category:'none',last_probability:0});
          }
          if (sql.includes('UPDATE archived_messages') && (!sql.includes('EXISTS (') ||
              (jobs.get(values[4])?.generation===values[6] && jobs.get(values[4])?.lease_id===values[7] &&
               jobs.get(values[4])?.status==='running' && jobs.get(values[4])?.claimed)))
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
      if(pauseAi)await pauseAi(aiCalls.length);
      if(aiFail) throw new Error('inference unavailable');
      const currentScore = remove ? (scores?.[aiCalls.length - 1] ?? score) : 0;
      return { answers: {
        violation: { type: 'noul', noul: currentScore },
        category: { type: 'choice', choice: categoryChoice ?? (remove ? 'spam' : 'none'), confidence: categoryConfidence, probabilities: { none: remove ? 0 : 1, spam: remove ? 1 : 0 } },
        severity: { type: 'choice', choice: 'none', confidence: 1, probabilities: { none: 1, critical: 0 } },
      } };
    }
  }
  context.fetch = async request => {
    if (new URL(request.url).hostname !== "api.telegram.org") {
      pageRequests.push(request);
      if (!pageFetch) throw new Error("Unexpected page fetch");
      return pageFetch(request);
    }
    if (new URL(request.url).pathname.startsWith('/file/'))
      return new Response(new Uint8Array([1,2,3,4]));
    const method = new URL(request.url).pathname.split('/').at(-1);
    const args = JSON.parse(await request.text());
    telegram.push({ method, args });
    if (method === 'getFile') return Response.json({ok:true,result:{file_path:'fixtures/'+args.file_id,file_size:4}});
    if (method === 'getChatAdministrators' && pauseAdmins) await pauseAdmins(args.chat_id);
    if (method === 'getChatAdministrators') return Response.json({ ok: true, result: [
      ...(args.chat_id === groupA ? adminsA : [21]).map(id=>({status:owners[args.chat_id]?.id===id?'creator':'administrator',user:{id,is_bot:false,...(owners[args.chat_id]?.id===id?owners[args.chat_id]:{})}})),
      { status: 'administrator', user: { id: 99, is_bot: true } },
    ] });
    if (method === 'getChatMember' && args.user_id===22 && pauseMember) await pauseMember();
    if (method === 'getChatMember' && args.chat_id===groupB && memberErrorCode !== null) return Response.json({ok:false},{status:memberErrorCode});
    if (method === 'getChatMember') return Response.json({ ok: true, result: {
      user: { id: args.user_id }, status: args.chat_id === groupA && args.user_id===11 ? 'administrator' : 'member',
    } });
    if (['restrictChatMember','banChatMember'].includes(method)) return Response.json({ok:true,result:true});
    if (method === 'deleteMessage') {
      if(pauseDelete && (pauseDeleteTarget===null || args.message_id===pauseDeleteTarget))await pauseDelete();
      if(deleteError)return Response.json({ok:false},{status:403});
      return Response.json({ok:true,result:true});
    }
    if (method === 'sendMessage' && ((dmFail && args.chat_id>0)||(noticeFail && args.chat_id<0))) return Response.json({ok:false},{status:403});
    if (method === 'sendMessage') return Response.json({ ok: true, result: { message_id: 1 } });
    if(method==='getChat' && policyFail) return Response.json({ok:false},{status:503});
    if(method==='getChat' && groupInfoFail && telegram.filter(c=>c.method==='getChat'&&c.args.chat_id===args.chat_id).length>1) return Response.json({ok:false},{status:503});
    if (method === 'getChat') return Response.json({ ok: true, result: {
      id: args.chat_id, type: 'supergroup',title:groupTitles[args.chat_id],username:groupUsernames[args.chat_id],invite_link:groupInvite, description: rules,
    } });
    throw new Error(`Unexpected Telegram method: ${method}`);
  };
  const env = { WEBHOOK_SECRET: secret, BOT_TOKEN: 'test-only-token', MODERATION_QUEUE: new WorkerQueue(),
    DB: new D1Database(), AI: new Ai(), DRY_RUN: 'true', ARCHIVE_DAYS: '0', BOT_USERNAME: 'example_bot' };
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
  async function tick(scheduledTime=Date.now()){await worker.scheduled({cron:'* * * * *',scheduledTime});}
  return { worker, tick, deliveries, statements, telegram, completed, errors, aiCalls, pageRequests, cleanupState,
    settings, reviews, archives, jobs, warningAdjustments, warningReceiptIds, consume, consumeBatch, env };
}

await test('daily cleanup is not lost when midnight Cron starts after its scheduled minute', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock = {value: midnight + 61_000};
  const f = fixture({clock});
  await f.tick(midnight);
  assert.equal(f.statements.filter(s => s.sql.startsWith('DELETE FROM processed_updates')).length, 1);
});

await test('daily cleanup is not lost when pending work crosses the midnight minute', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock = {value: midnight + 59_000};
  const f = fixture({clock, onPendingRun: () => {clock.value = midnight + 61_000;}});
  await f.tick(midnight);
  assert.equal(f.statements.filter(s => s.sql.startsWith('DELETE FROM processed_updates')).length, 1);
});

await test('daily cleanup performs once per UTC day and retains ARCHIVE_DAYS=0 archive rows', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock = {value: midnight};
  const f = fixture({clock});
  f.archives.set('-100:1', {message_id:1});
  await f.tick(midnight);
  clock.value += 60_000;
  await f.tick(midnight + 60_000);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,1);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM archived_messages')).length,0);
  assert.equal(f.archives.size,1);
  assert.equal(f.cleanupState.last_success_day,Math.floor(midnight / 86_400_000));
  clock.value += 86_400_000;
  await f.tick(midnight + 86_460_000);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,2);
});

await test('failure does not record success and retries after a bounded backoff', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock = {value: midnight + 62_000}, failCleanup={value:true};
  const f = fixture({clock,failCleanup});
  await f.tick(midnight);
  assert.ok(f.errors.some(e=>e.includes('cleanup_failed:')));
  assert.equal(f.cleanupState.last_success_day,-1);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,1);
  failCleanup.value=false;
  clock.value += 299_000;
  await f.tick(midnight);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,1);
  clock.value += 1_000;
  await f.tick(midnight);
  assert.equal(f.cleanupState.last_success_day,Math.floor(clock.value / 86_400_000));
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,2);
});

await test('overlapping Cron invocations only perform one live cleanup', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock = {value: midnight + 62_000};
  let started, release;
  const begun = new Promise(resolve=>{started=resolve;});
  const pause = new Promise(resolve=>{release=resolve;});
  const f = fixture({clock,onCleanupDelete:async()=>{started();await pause;}});
  const active=f.tick(midnight);
  await begun;
  clock.value+=60_000;
  await f.tick(midnight+60_000);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,1);
  release();
  await active;
  assert.equal(f.cleanupState.lease_id,1);
  assert.ok(f.cleanupState.last_success_day>=0);
});

await test('interrupted cleanup lease expires and stale completion is fenced', async () => {
  const midnight = Date.parse('2026-10-25T00:00:00Z');
  const clock={value:midnight+62_000};
  let started, release, first=true;
  const begun=new Promise(resolve=>{started=resolve;});
  const pause=new Promise(resolve=>{release=resolve;});
  const f=fixture({clock,onCleanupDelete:async()=>{
    if(first){first=false;started();await pause;}
  }});
  const stale=f.tick(midnight);
  await begun;
  clock.value+=1_199_000;
  await f.tick(midnight);
  assert.equal(f.cleanupState.lease_id,1);
  clock.value+=1_000;
  await f.tick(midnight);
  assert.equal(f.cleanupState.lease_id,2);
  assert.ok(f.cleanupState.last_success_day>=0);
  release();
  await stale;
  assert.ok(f.errors.some(e=>e.includes('cleanup_completion_lost_lease')));
  assert.equal(f.cleanupState.lease_id,2);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,2);
});

await test('cleanup finishing after midnight credits the completion UTC day', async () => {
  const next = Date.parse('2026-10-26T00:00:00Z');
  const clock={value:next-1_000};
  const f=fixture({clock,onCleanupDelete:()=>{clock.value=next+1_000;}});
  await f.tick(next-60_000);
  assert.equal(f.cleanupState.last_success_day,Math.floor(next/86_400_000));
  await f.tick(next);
  assert.equal(f.statements.filter(s=>s.sql.startsWith('DELETE FROM processed_updates')).length,1);
});

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
  assert.equal(f.aiCalls[0].model, '@cf/cloudflare/clef-flash');
  const input = f.aiCalls[0].input;
  assert.equal(input.model, 'clef-flash');
  assert.equal(input.state?.group_description, 'Do not post advertisements');
  assert.equal(input.state?.message_text, 'A normal message');
  assert.equal(input.questions?.violation?.type, 'noul');
  assert.equal(input.questions?.action, undefined, 'model never chooses allow/remove');
  assert.deepEqual(Object.keys(input.questions.violation.criteria).sort(), ['false', 'true']);
  for (const field of ['images', 'audio', 'videos']) assert.equal(input[field], undefined);
  const reviewed = f.statements.find(s => s.sql.includes('UPDATE archived_messages'));
  assert.equal(reviewed?.values[0], 'allow');
  assert.equal(f.errors.length, 0);
});

await test('media with captions keeps the omni route and its actual media input',async()=>{
  for(const [media,field] of [
    [{photo:[{file_id:'photo',file_size:4}]},'images'],
    [{document:{file_id:'image-doc',mime_type:'image/png'}},'images'],
    [{sticker:{file_id:'sticker',is_animated:false,is_video:false}},'images'],
    [{audio:{file_id:'audio',mime_type:'audio/mpeg'}},'audio'],
    [{video:{file_id:'video',mime_type:'video/mp4',duration:2}},'videos'],
  ]){
    const f=fixture();const update=message(groupA,9500);
    Object.assign(update.message,media,{caption:'Caption is not a text-only message'});
    delete update.message.text;
    await f.consume(update);
    assert.equal(f.aiCalls.length,1);
    assert.equal(f.aiCalls[0].model,'@cf/cloudflare/clef-omni');
    assert.equal(f.aiCalls[0].input.model,'clef-omni');
    assert.equal(f.aiCalls[0].input[field].length,1);
    assert.match(f.aiCalls[0].input[field][0],/^data:.*;base64,/);
    assert.equal(f.aiCalls[0].input.state.caption,'Caption is not a text-only message');
  }
});

await test('unsupported or malformed media never falls back to flash caption-only review',async()=>{
  for(const media of [
    {document:{file_id:'pdf',mime_type:'application/pdf'}},
    {photo:[]},
    {photo:{file_id:'malformed-photo-array'}},
    {sticker:{file_id:'animated',is_animated:true}},
    {voice:{file_id:'voice',mime_type:'audio/ogg'}},
    {photo:[{file_id:'large',file_size:5*1024*1024}]},
  ]){
    const f=fixture({remove:true});const update=message(groupA,9501);
    f.settings.set(groupA,{dry_run:0,logs_enabled:0});
    Object.assign(update.message,media,{caption:'Do not discard this attachment'});
    await f.consume(update);
    assert.equal(f.aiCalls.length,0);
    assert.equal(f.reviews.get(groupA+':9501')?.last_status,'review_error');
    assert.equal(f.telegram.some(x=>['deleteMessage','restrictChatMember','banChatMember'].includes(x.method)),false);
  }
});

function command(text, chat = { id: 11, type: 'private' }) {
  return { update_id: 789, message: { message_id: 42, chat, from: { id: 11 }, text } };
}

await test('group A administrator cannot execute a private command against group B', async () => {
  const f = fixture();
  await f.consume(command(`/scan_stop ${groupB}`));
  assert.deepEqual(f.telegram.filter(c=>c.method==='getChatMember'), [{ method: 'getChatMember', args: { chat_id: groupB, user_id: 11 } }]);
  assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'', /管理员/);
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
  assert.equal(f.telegram.find(c => c.method === 'sendMessage')?.args.chat_id,groupA);
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
 assert.equal(f.telegram.some(x=>['restrictChatMember','banChatMember'].includes(x.method)),false);
 assert.equal(f.reviews.get(groupA+':870')?.last_status,'deleted');
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

await test('logs warn is persisted per group and leaves dry-run, threshold and other groups unchanged',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0,remove_threshold:0.8});
 await f.consume(command(`/logs ${groupA} warn`));
 assert.deepEqual(f.settings.get(groupA),{dry_run:0,logs_enabled:1,remove_threshold:0.8,logs_warn_only:1});
 assert.equal(f.settings.has(groupB),false);
 assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/warn/);
 assert.equal(f.statements.filter(s=>s.operation==='write'&&s.sql.includes('INSERT INTO group_settings')).length,1);
 f.telegram.length=0;await f.consume({...command(`/logs ${groupA}`),update_id:790});
 assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/warn/);
});
await test('logs on restores full logs and off disables them after warn',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:1});
 await f.consume(command(`/logs ${groupA} on`));assert.equal(f.settings.get(groupA).logs_warn_only,0);assert.equal(f.settings.get(groupA).logs_enabled,1);
 f.telegram.length=0;await f.consume(message(groupA,2100));assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===11));
 await f.consume({...command(`/logs ${groupA} off`),update_id:2101});assert.equal(f.settings.get(groupA).logs_enabled,0);
 f.telegram.length=0;await f.consume(message(groupA,2102));assert.equal(f.telegram.some(c=>c.method==='sendMessage'),false);
});
await test('logs warn accepts addressed group commands but cannot retarget another group',async()=>{
 const f=fixture();await f.consume(command('/logs@example_bot warn',{id:groupA,type:'supergroup'}));
 assert.equal(f.settings.get(groupA).logs_warn_only,1);
 await f.consume({...command(`/logs ${groupB} warn`),update_id:2110});assert.equal(f.settings.has(groupB),false);
});
await test('logs only accepts on warn off and rejects the singular log alias and extra only argument',async()=>{
 for(const text of [`/logs ${groupA} warn only`,`/logs ${groupA} warn(only)`,`/logs ${groupA} only`,`/log ${groupA} warn`]) {
  const f=fixture();await f.consume(command(text));assert.equal(f.settings.size,0,text);
  assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===11),text);
 }
});
await test('warn mode suppresses allowed digests and avoids administrator lookups without affecting reviews',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:1});
 const outcomes=await f.consumeBatch(Array.from({length:20},(_,i)=>message(groupA,2200+i)));
 assert.ok(outcomes.every(o=>o.acked&&!o.retried));assert.equal(f.aiCalls.length,20);
 assert.equal(f.telegram.some(c=>c.method==='getChatAdministrators'||c.method==='sendMessage'),false);
});
await test('warn mode keeps only flagged rows in mixed digests while another group still receives allows',async()=>{
 const f=fixture({remove:true,scores:[0.1,0.99,0.1]});f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:1});
 await f.consumeBatch([message(groupA,2300),message(groupA,2301),message(groupB,2302)]);
 const a=f.telegram.find(c=>c.method==='sendMessage'&&c.args.chat_id===11)?.args.text||'';
 assert.match(a,/1 条/);assert.ok(a.includes('2301'));assert.ok(!a.includes('2300'));assert.match(a,/模拟命中/);
 assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===21&&c.args.text.includes('2302')));
});
await test('retryable policy lookup errors alert only permitted admins in on or warn modes',async()=>{
 for(const mode of ['on','warn','off']) {
  const f=fixture({policyFail:true});
  f.settings.set(groupA,{dry_run:0,logs_enabled:mode==='off'?0:1,logs_warn_only:mode==='warn'?1:0});
  await f.consume(message(groupA,2390),{retry:true});
  assert.equal(f.reviews.get(`${groupA}:2390`)?.last_status,'policy_unavailable');
  assert.equal(f.completed.has(2390),false,'retryable error must not be acknowledged');
  assert.equal(f.telegram.some(c=>['deleteMessage','banChatMember','restrictChatMember'].includes(c.method)),false);
  const notices=f.telegram.filter(c=>c.method==='sendMessage');
  if (mode==='off') assert.equal(notices.length,0);
  else {
   assert.equal(notices.length,1,'send only a scoped warning on this attempt');
   assert.equal(notices[0].args.chat_id,11,'use the current human administrator, not a global target');
   assert.match(notices[0].args.text,/群规查询失败/);
   assert.match(notices[0].args.text,/2390/);
  }
 }
});
await test('warn mode retains audit hits, uncertain decisions and inference failures without fake scores',async()=>{
 for(const options of [{remove:true},{remove:true,score:.99,categoryChoice:'none'},{aiFail:true},{rules:''}]) {
  const f=fixture(options);f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:1});await f.consume(message(groupA,2400));
  const log=f.telegram.find(c=>c.method==='sendMessage'&&c.args.chat_id===11)?.args.text||'';assert.ok(log.includes('2400'));
  if(options.aiFail||options.rules==='')assert.ok(!log.includes('违规概率'));
 }
 const audit=fixture({remove:true});audit.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:1});
 const archived=message(groupA,2401);audit.archives.set(`${groupA}:2401`,archived.message);
 await audit.consume(command(`/scan ${groupA} 2401 audit`));
 assert.ok(audit.telegram.some(c=>c.method==='sendMessage'&&c.args.text.includes('审计命中')));
});
await test('warn mode keeps actual deletion and ban notices without changing enforcement',async()=>{
 const f=fixture({remove:true,strikeCount:1});f.settings.set(groupA,{dry_run:0,logs_enabled:1,logs_warn_only:1});await f.consume(message(groupA,2500));
 assert.equal(f.telegram.filter(c=>c.method==='deleteMessage').length,1);
 assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===groupA&&c.args.text.includes('违规消息已删除')));
 assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===11&&c.args.text.includes('违规消息已删除')));
 const ban=fixture({timer:true});ban.settings.set(groupA,{dry_run:0,logs_enabled:1,logs_warn_only:1});
 await ban.consume({kind:'ban_check',chat_id:groupA,user_id:22,message_id:42});
 assert.ok(ban.telegram.some(c=>c.method==='banChatMember'));
 assert.ok(ban.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===11&&c.args.text.includes('封禁已执行')));
});
await test('changing on to warn or off while administrator lookup waits suppresses allows',async()=>{
 for(const mode of ['warn','off']) {
  let enter,release;const entered=new Promise(r=>enter=r),held=new Promise(r=>release=r);
  const f=fixture({pauseAdmins:async()=>{enter();await held;}});f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:0});
  const pending=f.consume(message(groupA,2600));await entered;
  await f.consume(command(`/logs ${groupA} ${mode}`));release();await pending;
  assert.equal(f.telegram.some(c=>c.method==='sendMessage'&&c.args.text.includes('审核摘要')),false,mode);
 }
});
await test('warn suppresses routine scan completion while command replies remain visible',async()=>{
 for(const warn of [0,1]) {
  const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:1,logs_warn_only:warn});
  await f.consume(command(`/scan_all ${groupA} audit`));
  assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===11&&c.args.text.includes('历史扫描')));
  f.telegram.length=0;await f.tick();
  const completed=f.telegram.some(c=>c.method==='sendMessage'&&c.args.text.includes('历史审查完成'));
  assert.equal(completed,!warn);
 }
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
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO violations')).length,1);
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
await test('other and adult categories behave like none, without punishment or distinct stored categories',async()=>{
  for(const categoryChoice of ['none','other','adult']) for(const [score,expected] of [[0.99,'needs_review'],[0.6,'allow']]){
    const f=fixture({remove:true,score,categoryChoice});
    f.settings.set(groupA,{dry_run:0,logs_enabled:1,remove_threshold:0.8});
    await f.consume(message(groupA,1707));
    assert.deepEqual(f.reviews.get(`${groupA}:1707`),
      {last_status:expected,last_category:'none',last_probability:score});
    assert.equal(f.telegram.some(c=>['deleteMessage','restrictChatMember','banChatMember'].includes(c.method)),false);
    assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO violations')).length,0);
    const digest=f.telegram.find(c=>c.method==='sendMessage'&&c.args.chat_id>0)?.args.text||'';
    assert.ok(digest.includes(expected==='needs_review'?'人工复核':'通过'));
    assert.ok(!digest.includes('其他违反群规'));
    assert.ok(!digest.includes('不当成人内容'));
  }
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
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO violations')).length,1);
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
 assert.match(sends.map(c=>c.args.text).join(' '),/已删除/);
 assert.ok(sends.some(c=>c.args.text.includes('`#610`')));
 assert.ok(sends.some(c=>c.args.text.includes('tg://user?id=22')&&c.args.text.includes('近 7 天警告')));
 assert.ok(f.telegram.findIndex(c=>c.method==='deleteMessage')<f.telegram.findIndex(c=>c.method==='sendMessage'));
});
await test('third strike announces a cancellable pending ban without claiming the ban happened',async()=>{
 const f=fixture({remove:true,strikeCount:3});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,611));
 const text=f.telegram.filter(c=>c.method==='sendMessage').map(c=>c.args.text).join(' ');
 assert.match(text,/已删除/);assert.ok(text.includes('`#611`'));assert.match(text,/60.*封禁/);assert.match(text,/\/cancel 22/);
 assert.equal(f.telegram.some(c=>c.method==='banChatMember'),false);
});
await test('completed ban sends a notice in the same group only',async()=>{
 const f=fixture({timer:true});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume({kind:'ban_check',chat_id:groupA,user_id:22,message_id:42});
 assert.equal(f.telegram.filter(c=>c.method==='banChatMember').length,1);
 const sends=f.telegram.filter(c=>c.method==='sendMessage');assert.equal(sends.length,1);
 assert.equal(sends[0].args.chat_id,groupA);assert.match(sends[0].args.text,/封禁/);assert.ok(sends[0].args.text.includes('tg://user?id=22'));
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

await test('every supported command replies in its original chat and topic with private logs off',async()=>{
 for(const privateChat of [false,true]) {
  for(const text of ['/start','/help','/threshold','/dry-run','/dry_run','/logs','/rules','/cancel 22','/pending 22','/warnings 22','/scan','/scan_id','/scan_all','/scan_status','/scan_stop']) {
   const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
   const [cmd,...args]=text.split(' ');
   const update=command(privateChat&&!['/start','/help'].includes(cmd)?[cmd,groupA,...args].join(' '):text,
     {id:privateChat?11:groupA,type:privateChat?'private':'supergroup'});
   if(!privateChat)update.message.message_thread_id=77;
   await f.consume(update);
   const replies=f.telegram.filter(c=>c.method==='sendMessage');
   assert.ok(replies.length>=1,text);
   assert.ok(replies.every(c=>c.args.chat_id===(privateChat?11:groupA)),text);
   for(const {args} of replies) {
    assert.equal(args.reply_parameters?.message_id,42,text);
    assert.equal(args.reply_parameters?.allow_sending_without_reply,true,text);
    assert.equal(args.message_thread_id,privateChat?undefined:77,text);
    assert.equal(args.parse_mode,'MarkdownV2',text);
   }
  }
 }
});

await test('unknown commands and invalid arguments get feedback without changing group settings',async()=>{
 for(const text of ['/unknown','/dry_run maybe','/logs maybe','/pending','/cancel','/threshold 101%']) {
  const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  await f.consume(command(text,{id:groupA,type:'supergroup'}));
  assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===groupA),text);
  assert.equal(f.statements.some(s=>s.sql.includes('INSERT INTO group_settings')),false);
 }
 const f=fixture();await f.consume(command('/unknown'));
 assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/未知命令/);
});

await test('commands addressed to another bot get no command reply and remain subject to group review',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 await f.consume(command('/scan@another_bot',{id:groupA,type:'supergroup'}));
 assert.equal(f.telegram.some(c=>c.method==='sendMessage'),false);
 assert.equal(f.aiCalls.length,1);
  assert.equal(f.telegram.some(c=>c.method==='deleteMessage'),false);
});

await test('disabled groups receive command feedback and execute no management action',async()=>{
 const f=fixture({managedStatus:'disabled'});
 await f.consume(command('/scan_stop',{id:groupA,type:'supergroup'}));
 assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/权限不足/);
 assert.equal(f.statements.some(s=>s.sql.includes('UPDATE scan_jobs')),false);
});

await test('explicit audit scan reports findings without sanctions in the same topic',async()=>{
 const f=fixture({remove:true,score:0.99});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 const update=command('/scan audit',{id:groupA,type:'supergroup'});
 update.message.reply_to_message=message(groupA,1600).message;update.message.message_thread_id=88;
 await f.consume(update);
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 assert.equal(sends.length,2);
 assert.match(sends[0].args.text,/正在审查消息 1600/);
 assert.match(sends[1].args.text,/审计命中.*广告/);
 assert.match(sends[1].args.text,/▰+.*99/);
 assert.ok(sends.every(c=>c.args.chat_id===groupA&&c.args.message_thread_id===88));
 assert.equal(f.aiCalls.length,1);
 assert.deepEqual(f.telegram.filter(c=>c.method==='deleteMessage').map(c=>c.args.message_id),[42]);
 assert.equal(f.telegram.some(c=>c.method==='deleteMessage'&&c.args.message_id===1600),false);
 assert.ok(f.archives.has(`${groupA}:1600`));
});

await test('scan_id reads only the authorized group archive and replies privately with logs off',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 f.archives.set(`${groupA}:1601`,message(groupA,1601).message);
 await f.consume(command(`/scan_id ${groupA} 1601`));
 const sends=f.telegram.filter(c=>c.method==='sendMessage');
 assert.equal(sends.length,2);assert.ok(sends.every(c=>c.args.chat_id===11));
 assert.match(sends[1].args.text,/1601.*通过/);
 const reads=f.statements.filter(s=>s.sql.includes('SELECT payload FROM archived_messages'));
 assert.deepEqual(reads.map(s=>s.values),[[groupA,1601]]);
});

await test('scan rejects missing, ineligible and cross-group targets with visible feedback',async()=>{
 for(const target of [undefined,{...message(groupA,1602).message,from:{id:99,is_bot:true}},message(groupB,1602).message]) {
  const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const update=command('/scan 99999',{id:groupA,type:'supergroup'});
  if(target)update.message.reply_to_message=target;
  await f.consume(update);
  assert.equal(f.aiCalls.length,0);
  assert.equal(f.archives.size,0);
  assert.ok(f.telegram.some(c=>c.method==='sendMessage'&&c.args.chat_id===groupA));
 }
});

await test('scan reports model failure or missing group rules without claiming a pass',async()=>{
 for(const options of [{aiFail:true},{rules:''}]) {
  const f=fixture(options);f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const update=command('/scan',{id:groupA,type:'supergroup'});
  update.message.reply_to_message=message(groupA,1603).message;
  await f.consume(update);
  const final=f.telegram.filter(c=>c.method==='sendMessage').at(-1)?.args.text||'';
  assert.match(final,/未获得有效审核结果/);
  assert.ok(!final.includes('通过'));assert.ok(!final.includes('违规概率'));
  assert.equal(f.telegram.some(c=>c.method==='deleteMessage'&&c.args.message_id===1603),false);
 }
});

await test('transient command failures reply safely and remain retryable',async()=>{
 const f=fixture({policyFail:true});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 const update=command('/scan',{id:groupA,type:'supergroup'});
 update.message.reply_to_message=message(groupA,1604).message;
 await f.consume(update,{retry:true});
 assert.match(f.telegram.filter(c=>c.method==='sendMessage').at(-1)?.args.text||'',/服务暂不可用/);
 assert.equal(f.completed.has(update.update_id),false);
});

await test('scan enforce preserves dry-run and duplicate punishment protections',async()=>{
 for(const dryRun of [0,1]) {
  const f=fixture({remove:true,strikeCount:1});f.settings.set(groupA,{dry_run:dryRun,logs_enabled:0});
  const update=command('/scan enforce',{id:groupA,type:'supergroup'});
  update.message.reply_to_message=message(groupA,1605).message;
  await f.consume(update);
  await f.consume({...update,update_id:update.update_id+1});
  assert.equal(f.telegram.filter(c=>c.method==='deleteMessage'&&c.args.message_id===1605).length,dryRun?0:1);
  assert.equal(f.telegram.filter(c=>c.method==='banChatMember').length,0);
  assert.match(f.telegram.filter(c=>c.method==='sendMessage').at(-1)?.args.text||'',dryRun?/模拟命中/:/已处理/);
 }
});

await test('failed command replies do not retry completed management writes',async()=>{
 const f=fixture({noticeFail:true});f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 const update=command('/dry_run off',{id:groupA,type:'supergroup'});
 await f.consume(update);await f.consume(update);
 assert.equal(f.settings.get(groupA).dry_run,0);
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO group_settings')).length,1);
 assert.ok(f.errors.some(e=>e.includes('command_reply_delivery_failed')));
});

await test('digests and action logs show the current creator name and numeric profile link',async()=>{
 const owners={[groupA]:{id:11,first_name:'Alice *[x]*',last_name:'Owner'},[groupB]:{id:21,first_name:'Bob'}};
 const f=fixture({owners,remove:true,strikeCount:1});f.settings.set(groupA,{dry_run:0,logs_enabled:1});
 await f.consume(message(groupA,1700));await f.consume(message(groupB,1701));
 const a=f.telegram.filter(c=>c.method==='sendMessage'&&c.args.chat_id===11);
 assert.ok(a.length>=2);
 for(const c of a)assert.ok(c.args.text.includes('群主：[Alice \\*\\[x\\]\\* Owner](tg://user?id=11)'));
 const b=f.telegram.find(c=>c.method==='sendMessage'&&c.args.chat_id===21)?.args.text||'';
 assert.ok(b.includes('群主：[Bob](tg://user?id=21)'));assert.ok(!b.includes('Alice'));
 assert.equal(f.telegram.filter(c=>c.method==='getChatAdministrators').length,3,'no extra lookup for owner');
});

await test('creator changes and absent names use the current administrator snapshot',async()=>{
 const admins=[11],owners={[groupA]:{id:11,first_name:'Alice'}};
 const f=fixture({adminsA:admins,owners});await f.consume(message(groupA,1702));
 admins.splice(0,1,12);owners[groupA]={id:12};
 await f.consume(message(groupA,1703));
 const text=f.telegram.filter(c=>c.method==='sendMessage').at(-1)?.args.text||'';
 assert.ok(text.includes('[用户 12](tg://user?id=12)'));assert.ok(!text.includes('Alice'));
 const missing=fixture({owners:{}});await missing.consume(message(groupA,1704));
 assert.match(missing.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/群主：暂不可用/);
});

await test('group warnings display escaped user names and the seven-day count as a three-step Markdown bar',async()=>{
 for(const [count,bar] of [[1,'▰▱▱'],[2,'▰▰▱'],[3,'▰▰▰'],[5,'▰▰▰']]) {
  const f=fixture({remove:true,strikeCount:count});f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const update=message(groupA,1800);update.message.from.first_name='Eve *[x]*';update.message.from.last_name='Smith';
  await f.consume(update);
  const text=f.telegram.find(c=>c.method==='sendMessage'&&c.args.chat_id===groupA)?.args.text||'';
  assert.ok(text.includes('[Eve \\*\\[x\\]\\* Smith](tg://user?id=22)'));
  assert.ok(text.includes(`近 7 天警告：\`${count}/3\` \`${bar}\``));
  assert.equal(text.split('`#1800`').length-1,1,'message ID appears once');
  assert.equal(f.archives.get(`${groupA}:1800`).from.first_name,'Eve *[x]*');
 }
});

await test('administrator probability bars keep the exact score and omit duplicate or unavailable fields',async()=>{
 const f=fixture({remove:true,score:0.7956});await f.consume(message(groupA,1801));
 const text=f.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.ok(text.includes('`▰▰▰▰▰▰▰▰▱▱`'));assert.ok(text.includes('79\\.6%'));
 assert.equal(text.split('`'+groupA+'`').length-1,1,'group ID appears once per digest');
 assert.ok(!text.includes('none'));
 const fail=fixture({aiFail:true});await fail.consume(message(groupA,1802));
 const failed=fail.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.match(failed,/审核暂不可用/);assert.ok(!failed.includes('0\\.0%'));assert.ok(!failed.includes('▰'));
});

await test('policy cache does not retain failed lookups or cross a membership change',async()=>{
 const fail=fixture({policyFail:true});
 const outcomes=await fail.consumeBatch([message(groupA,1900),message(groupA,1901)]);
 assert.ok(outcomes.every(o=>o.retried&&!o.acked));
 assert.equal(fail.telegram.filter(c=>c.method==='getChat').length,4,'two policy attempts plus two best-effort admin log metadata lookups');
 const f=fixture();
 await f.consumeBatch([message(groupA,1902),{update_id:1903,chat_member:{chat:{id:groupA}}},message(groupA,1904)]);
 assert.equal(f.telegram.filter(c=>c.method==='getChat').length,3);
 assert.equal(f.aiCalls.length,2);
});

await test('warning text does not promise a ban when automatic banning is disabled',async()=>{
 const f=fixture({remove:true,strikeCount:2});f.env.ENABLE_BAN='false';
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});await f.consume(message(groupA,1905));
 const text=f.telegram.find(c=>c.method==='sendMessage')?.args.text||'';
 assert.match(text,/自动封禁未启用/);assert.ok(text.includes('`2/3`'));
 assert.ok(!text.includes('复核期结束后封禁'));
});

await test('any group member can read rules in the same topic with logging and moderation disabled',async()=>{
 for(const anonymous of [false,true]) {
  const f=fixture({managedStatus:'disabled',rules:'No *spam* [links] (please)!'});
  f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const update=message(groupA,2000);update.message.text='/rules';update.message.message_thread_id=101;
  if(anonymous)update.message.sender_chat={id:groupA};
  await f.consume(update);
  const sends=f.telegram.filter(c=>c.method==='sendMessage');assert.equal(sends.length,1);
  assert.equal(sends[0].args.chat_id,groupA);assert.equal(sends[0].args.message_thread_id,101);
  assert.ok(sends[0].args.text.includes('No \\*spam\\* \\[links\\] \\(please\\)\\!'));
  assert.equal(f.telegram.some(c=>['getChatMember','getMe'].includes(c.method)),false);
  assert.equal(f.aiCalls.length,0);assert.equal(f.archives.size,0);
  assert.equal(f.statements.some(s=>s.sql.includes('managed_chats')),false);
 }
});

await test('rules mentions reply once, empty descriptions are explicit, and repeated updates are deduplicated',async()=>{
 const f=fixture({rules:''});const update=message(groupA,2001);update.message.text='/rules@EXAMPLE_BOT';
 await f.consume(update);await f.consume(update);
 const sends=f.telegram.filter(c=>c.method==='sendMessage');assert.equal(sends.length,1);
 assert.match(sends[0].args.text,/尚未设置群规/);
 assert.equal(f.telegram.some(c=>c.method==='getChatMember'),false);
});

await test('group rules cannot retarget another group or hide extra text from moderation',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 const update=message(groupA,2002);update.message.text=`/rules ${groupB} extra text`;
 await f.consume(update);
 assert.match(f.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/无需参数/);
 assert.ok(f.telegram.every(c=>c.args.chat_id!==groupB));assert.equal(f.aiCalls.length,1);
});

await test('private rules keep exact-group authorization and provide rules-specific usage',async()=>{
 const missing=fixture();await missing.consume(command('/rules'));
 assert.match(missing.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/私聊用法.*\/rules/);
 const denied=fixture();await denied.consume(command(`/rules ${groupB}`));
 assert.equal(denied.telegram.some(c=>c.method==='getChat'),false);
 assert.match(denied.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/管理员/);
 const allowed=fixture({managedStatus:'disabled'});await allowed.consume(command(`/rules ${groupA}`));
 assert.match(allowed.telegram.find(c=>c.method==='sendMessage')?.args.text||'',/Do not post advertisements/);
 assert.equal(allowed.statements.some(s=>s.sql.includes('managed_chats')),false);
});


await test('command responses consistently use titled MarkdownV2, localized labels, and status tones',async()=>{
 const cases=[
  ['/help', /ℹ️ \*命令帮助\*\n\n/],
  ['/rules', /ℹ️ \*群组规则\*\n\n/],
  ['/threshold', /ℹ️ \*删除阈值\*\n\n/],
  ['/threshold 75%', /✅ \*删除阈值\*\n\n/],
  ['/dry_run', /ℹ️ \*群组设置\*\n\n/],
  ['/logs off', /✅ \*群组设置\*\n\n/],
  ['/cancel 22', /ℹ️ \*封禁复核取消\*\n\n/],
  ['/pending 22', /ℹ️ \*封禁复核状态\*\n\n/],
  ['/scan_all', /✅ \*历史扫描\*\n\n/],
  ['/scan_status', /ℹ️ \*历史扫描进度\*\n\n/],
  ['/scan_stop', /✅ \*历史扫描\*\n\n/],
  ['/warnings 22', /ℹ️ \*用户警告次数\*\n\n/],
  ['/warnings 22 2', /✅ \*用户警告次数\*\n\n/],
  ['/dry_run maybe', /⚠️ \*参数错误\*\n\n/],
  ['/unknown', /⚠️ \*未知命令\*\n\n/],
 ];
 for(const [commandLine,pattern] of cases){
  const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  await f.consume(command(commandLine,{id:groupA,type:'supergroup'}));
  const responses=f.telegram.filter(x=>x.method==='sendMessage');
  assert.ok(responses.length>0,commandLine);
  assert.match(responses[0].args.text,pattern,commandLine);
  assert.equal(responses[0].args.parse_mode,'MarkdownV2');
  assert.equal(responses[0].args.link_preview_options?.is_disabled,true);
 }
});
await test('rule descriptions escape Markdown while action and digest details use matching labels',async()=>{
 const rules='Text *bad* [link](https://example.org) _under_ !';
 const f=fixture({rules});f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 await f.consume(command('/rules',{id:groupA,type:'supergroup'}));
 const reply=f.telegram.find(x=>x.method==='sendMessage')?.args.text||'';
 assert.match(reply,/ℹ️ \*群组规则\*/);
 assert.ok(reply.includes('Text \\*bad\\* \\[link\\]\\(https://example\\.org\\) \\_under\\_ \\!'));
 assert.ok(reply.includes('来源：群组简介'));
 const flagged=fixture({remove:true,score:0.98,strikeCount:2});
 flagged.settings.set(groupA,{dry_run:0,logs_enabled:1,remove_threshold:0.9});
 await flagged.consume(message(groupA,3030));
 const outputs=flagged.telegram.filter(x=>x.method==='sendMessage').map(x=>x.args.text);
 const groupNotice=flagged.telegram.find(x=>x.method==='sendMessage'&&x.args.chat_id===groupA)?.args.text||'';
 assert.ok(groupNotice.startsWith('🚨 *违规消息已删除*\n\n'));
 assert.ok(groupNotice.includes('👤 用户：'));
 assert.ok(groupNotice.includes('📋 *处理说明*'));
 assert.ok(groupNotice.includes('违规类别：'));
 assert.ok(groupNotice.includes('📊 近 7 天警告：'));
 assert.ok(!groupNotice.includes('/3030)'), 'deleted message link is not presented');
 assert.ok(outputs.every(x=>x.length<4096),'Telegram message size stays within limit');
});


await test('single /scan enforces by default only in a group with enforcement enabled',async()=>{
  const enforcing=fixture({remove:true,score:0.99,strikeCount:1});
  enforcing.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const commandUpdate=command('/scan',{id:groupA,type:'supergroup'});
  commandUpdate.message.reply_to_message=message(groupA,2201).message;
  await enforcing.consume(commandUpdate);
  assert.equal(enforcing.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id===2201).length,1);
  assert.ok(enforcing.statements.some(x=>x.sql.includes('INSERT INTO violations')));
  assert.equal(enforcing.reviews.get(`${groupA}:2201`)?.last_status,'deleted');
  assert.match(enforcing.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/已删除/);

  const dry=fixture({remove:true,score:0.99});
  dry.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const dryUpdate=command('/scan',{id:groupA,type:'supergroup'});
  dryUpdate.message.reply_to_message=message(groupA,2202).message;
  await dry.consume(dryUpdate);
  assert.equal(dry.telegram.some(x=>x.method==='deleteMessage'&&x.args.message_id===2202),false);
  assert.equal(dry.reviews.get(`${groupA}:2202`)?.last_status,'dry_run_flag');
  assert.match(dry.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/dry\\-run.*未执行处罚/);
});

await test('single /scan audit explicitly remains non-punitive even when enforcement is enabled',async()=>{
  for(const dry_run of [0,1]){
    const f=fixture({remove:true,score:0.99});
    f.settings.set(groupA,{dry_run,logs_enabled:0});
    const update=command('/scan audit',{id:groupA,type:'supergroup'});
    update.message.reply_to_message=message(groupA,2203).message;
    await f.consume(update);
    assert.equal(f.telegram.some(x=>['restrictChatMember','banChatMember'].includes(x.method) || (x.method==='deleteMessage'&&x.args.message_id===2203)),false);
    assert.equal(f.reviews.get(`${groupA}:2203`)?.last_status,'audit_flag');
    assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/仅审计，未执行处罚/);
  }
});

await test('single scan rejects invalid flags and nonpositive message IDs before D1 archive access',async()=>{
  for(const text of ['/scan nonsense','/scan 0','/scan -100200','/scan 2201 audit enforce','/scan audit 2201']){
    const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
    await f.consume(command(text,{id:groupA,type:'supergroup'}));
    assert.equal(f.aiCalls.length,0,text);
    assert.ok(!f.statements.some(x=>x.sql.includes('SELECT payload FROM archived_messages')),text);
    assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>x.args.message_id),[42],text);
    assert.match(f.telegram.find(x=>x.method==='sendMessage')?.args.text||'',/参数错误/,text);
  }
});

await test('authorized group admin can set and query one member warning count without touching original strikes',async()=>{
  const f=fixture({strikeCount:2});
  f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const set=command('/warnings 22 0',{id:groupA,type:'supergroup'});set.update_id=2600;
  await f.consume(set);
  assert.equal(f.warningAdjustments.get(`${groupA}:22`)?.count,0);
  const writes=f.statements.filter(x=>x.sql.includes('INSERT INTO warning_edit_receipts'));
  assert.equal(writes.length,1);
  assert.deepEqual(writes[0].values.slice(1,4),[groupA,22,0]);
  assert.equal(f.telegram.some(x=>['restrictChatMember','banChatMember'].includes(x.method)),false);
  assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>x.args.message_id),[42]);
  assert.match(f.telegram.find(x=>x.method==='sendMessage')?.args.text||'',/0\/3/);
  const query=command('/warnings 22',{id:groupA,type:'supergroup'});query.update_id=2601;
  await f.consume(query);
  assert.equal(f.statements.filter(x=>x.sql.includes('INSERT INTO warning_edit_receipts')).length,1);
  assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/仅查询.*未修改警告次数/);
  assert.equal(f.warningAdjustments.has(`${groupB}:22`),false);
});

await test('manual warning adjustments count only subsequent violations without duplicate punishment',async()=>{
  const f=fixture({remove:true,score:0.99,strikeCount:2});
  f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const reset=command('/warnings 22 0',{id:groupA,type:'supergroup'});reset.update_id=2700;
  await f.consume(reset);
  await f.consume(message(groupA,2701));
  assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id!==42)
    .map(x=>x.args.message_id),[2701]);
  assert.equal(f.telegram.some(x=>x.method==='restrictChatMember'),false);
  assert.match(f.telegram.find(x=>x.method==='sendMessage'&&x.args.chat_id===groupA&&x.args.text.includes('违规消息已删除'))?.args.text||'',/1\/3/);
  const set=command('/warnings 22 3',{id:groupA,type:'supergroup'});set.update_id=2702;
  await f.consume(set);
  assert.equal(f.telegram.filter(x=>x.method==='restrictChatMember').length,0,'setting 3 is not itself a penalty');
  await f.consume(message(groupA,2703));
  assert.equal(f.telegram.filter(x=>x.method==='restrictChatMember').length,1,'next actionable violation can escalate');
  assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id!==42)
    .map(x=>x.args.message_id),[2701,2703]);
});

await test('warning management is isolated by exact target group and rejects unauthorized or anonymous actors',async()=>{
  const privateDenied=fixture();
  await privateDenied.consume(command(`/warnings ${groupB} 22 3`));
  assert.deepEqual(privateDenied.telegram.filter(x=>x.method==='getChatMember').map(x=>x.args),
    [{chat_id:groupB,user_id:11}]);
  assert.equal(privateDenied.statements.some(x=>x.sql.includes('warning_adjustments')),false);

  for(const text of [`/warnings ${groupB} 2`,'/warnings 22 2 extra','/warnings -22 2']){
    const f=fixture();
    await f.consume(command(text,{id:groupA,type:'supergroup'}));
    assert.equal(f.warningAdjustments.size,0,text);
    assert.ok(!f.statements.some(x=>x.sql.includes('warning_adjustments')),text);
    assert.ok(!f.telegram.some(x=>x.method==='getChatMember'&&x.args.chat_id===groupB),text);
  }
  for(const anonymous of [true,false]) {
    const f=fixture();
    const update=message(groupA,2800+(anonymous?1:0));update.message.text='/warnings 22 3';
    if(anonymous)update.message.sender_chat={id:groupA};
    await f.consume(update);
    assert.equal(f.warningAdjustments.size,0);
    assert.equal(f.aiCalls.length,1,'denied group commands remain ordinary moderated messages');
  }
  const allowed=fixture();
  const okay=command(`/warnings ${groupA} 22 2`);okay.update_id=2805;
  await allowed.consume(okay);
  assert.equal(allowed.warningAdjustments.get(`${groupA}:22`)?.count,2);
  assert.equal(allowed.warningAdjustments.has(`${groupB}:22`),false);
});

await test('warning count rejects invalid user IDs and out-of-range counts without D1 writes',async()=>{
 for(const text of ['/warnings','/warnings 22 -1','/warnings 22 4','/warnings 22 99','/warnings 22 nan','/warnings 0 2','/warnings -100200 2']){
  const f=fixture();
  await f.consume(command(text,{id:groupA,type:'supergroup'}));
  assert.equal(f.warningAdjustments.size,0,text);
  assert.ok(!f.statements.some(x=>x.sql.includes('INSERT INTO warning_edit_receipts')),text);
  assert.match(f.telegram.find(x=>x.method==='sendMessage')?.args.text||'',/参数错误/,text);
 }
});
await test('replayed warning update cannot erase a later strike in a new command',async()=>{
  const f=fixture({remove:true,score:0.99});
  f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const reset=command('/warnings 22 0',{id:groupA,type:'supergroup'});reset.update_id=3400;
  await f.consume(reset);
  await f.consume(message(groupA,3401));
  const again={...reset};
  // Direct fixture processing sees processed update IDs as already complete;
  // the durable receipt separately protects the partial-failure retry path.
  f.completed.delete(3400);
  await f.consume(again);
  assert.equal(f.warningReceiptIds.size,1);
  assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/1\/3/);
});



await test('cancel and warning updates never call Telegram unmute or restore permissions', async()=>{
  const cancel=fixture();
  cancel.settings.set(groupA,{dry_run:1,logs_enabled:0});
  await cancel.consume(command('/cancel 22',{id:groupA,type:'supergroup'}));
  assert.equal(cancel.telegram.some(x=>x.method==='restrictChatMember'),false);
  assert.equal(cancel.telegram.some(x=>x.method==='getChat' && x.args.chat_id===groupA),false);
  const warning=fixture();
  warning.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const cmd=command('/warnings 22 0',{id:groupA,type:'supergroup'});cmd.update_id=3901;
  await warning.consume(cmd);
  assert.equal(warning.telegram.some(x=>x.method==='restrictChatMember'),false);
  assert.equal(warning.telegram.some(x=>x.method==='banChatMember'),false);
  assert.match(warning.telegram.find(x=>x.method==='sendMessage')?.args.text||'',/不会自动解除/);
});

await test('missing warning table blocks deletion before Telegram or strike writes', async()=>{
 const f=fixture({remove:true,missingWarningTable:true});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(message(groupA,3100),{retry:true});
 assert.equal(f.telegram.some(call=>call.method==='deleteMessage'),false);
 assert.equal(f.statements.some(s=>s.sql.includes('INSERT INTO violations')),false);
 assert.equal(f.completed.has(3100),false);
 assert.equal(f.archives.has(String(groupA)+':3100'),true,'archival can precede the failed preflight');
 assert.ok(f.statements.some(s=>s.operation==='read' && s.sql.includes('SELECT COALESCE((SELECT base_count FROM warning_adjustments')), 'pre-deletion warning-count D1 query must be attempted');
 assert.equal(f.reviews.get(`${groupA}:3100`)?.last_status,'pending','failed preflight must not report a finished moderation action');
});

await test('missing warning table fails administrator updates without canceling punishments', async()=>{
 const f=fixture({missingWarningTable:true});
 const cmd=command('/warnings 22 2',{id:groupA,type:'supergroup'});
 cmd.update_id=3101;
 await f.consume(cmd,{retry:true});
 assert.equal(f.warningAdjustments.size,0);
 assert.equal(f.statements.some(s=>s.sql.includes('UPDATE pending_bans')),false);
 assert.equal(f.completed.has(3101),false);
});

await test('post-delete count failure is a partial result, and retry avoids duplicate deletion', async()=>{
 const f=fixture({remove:true,warningReadFailsAfterDelete:true});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 const update=message(groupA,3102);
 await f.consume(update,{retry:true});
 assert.equal(f.telegram.filter(call=>call.method==='deleteMessage').length,1);
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO violations')).length,1);
 assert.equal(f.completed.has(3102),false);
 assert.equal(f.reviews.get(String(groupA)+':3102')?.last_status,'pending');
 await f.consume(update);
 assert.equal(f.telegram.filter(call=>call.method==='deleteMessage').length,1);
 assert.equal(f.statements.filter(s=>s.sql.includes('INSERT INTO violations')).length,1);
 assert.equal(f.reviews.get(String(groupA)+':3102')?.last_status,'previously_deleted');
 assert.equal(f.telegram.some(call=>call.method==='restrictChatMember'),false);
});

await test('bulk scan modes and rejected flags',async()=>{
 for(const [line,enforce] of [['/scan_all',1],['/scan_all enforce',1],['/scan_all audit',0]]){
  const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  f.archives.set(groupA+':3001',message(groupA,3001).message);
  await f.consume(command(line,{id:groupA,type:'supergroup'}));
  assert.equal(f.jobs.get(groupA)?.enforce,enforce,line);
  assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',enforce?/执行模式/:/仅审计/,line);
 }
 for(const line of ['/scan_all invalid','/scan_all audit enforce','/scan_all enforce audit']){
  const f=fixture();await f.consume(command(line,{id:groupA,type:'supergroup'}));
  assert.equal(f.jobs.size,0,line);
  assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/参数错误/,line);
 }
});

await test('bulk scan respects group dry-run',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 await f.consume(command('/scan_all',{id:groupA,type:'supergroup'}));
 assert.equal(f.jobs.get(groupA)?.enforce,1);
 assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/模拟模式.*不执行处罚/);
 assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>x.args.message_id),[42]);
});

await test('duplicate bulk scan does not override active job and stop permits restart',async()=>{
 const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 f.archives.set(groupA+':3002',message(groupA,3002).message);
 await f.consume(command('/scan_all audit',{id:groupA,type:'supergroup'}));
 const job=f.jobs.get(groupA);job.cursor_message_id=100;job.scanned=3;
 const second=command('/scan_all enforce',{id:groupA,type:'supergroup'});second.update_id=3003;
 await f.consume(second);
 assert.strictEqual(f.jobs.get(groupA),job);
 assert.equal(job.cursor_message_id,100);assert.equal(job.scanned,3);assert.equal(job.enforce,0);
 assert.match(f.telegram.filter(x=>x.method==='sendMessage').at(-1)?.args.text||'',/扫描进行中/);
 const stop=command('/scan_stop',{id:groupA,type:'supergroup'});stop.update_id=3004;await f.consume(stop);
 assert.equal(job.status,'stopped');
 const restart=command('/scan_all enforce',{id:groupA,type:'supergroup'});restart.update_id=3005;await f.consume(restart);
 assert.equal(f.jobs.get(groupA).enforce,1);assert.equal(f.jobs.get(groupA).cursor_message_id,0);
});

await test('bulk scan cannot operate on unauthorized target group',async()=>{
 const denied=fixture();await denied.consume(command('/scan_all '+groupB+' enforce'));
 assert.equal(denied.jobs.size,0);
 const f=fixture();f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 await f.consume(command('/scan_all audit',{id:groupA,type:'supergroup'}));
 assert.equal(f.jobs.get(groupA).enforce,0);assert.equal(f.jobs.has(groupB),false);
 assert.ok(f.statements.filter(x=>x.sql.includes('INSERT INTO scan_jobs')).every(x=>x.values[0]===groupA));
});

await test('old enforcement result after stop and audit restart cannot alter new scan',async()=>{
 let unblock,entered;
 const begun=new Promise(resolve=>entered=resolve);
 const held=new Promise(resolve=>unblock=resolve);
 const f=fixture({remove:true,score:.99,pauseAi:async()=>{entered();await held}});
 f.settings.set(groupA,{dry_run:0,logs_enabled:1});
 f.archives.set(groupA+':17',message(groupA,17).message);
 const start=command('/scan_all enforce',{id:groupA,type:'supergroup'});start.update_id=9100;
 await f.consume(start);
 const old=f.tick();await begun;
 const stop=command('/scan_stop',{id:groupA,type:'supergroup'});stop.update_id=9101;
 await f.consume(stop);
 const restart=command('/scan_all audit',{id:groupA,type:'supergroup'});restart.update_id=9102;
 await f.consume(restart);
 assert.equal(f.jobs.get(groupA).enforce,0);
 assert.equal(f.jobs.get(groupA).cursor_message_id,0);
 const newer=f.jobs.get(groupA).generation;
 unblock();await old;
 assert.equal(f.jobs.get(groupA).generation,newer);
 assert.equal(f.jobs.get(groupA).cursor_message_id,0);
 assert.equal(f.jobs.get(groupA).scanned,0);
 assert.equal(f.jobs.get(groupA).flagged,0);
 assert.equal(f.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id===17).length,0);
 assert.equal(f.reviews.get(groupA+':17')?.last_status,undefined);
 const second=f.tick();await second;
 assert.equal(f.jobs.get(groupA).enforce,0);
 assert.equal(f.jobs.get(groupA).scanned,1);
 assert.equal(f.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id===17).length,0);
 await f.tick();
 assert.equal(f.jobs.get(groupA).status,'done');
 assert.equal(f.telegram.filter(x=>x.method==='sendMessage'&&x.args.text.includes('历史审查完成')).length,1);
});
await test('concurrent Cron claims cannot process the same group twice',async()=>{
 let unblock,entered;
 const held=new Promise(resolve=>unblock=resolve),begun=new Promise(resolve=>entered=resolve);
 const f=fixture({pauseAi:async()=>{entered();await held}});
 f.settings.set(groupA,{dry_run:1,logs_enabled:0});
 f.archives.set(groupA+':17',message(groupA,17).message);
 let one=command('/scan_all audit',{id:groupA,type:'supergroup'});one.update_id=9200;
 await f.consume(one);
 const first=f.tick();await begun;
 const second=f.tick();
 await second;
 assert.equal(f.aiCalls.length,1,'another Cron cannot claim an in-progress scan');
 unblock();await first;
 assert.equal(f.jobs.get(groupA).scanned,1);
 assert.equal(f.jobs.get(groupA).flagged,0);
 // Other groups can be started and scanned independently after this one.
 f.archives.set(groupB+':27',message(groupB,27).message);
 // Seed the second group's own database job directly; authorization of
 // group-B commands is covered by separate group permission tests.
 f.jobs.set(groupB,{chat_id:groupB,cursor_message_id:0,max_message_id:27,
   scanned:0,flagged:0,enforce:0,status:'running',generation:1,lease_id:0,claimed:false});
 await f.tick();
 assert.equal(f.jobs.get(groupB)?.scanned,1);
 assert.equal(f.jobs.get(groupA).scanned,1);
});
await test('Telegram deletion already in flight is not represented as atomically cancellable',async()=>{
 let unblock,entered;
 const held=new Promise(resolve=>unblock=resolve),begun=new Promise(resolve=>entered=resolve);
 const f=fixture({remove:true,score:.99,strikeCount:2,pauseDeleteTarget:17,pauseDelete:async()=>{entered();await held}});
 f.settings.set(groupA,{dry_run:0,logs_enabled:0});
 f.archives.set(groupA+':17',message(groupA,17).message);
 let start=command('/scan_all enforce',{id:groupA,type:'supergroup'});start.update_id=9300;await f.consume(start);
 let old=f.tick();await begun;
 let stop=command('/scan_stop',{id:groupA,type:'supergroup'});stop.update_id=9301;await f.consume(stop);
 let restart=command('/scan_all audit',{id:groupA,type:'supergroup'});restart.update_id=9302;await f.consume(restart);
 unblock();await old;
 assert.equal(f.jobs.get(groupA).cursor_message_id,0);
 assert.equal(f.jobs.get(groupA).enforce,0);
 assert.equal(f.telegram.filter(x=>x.method==='deleteMessage'&&x.args.message_id===17).length,1);
 assert.equal(f.telegram.some(x=>['restrictChatMember','banChatMember'].includes(x.method)),false);
 assert.equal(f.reviews.get(groupA+':17')?.last_status,'deleted');
});


await test('non-media Telegram message types are archived and routed to flash with typed context', async()=>{
  for(const [kind,value] of [
    ['poll',{question:'Do you want ads?',options:[{text:'Yes'},{text:'No'}]}],
    ['contact',{first_name:'Alice',last_name:'Example',phone_number:'+1234567890'}],
    ['location',{latitude:1.234,longitude:2.345}],
    ['dice',{emoji:'🎲',value:5}],
    ['invoice',{title:'Invoice',description:'Donation',currency:'USD',payload:'private-reference'}],
  ]){
    const f=fixture();
    const u=message(groupA,4600);
    delete u.message.text;
    u.message[kind]=value;
    await f.consume(u);
    assert.equal(f.aiCalls.length,1,kind);
    assert.equal(f.aiCalls[0].model,'@cf/cloudflare/clef-flash',kind);
    assert.ok(f.aiCalls[0].input.state.structured_message[kind],kind);
    const sent=JSON.stringify(f.aiCalls[0].input.state.structured_message);
    assert.ok(!sent.includes('+1234567890'),'phone number must not be forwarded to AI');
    assert.ok(!sent.includes('1.234'),'precise location must not be forwarded to AI');
    assert.ok(!sent.includes('private-reference'),'invoice payload must not be forwarded to AI');
    assert.equal(f.archives.get(groupA+':4600')[kind]!=null,true);
  }
});

await test('administrator flagged messages cannot bypass Bot API 48 hour deletion window',async()=>{
  const f=fixture({remove:true});
  f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const update=message(groupA,4601);
  update.message.from.id=11;
  update.message.date=Math.floor(Date.now()/1000)-49*3600;
  await f.consume(update);
  assert.equal(f.aiCalls.length,1);
  assert.equal(f.reviews.get(groupA+':4601')?.last_status,'too_old_to_delete');
  assert.equal(f.telegram.some(call=>call.method==='deleteMessage'),false);
  assert.equal(f.telegram.some(call=>call.method==='banChatMember'),false);
});


await test('group bot commands are removed after their responses while private commands are retained',async()=>{
  for(const cmd of ['/help','/rules','/logs warn','/threshold','/scan_status','/unknown','/logs bogus']){
    const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
    const update=command(cmd,{id:groupA,type:'supergroup'});
    update.message.message_thread_id=19;
    await f.consume(update);
    const sends=f.telegram.filter(x=>x.method==='sendMessage');
    assert.ok(sends.length>=1,cmd);
    assert.ok(sends.every(x=>x.args.chat_id===groupA&&x.args.message_thread_id===19),cmd);
    assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>[x.args.chat_id,x.args.message_id]),[[groupA,42]],cmd);
    assert.ok(f.telegram.findIndex(x=>x.method==='deleteMessage')>f.telegram.findIndex(x=>x.method==='sendMessage'),cmd);
    assert.equal(f.completed.has(789),true,cmd);
  }
  const privateCmd=fixture();
  await privateCmd.consume(command('/help'));
  assert.equal(privateCmd.telegram.some(x=>x.method==='deleteMessage'),false);
});

await test('unauthorized group commands are moderated before cleanup and cannot mutate group settings',async()=>{
  for(const anonymous of [false,true]){
    const f=fixture();f.settings.set(groupA,{dry_run:1,logs_enabled:0});
    const update=command('/logs off',{id:groupA,type:'supergroup'});
    update.message.from.id=22;
    if(anonymous)update.message.sender_chat={id:groupA};
    await f.consume(update);
    assert.equal(f.settings.get(groupA)?.logs_enabled,0);
    assert.equal(f.statements.some(x=>x.sql.includes('INSERT INTO group_settings')),false);
    assert.equal(f.aiCalls.length,1);
    assert.equal(f.reviews.get(groupA+':42')?.last_status,'allow');
    assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>x.args.message_id),[42]);
  }
});

await test('punitive removal of a slash-message does not issue a duplicate housekeeping delete',async()=>{
  const f=fixture({remove:true,score:1});
  f.settings.set(groupA,{dry_run:0,logs_enabled:0});
  const update=message(groupA,6102);
  update.message.text='/help forbidden-advertisement';
  await f.consume(update);
  assert.equal(f.reviews.get(groupA+':6102')?.last_status,'deleted');
  assert.deepEqual(f.telegram.filter(x=>x.method==='deleteMessage').map(x=>x.args.message_id),[6102]);
});

await test('failed command cleanup is nonfatal, nonretryable and not applied to old messages',async()=>{
  const f=fixture({deleteError:true});
  f.settings.set(groupA,{dry_run:1,logs_enabled:0});
  const update=command('/dry_run off',{id:groupA,type:'supergroup'});
  await f.consume(update);await f.consume(update);
  assert.equal(f.settings.get(groupA)?.dry_run,0);
  assert.equal(f.completed.has(update.update_id),true);
  assert.equal(f.statements.filter(x=>x.sql.includes('INSERT INTO group_settings')).length,1);
  assert.equal(f.telegram.filter(x=>x.method==='deleteMessage').length,1);
  assert.ok(f.errors.some(x=>x.includes('command_message_cleanup_failed')));

  const old=fixture();
  const stale=command('/help',{id:groupA,type:'supergroup'});
  stale.message.date=Math.floor(Date.now()/1000)-49*3600;
  await old.consume(stale);
  assert.equal(old.telegram.some(x=>x.method==='deleteMessage'),false);
});

await test('transient command failures keep their source messages for successful retries',async()=>{
  const f=fixture({policyFail:true});
  const update=command('/rules',{id:groupA,type:'supergroup'});
  await f.consume(update,{retry:true});
  assert.equal(f.completed.has(update.update_id),false);
  assert.equal(f.telegram.some(x=>x.method==='deleteMessage'),false);
});


await test('hidden credentialed text links never add credentials to model input or fetches', async () => {
  for (const caption of [false, true]) {
    const f = fixture();
    const update = message(groupA, 4100);
    const target = 'https://hidden-user:Hidden%40Password@example.org/private?token=hidden-query#hidden-fragment';
    update.message.text = 'Read this';
    const entities = [{type:'text_link',offset:0,length:4,url:target}];
    if (caption) {
      delete update.message.text;
      update.message.caption = 'Read this';
      update.message.caption_entities = entities;
    } else update.message.entities = entities;
    await f.consume(update);
    assert.equal(f.aiCalls.length, 1);
    assert.equal(f.pageRequests.length, 0);
    const input = f.aiCalls[0].input;
    assert.equal(input.state.link_evidence[0].status, 'blocked_target');
    for (const secret of ['hidden-user', 'Hidden%40Password', 'hidden-query', 'hidden-fragment'])
      assert.ok(!JSON.stringify(input).includes(secret), secret);
    assert.equal(input.state.link_evidence[0].url, undefined);
  }
});

await test('URL evidence strips userinfo defensively and redirects never disclose or fetch rejected targets', async () => {
  for (const location of [
    'https://redirect-user:RedirectPassword@example.org/secret?redirect-token=private#fragment',
    'http://127.0.0.1/private', 'https://private.internal/path',
  ]) {
    const f = fixture({pageFetch: () => new Response(null, {status:302,headers:{Location:location}})});
    const update = message(groupA, 4101);
    update.message.text = 'Read this';
    update.message.entities = [{type:'text_link',offset:0,length:4,url:'https://example.org/start?original-token=private#fragment'}];
    await f.consume(update);
    assert.equal(f.pageRequests.length,1);
    assert.equal(f.pageRequests[0].redirect,'manual');
    const input = f.aiCalls[0].input;
    assert.deepEqual(input.state.link_evidence,[{url:'https://example.org/start',status:'blocked_redirect'}]);
    for(const secret of ['redirect-user','RedirectPassword','redirect-token','original-token','fragment'])
      assert.ok(!JSON.stringify(input).includes(secret),secret);
  }
});

await test('safe redirects retain page review and sanitized final URLs within fetch limits', async () => {
  const f = fixture({pageFetch: request => new URL(request.url).pathname === '/start'
    ? new Response(null,{status:302,headers:{Location:'/final?final-token=private#fragment'}})
    : new Response('<p>Useful page evidence</p>',{headers:{'Content-Type':'text/html'}})});
  const update = message(groupA,4102);
  update.message.text = 'Read these';
  update.message.entities = ['start','other','ignored'].map(p=>({type:'text_link',offset:0,length:4,url:`https://example.org/${p}?query-token=private#fragment`}));
  await f.consume(update);
  assert.equal(f.pageRequests.length,3);
  assert.ok(f.pageRequests.every(r=>r.redirect==='manual'));
  assert.equal(f.aiCalls[0].input.state.link_evidence.length,2);
  assert.deepEqual(f.aiCalls[0].input.state.link_evidence[0],{url:'https://example.org/start',status:'fetched',final_url:'https://example.org/final',http_status:200,mime:'text/html',excerpt:'Useful page evidence'});
  assert.ok(!JSON.stringify(f.aiCalls[0].input).includes('token='));
  const capped = fixture({pageFetch:()=>new Response(null,{status:302,headers:{Location:'/again'}})});
  await capped.consume(update);
  assert.equal(capped.pageRequests.length,6,'two links, three hops each');
  assert.ok(capped.aiCalls[0].input.state.link_evidence.every(x=>x.status==='redirect_limit'));
});

await test('explicit text and caption URLs remain original moderation content, blocked evidence adds no URL', async () => {
  for(const field of ['text','caption']) {
    const f=fixture();const update=message(groupA,4103);
    delete update.message.text;
    const original='Review https://visible-user:VisiblePassword@example.org/path?visible-token=private#visible-fragment';
    update.message[field]=original;
    await f.consume(update);
    assert.equal(f.pageRequests.length,0);
    assert.equal(f.aiCalls[0].input.state[field==='text'?'message_text':'caption'],original);
    assert.deepEqual(f.aiCalls[0].input.state.link_evidence,[{status:'blocked_target'}]);
  }
});
