# Telegram Clef-omni Moderator

A **Rust/WebAssembly Cloudflare Worker** that discovers Telegram groups where it is an administrator, uses each group's `getChat.description` as its moderation policy, reviews text-only messages with `@cf/cloudflare/clef-flash` and media with `@cf/cloudflare/clef-omni`, archives Bot API updates in D1, and can delete messages, warn users, mute them for 60 seconds, and ban them after the administrator cancellation window.

**This bot is not silent.** Private review logs are enabled by default for each managed group, including when moderation is in dry-run mode. Successful enforcement also triggers public notices in the affected group. Commands reply in the chat and forum topic where they were issued. These are separate notification paths.

- **Group discovery:** `my_chat_member` updates and a first-message fallback. The Bot API cannot list every group the bot previously joined.
- **Safe enforcement default:** `DRY_RUN=true` archives and classifies messages without deleting or banning; per-group `/dry-run off` enables enforcement.
- **Per-group isolation:** each group has independent administrator authorization, dry-run, private-log and removal-threshold settings. Clef-omni's binary `violation.noul` score threshold defaults to 93%. Cloudflare Queues batches up to 20 events or 10 seconds (whichever comes first); each message is still classified and punished independently.
- **History:** a Telegram Desktop JSON importer handles text-only history. The Bot API cannot retrieve unobserved old messages.

**Model routing:** Pure text uses Clef-flash in live moderation, single-message scans and bulk history scans. Messages containing supported images, audio or video use Clef-omni, including their captions. Unsupported, malformed or oversized media remains unavailable for automatic enforcement; it is never silently reviewed as caption-only text. Both routes use the same typed questions, parsing, thresholds and group safeguards.

**Chinese instructions:** [README_CN.md](README_CN.md).

## Notifications

### Private administrator review logs

- **Private administrator logs are on by default.** For a group without saved settings, `src/store.rs` defaults `group_logs` to `true`; the D1 `group_settings.logs_enabled` column defaults to `1`.
- The model only outputs a typed `violation.noul` probability, category and severity. **The bot's Rust code derives `allow`, `needs_review` and deletion**; Clef is never asked to output an allow/review/remove action. Routine reviewed-message statuses are combined into short per-group summaries within each Queue batch, with clickable links to still-existing supergroup messages. Enforcement, cancellations and historical scan completions remain separate best-effort notifications. Worker console errors are not guaranteed by DM.
- On each attempted batch delivery, `getChatAdministrators` fetches that **specific group's current human administrators**, excluding bot accounts. There is no fixed global recipient. Each intended recipient must first open a private chat with the bot (for example, send `/start`), must not have blocked it, and must be reachable through Telegram; otherwise the DM fails. Admin lookup failures also prevent delivery. Recipients are refreshed for later notifications.
- `/logs off` stops unsolicited **private review logs for that group only**; `/logs on` re-enables them. It **does not disable group action notices**, command replies, D1 archiving, Worker console logs, or replies to commands issued in a private chat. Turning logs off for one group does not affect another group.

### Consistent bot message format

All bot-generated Telegram messages use **MarkdownV2** and consistent headings: ℹ️ information, ✅ successful changes, ⚠️ invalid arguments or permissions, ❌ transient command failures, 🚨 enforcement, and 📋 per-group review summaries. Command replies show concise Chinese labels (group, setting, status, mode, message ID and next steps) instead of unprocessed `on/off`, `pending`, `enforce` or other internal values. Reviews and scan results use the same status terminology and score bars. Failure statuses never display a fabricated score. Untrusted rule text, user names, titles and error details are escaped; deleted messages are **never linked**. Command replies remain in their originating topic; administrator DMs keep group and owner context. No behavior, authorization, score threshold, retention or database schema changes are implied by formatting.

### Public messages in the affected group

- **After successful deletion**, the bot immediately attempts a MarkdownV2 notice with the deleted message ID, category, a clickable user display name (numeric ID fallback), the seven-day warning count as a three-step Markdown bar, and, where applicable, the 60-second mute, pending ban and `/cancel USER_ID` instructions. A failed mute is reported as such, not as a completed ban.
- **Only after Telegram confirms a ban** does the bot send a separate completed-ban announcement. Notifications use escaped Telegram MarkdownV2 and generated numeric-ID links; links to deleted messages are deliberately omitted. Notices stay in the affected group and do not quote the deleted message's text.
- Dry-run and audit reviews never claim that a deletion or ban happened. Posting is **best effort**: if Telegram rejects a group notice, the already-completed moderation action is not rolled back or repeated to retry the notice.
- All supported commands, including `/start` and `/help`, reply in their originating chat and forum topic, even with `/logs off`. Unknown commands, invalid arguments, permission denials and disabled groups receive feedback. Commands addressed to another bot are ignored. Help, unknown and denied group commands still undergo normal moderation; `/rules` with extra text is also moderated.

## Telegram permissions and delivery requirements

1. **Receiving and moderating:** the bot must be an administrator in the target group to receive all relevant group updates (without disabling Privacy Mode). `can_delete_messages` is required for automatic deletion; without it, moderation is paused. The group must be registered or rediscovered through updates.
2. **Muting and banning:** a **supergroup** and `can_restrict_members` are required in addition to deletion permission. With deletion permission alone, the bot can delete but cannot automatically mute or ban. Punishment additionally requires enforcement to be enabled and the group description to contain applicable rules.
3. **Public notices:** the bot must still have access to the group **and be able to send messages there** (`sendMessage`). Delete/restrict permissions do not guarantee that public notices can be posted. If Telegram refuses delivery, the action can still have succeeded; the notice is not retried.
4. **Private logs:** `getChatAdministrators` must succeed for the target group, and each human administrator must have initiated a private chat with the bot and permit its messages. The bot cannot initiate a DM conversation with an administrator who has never contacted it. Delivery failures do not undo an archived review or completed punishment.
5. **Webhook:** configure the Bot API webhook with an independent `secret_token` and `allowed_updates=["message","edited_message","my_chat_member","chat_member"]`, so group discovery, message reviews and administrator cancellations receive the necessary updates.

## Administrator commands

Group `/rules` is available to all members, including anonymous senders, and replies with the current group description even when moderation is disabled. It takes no group argument and stays in the originating topic; an empty description receives an explicit notice. Private `/rules GROUP_ID` still requires administrator authorization in that exact group.

All other management commands check the issuing user's `getChatMember` administrator/creator status **in the exact target group** before accessing its data. Private commands require a **negative group ID**; group commands always target their originating group. Group settings apply only to that group.

| Purpose | Command in the group | Command in a private chat |
| --- | --- | --- |
| Removal threshold (query or set) | `/threshold [0.75 or 75%]` | `/threshold -100GROUP_ID [0.75 or 75%]` |
| Enforcement mode (query or set) | `/dry-run [on\|off]` (also `/dry_run`) | `/dry_run -100GROUP_ID [on\|off]` |
| Private review logs (query or set) | `/logs [on\|warn\|off]` | `/logs -100GROUP_ID [on\|warn\|off]` |
| Read group policy | `/rules` | `/rules -100GROUP_ID` |
| Cancel pending ban (does not unmute) | `/cancel USER_ID` | `/cancel -100GROUP_ID USER_ID` |
| Inspect pending/processing ban | `/pending USER_ID` | `/pending -100GROUP_ID USER_ID` |
| Read or set a user's seven-day warnings | `/warnings USER_ID [0-3]` | `/warnings -100GROUP_ID USER_ID [0-3]` |
| Re-review and enforce an archived or replied-to message by default | Reply with `/scan [audit\|enforce]` or use `/scan MESSAGE_ID [audit\|enforce]` | `/scan_id -100GROUP_ID MESSAGE_ID [audit\|enforce]` |
| Scan archived messages (enforcement by default) | `/scan_all [audit\|enforce]` | `/scan_all -100GROUP_ID [audit\|enforce]` |
| Scan progress / stop | `/scan_status` / `/scan_stop` | `/scan_status -100GROUP_ID` / `/scan_stop -100GROUP_ID` |

`/cancel` only cancels a `pending` ban in D1 and never restores Telegram permissions. The bot's 60-second restriction expires automatically unless a human changes it. A `processing` case may already have submitted a Telegram request and cannot be atomically recalled. Warning adjustments fence subsequent retries of the old violation.

Group bot-command invocation messages are **deleted on a best-effort basis after command execution and any required moderation**, independently of dry-run. This includes invalid commands and authorization-denied attempts, but not private-chat commands or slash commands addressed to another bot. Bot replies remain in the originating group/topic. Deletion requires Telegram `can_delete_messages` and remains subject to the Bot API's 48-hour limit; failures are logged but do not retry or undo a completed administrative command. If ordinary content attached to a slash command must be moderated, that review runs before housekeeping; content already deleted by enforcement is not deleted a second time.

Send `/start` or `/help` in a group or private chat to see command help. Omitting the mode queries the current logs setting; omitting `on`/`off` queries dry-run. Replies are independent of private-log settings; a failed reply does not replay the completed management action.

`/scan` and `/scan_id` accept a replied-to message in the same group or a message ID already archived for that group. **Single-message scans request enforcement by default**; append `audit` to inspect without punishment (`enforce` remains accepted). They report the stored status, category and violation score, and explicitly explain when a dry-run prevented deletion. Even an enforcement scan never bypasses group policy, a nonactionable category, configured threshold, a group's `/dry_run on` state, the 48-hour deletion window, Telegram permissions or duplicate-strike protections. **Bulk `/scan_all` also requests enforcement by default**; `audit` is the explicit non-punitive option, and `enforce` remains accepted. The group's dry-run setting still blocks punitive actions. Starting another `/scan_all` while the same group's scan is running returns a conflict message without resetting its mode or progress; use `/scan_status` or `/scan_stop` before restarting. Independent groups can scan separately. The Bot API cannot fetch arbitrary unarchived messages by ID. Missing policy or model failure is reported as unavailable, not a clean pass; transient errors retain Queue retries.

Use `/warnings USER_ID` to see that user's effective warning count **in the current group**, or `/warnings USER_ID 0` through `3` to set it. In private chat, include a negative group ID first, e.g. `/warnings -1001234567890 22 2`. The issuing user must be an administrator/creator of that **specific target group**; group commands cannot select a different group. The change never deletes prior violation evidence and never triggers punishment on its own. A new genuine violation after the adjustment adds to the set baseline; the manual baseline expires after seven days. Any still-pending automated ban for that group/user is canceled on adjustment, without overriding unrelated human restrictions.

Use `/threshold` to query or set the removal threshold (default `0.93`); only future reviews are affected. The model answers a typed `noul` yes/no violation question with a numeric probability (`answers.violation.noul`), rather than producing an `allow` decision. Rust computes `allow` below threshold, `needs_review` when the score meets threshold but the category is `none`, and deletion when the score meets threshold with an actionable category. Clef category `other` is normalized to `none` before decisions and storage, so it follows the same `allow`/`needs_review` threshold behavior as `none`. Category `adult` remains actionable: when the model finds a violation of the explicit group policy and its score meets the group threshold, it enters the normal deletion, warning and escalation flow; below threshold it is allowed. Adult-content labels are retained in reviews and displayed in scan results and digests. Dry-run, explicit group policy, Telegram permissions, deletion age and duplicate-punishment protections are unchanged. These model scores are not calibrated certainty estimates. `/dry-run on` cancels pending bans; `/logs off` does not affect punishment.

### Scan generation and cancellation semantics

Bulk scans use a monotonic `generation` and a per-Cron `lease_id`, checked in SQLite before progress changes, historical review status writes and completion. Stopping a scan invalidates its claims; restarting with `audit` does not inherit old enforcement results. A Cron claim expires after 20 minutes if the Worker disappears, and duplicate ticks cannot claim the same running job at once. The completion message includes its generation because Telegram delivery can be delayed after a subsequent restart.

**Deployment prerequisite:** `schema/current.sql` creates the new columns on empty databases. On existing databases, run the pre-migration check below and apply only the missing `scan_jobs` ALTER statements it prints. `schema/ensure_scan_fencing.sql` is not idempotent; do not repeat columns after a partial upgrade. Do not execute the bootstrap schema against existing databases. A build with scan fencing must not be deployed before its D1 columns exist; CI never migrates production D1.

Cancellation fences future D1 scan progress, completion and ordinary historical review writes. It cannot atomically recall a Telegram request that has already been issued (including a pending deletion or restriction). A deletion confirmed after stopping the scan is still recorded for audit to avoid duplicate punishment, while subsequent mute escalation and stale pending-ban insertion are blocked. Scan errors are retried through a future Cron claim only after the current claim releases/expires; unexpected Worker termination may leave the job temporarily paused until lease expiry.

## Configuration and deployment

- Set `BOT_TOKEN` and `WEBHOOK_SECRET` as **Cloudflare/Wrangler secrets**, not repository or YAML values. The webhook secret is distinct from the Telegram bot token.
- `DRY_RUN=true` is the deployment default, inherited by groups without a dry-run override. `ENABLE_BAN=true` and `IMMEDIATE_BAN=false` are separate enforcement settings.
- Daily D1 cleanup now uses a persisted, fenced UTC-day claim instead of a one-minute midnight window. Late Cron invocations or slow moderation work catch up on the next available tick, completed days are skipped, failures are retried after five minutes, and interrupted runs become eligible after a 20-minute lease. Existing databases require the additive `schema/ensure_cleanup_state.sql` migration and a passing read-only D1 release check **before** deploying this Worker; do not run the canonical bootstrap on live data. No production D1 or Worker deployment is performed by CI.
- `ARCHIVE_DAYS=0` means archived messages are **not automatically removed**. Set an appropriate positive retention period for your deployment. This setting only controls `archived_messages`, not other D1 tables or delivered Telegram logs. Use an explicit non-negative integer: a missing, malformed, negative or overflowing value now reports a cleanup error instead of silently becoming unlimited retention. Fixed-window D1 cleanup remains independent.
- `BOT_USERNAME` is used to recognize commands addressed to this bot in groups.
- There is **no active `ADMIN_DM_CHAT_ID` recipient configuration**. Leaving it unset **does not** disable administrator DMs. Setting it does not select or change the current per-group recipients. Use `/logs off` separately for any group whose private review logs should be disabled.
- The persistent per-group `logs_enabled`, `logs_warn_only` and `dry_run` switches and `remove_threshold` value live in D1; they are **not** controlled by a global DM chat ID. New databases use the **current full schema** (`schema/current.sql`), applied only to an empty database via `npx wrangler d1 execute DB --remote --file schema/current.sql`. **Never run that bootstrap file on existing D1 data.** To enable `/warnings` safely on an existing deployment, first apply the *additive, idempotent* `schema/ensure_warning_adjustments.sql` with the authorized Cloudflare D1 connector (or `npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql`); it creates `warning_adjustments`, `warning_edit_receipts` and the `apply_warning_edit_receipt` trigger without deleting violation history. The trigger applies future warning edits and pending-ban cancellation atomically. Also apply the missing scan-fencing ALTERs printed by the preflight. GitHub Actions builds/tests artifacts only; upload through Cloudflare MCP only after the complete production schema passes the normal check.

See `wrangler.jsonc`, `schema/current.sql` and `.github/workflows/build.yml` for current D1/Queues bindings, a fresh-database bootstrap, Rust fat-LTO CI and manual MCP deployment. Production must have the current schema, queues, AI binding, Telegram webhook and secrets configured before deploying; old schema layouts are not supported.

## Security and privacy

Do not commit production bot tokens, webhook secrets, Cloudflare credentials, private Telegram identifiers or real chat content. Keep credentials in Wrangler/Cloudflare Secrets; rotate exposed tokens. Only a verified administrator of a group may change that group's settings or read its managed data through commands.

**Default-on administrator DMs are a privacy-relevant behavior, even with `DRY_RUN=true`.** Telegram messages reviewed by the bot can be archived in D1 with message text, captions and media file IDs. Media bytes may be downloaded transiently for review. Routine admin logs disclose review metadata (including group and message IDs, the current group owner’s display name and numeric profile link, categories and scores); completed-action notices disclose enforcement details to the group. Public notices do not reproduce deleted message text. `/logs off` does not erase already sent messages, change D1 retention, stop platform logs or prevent public notices.

Group administrators should review notification and data-retention settings before adding the bot. If posting or DM delivery is denied by Telegram, notification attempts may fail even when moderation or archiving succeeds.


Passive review digests read final statuses with one D1 query per group and Queue batch (rather than per message); `/logs off` skips these reads. All digest chunks share one fresh administrator lookup. Each message still has its own inference, archive, acknowledgement and retry.

## Manual deployment through Cloudflare MCP

GitHub Actions runs builds and tests, then retains the Worker artifact with SHA-256 checksums. Neither pushes nor manual workflow dispatch deploy to Cloudflare. Actions does not use `CLOUDFLARE_API_TOKEN` or `CLOUDFLARE_ACCOUNT_ID` secrets.

To release, download the successful main-branch artifact and verify its checksums and source commit. Record the current Worker version and a D1 Time Travel recovery bookmark. Use the authenticated Cloudflare MCP connection to run the read-only metadata/PRAGMA batch printed by `cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --print-inspection-sql`, save its successful statement results as JSON, and validate with `--from-json response.json --pre-migration`. Apply only the indicated additive migrations and recheck without `--pre-migration` (exit 0 required), then upload the unchanged `index.js` and binary `index_bg.wasm` as Worker modules. Preserve existing bindings, secrets, variables, compatibility settings and observability. Check the active deployment and `/healthz` after upload. Database contents, Queue consumers, Cron triggers and Telegram webhook configuration remain in place.

Private administrator digests and action logs include the current group name, numeric ID and a clickable group link. Public groups use their username; private groups use an existing Telegram invite link when available, otherwise a surviving reviewed-message link accessible to group members. No new invitations are generated. If Telegram metadata is unavailable, logs fall back to the group ID and available message link. Group titles are Markdown-escaped, and recipients remain the exact group's current human administrators. Probability scores use ten-step Markdown bars with exact percentages. Missing policy or inference errors show no fabricated score; empty categories and repeated group IDs are omitted. Each log also identifies the current creator by their escaped display name and a numeric `tg://user?id=...` profile link, using the same administrator snapshot without extra API requests. Missing creator metadata is labelled unavailable; a missing name falls back to the numeric user ID. Digest chunks reserve space for both headings.

Warning counts use the existing per-group, per-user seven-day strike records and show the full count even above three; the bar saturates at the three-warning threshold. Archived messages preserve user display names for later scans. Anonymous senders do not receive an attributed warning count.

Within a Queue delivery, successful policy lookups are reused separately for each group. Commands and membership changes invalidate that delivery’s cache; failed lookups are never cached. A description edited externally during the same delivery may remain stale until the next invalidation or delivery. Inference, archive writes and ack/retry remain per message and sequential.


## Mandatory D1 release preflight

Before manually uploading a Worker, follow [the production D1 schema release gate](docs/manual-d1-release.md). The read-only check `cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --remote` must exit 0 after the additive migration and immediately before upload. Green GitHub Actions does not establish production migration status. Never run `schema/current.sql` against existing D1 data.

The release check includes `warning_edit_receipts` table and `apply_warning_edit_receipt` trigger. Its DDL is compared with the canonical schema and its behavior verified only against dummy in-memory data. Existing receipt tables without the trigger require manual reconciliation (not automatic migration).

Release preflight also requires the scan-fencing columns. With `--pre-migration`, apply only the missing scan `ALTER` statements printed by the checker; do not blindly repeat the one-time scan migration. See [manual release gate](docs/manual-d1-release.md).

Warning edits are replay-safe by Telegram update ID. Retention cleanup cannot hide newer strikes by reusing rowids below a surviving manual baseline. Do not independently rebuild or VACUUM the violations table without preserving its ordering relative to warning baselines.

## Implementation languages and local checks

The deployed moderation logic, history importer and transcoder are Rust. D1
release checks and offline SQLite/source/documentation regressions are also
native Rust in [`tools/maintenance`](tools/maintenance/README.md); Python is not
required. Run `cargo test --manifest-path tools/maintenance/Cargo.toml --locked`.

The audio transcoder's [resource limits and regression checks](transcoder/README.md)
describe its bounded 8 MiB stdout reader and child cleanup on overflow, timeout
or request cancellation. Run `cargo test --manifest-path transcoder/Cargo.toml`.

The remaining handwritten JavaScript is `tests/worker_runtime.mjs`, which tests
the actual compiled JS/Wasm artifact and Cloudflare host boundary. Generated
`build/index.js` is the SDK's required Wasm loader/glue. Node/Wrangler remain
build/development tools; replacing those boundary tests with native Rust would
lose integration coverage. The native maintenance dependencies are isolated from
the production Worker crate.

### Administrator log modes

Use `/logs on|warn|off` in the group, or `/logs -100GROUP_ID on|warn|off` privately as an administrator of that exact group. Omit the mode to query it. The only accepted modes are `on`, `warn` and `off`.

| Mode | Unsolicited private administrator logs |
| --- | --- |
| `on` | All review logs and existing action/scan notifications (unchanged default) |
| `warn` | Deletions/bans, dry-run and audit hits, manual-review outcomes, too-old violations and review failures; skips ordinary allows, already-processed duplicates and routine success/info scan/cancellation notices |
| `off` | No unsolicited private administrator logs |

Command replies and group action notices always remain independent of these modes. The mode is persisted per group, survives restarts and does not change dry-run, thresholds, penalties or archiving. `on` restores full logging after `warn`; delivery rechecks the mode after external lookups before rendering digests.

Existing deployments require the additive `group_settings.logs_warn_only` column. Run the Rust preflight in `--pre-migration` mode, execute only the missing ALTER printed from `schema/ensure_log_filter.sql`, then require a normal preflight exit 0 before uploading this Worker. Existing groups keep their previous on/off setting and default all-log behavior. This is a one-time ALTER; do not rerun it if the column already exists.


### Extended message and URL review

- Telegram text/caption, ordinary supported image/audio/video files, stickers, documents of recognized media MIME types, and supported metadata-only messages (polls, locations, venues, contacts, dice, games, paid-media notices, stories, giveaways, payment notices and video-chat events) are eligible for review. Text-only or structured metadata is evaluated with **Clef-flash**; actual supported image/audio/video bytes use **Clef-omni**. Not all Telegram media formats are decodable: unsupported and oversized files receive a review-error status rather than a false text-only clearance.
- HTTP(S) links in text/caption and Telegram URL/text-link entities are inspected using the **Worker's built-in Rust outbound fetch** (up to two distinct URLs per message). This is untrusted evidence, not an independent enforcement rule. Requests restrict hostnames, credentials, ports, redirections, body size and accepted content types; only short text excerpts enter the model. Failed and blocked fetches do not imply an offense. The bot does not execute web scripts or scan content behind logins, and linked media files are not decoded.
- Contact phone numbers, exact shared coordinates and payment payloads are omitted from the AI structured summary. Archived Telegram metadata may still include sensitive values; configure `ARCHIVE_DAYS` and `/logs` according to group privacy requirements.
- **Telegram Bot API limitation:** `deleteMessage` can delete messages only within its applicable time window (normally 48 hours), even if the sender was a group administrator. The administrator exemption applies to automatic bans, not to this API deletion-age limit.



Link evidence minimizes derived metadata: displayed `url`/`final_url` fields omit username, password, query and fragment; rejected targets produce only `blocked_target`, without an address. Redirect destinations are validated before any request, and rejected redirect addresses/Location headers are never copied to model input. Original URLs (including queries) are still used for permitted fetches so page behavior is preserved; these requests disclose the destination/query to the target website. Paths remain visible and can themselves contain secrets. Original `message_text`, `caption`, group rules, structured message content and fetched page excerpts are review content, not guaranteed secret-free: visible credentials or secrets in those fields may reach Workers AI. Hidden `text_link` destinations are not independently included in structured content. This fix does not rewrite or erase original Telegram payloads in D1 or Queue storage, change retention, or claim an observed production leak. No D1 migration is required.
