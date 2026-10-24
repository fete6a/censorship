# Telegram Clef-omni Moderator

A **Rust/WebAssembly Cloudflare Worker** that discovers Telegram groups where it is an administrator, uses each group's `getChat.description` as its moderation policy, reviews text and media with `@cf/cloudflare/clef-omni`, archives Bot API updates in D1, and can delete messages, warn users, mute them for 60 seconds, and ban them after the administrator cancellation window.

**This bot is not silent.** Private review logs are enabled by default for each managed group, including when moderation is in dry-run mode. Successful enforcement also triggers public notices in the affected group. Commands reply in the chat and forum topic where they were issued. These are separate notification paths.

- **Group discovery:** `my_chat_member` updates and a first-message fallback. The Bot API cannot list every group the bot previously joined.
- **Safe enforcement default:** `DRY_RUN=true` archives and classifies messages without deleting or banning; per-group `/dry-run off` enables enforcement.
- **Per-group isolation:** each group has independent administrator authorization, dry-run, private-log and removal-threshold settings. Clef-omni's binary `violation.noul` score threshold defaults to 93%. Cloudflare Queues batches up to 20 events or 10 seconds (whichever comes first); each message is still classified and punished independently.
- **History:** a Telegram Desktop JSON importer handles text-only history. The Bot API cannot retrieve unobserved old messages.

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
| Private review logs (query or set) | `/logs [on\|off]` | `/logs -100GROUP_ID [on\|off]` |
| Read group policy | `/rules` | `/rules -100GROUP_ID` |
| Cancel pending ban | `/cancel USER_ID` | `/cancel -100GROUP_ID USER_ID` |
| Inspect pending ban | `/pending USER_ID` | `/pending -100GROUP_ID USER_ID` |
| Read or set a user's seven-day warnings | `/warnings USER_ID [0-3]` | `/warnings -100GROUP_ID USER_ID [0-3]` |
| Re-review and enforce an archived or replied-to message by default | Reply with `/scan [audit]` or use `/scan MESSAGE_ID [audit]` | `/scan_id -100GROUP_ID MESSAGE_ID [audit]` |
| Scan archived messages (audit by default) | `/scan_all [enforce]` | `/scan_all -100GROUP_ID [enforce]` |
| Scan progress / stop | `/scan_status` / `/scan_stop` | `/scan_status -100GROUP_ID` / `/scan_stop -100GROUP_ID` |

Send `/start` or `/help` in a group or private chat to see command help. Omitting `on`/`off` queries the current logs/dry-run setting. Replies are independent of private-log settings; a failed reply does not replay the completed management action.

`/scan` and `/scan_id` accept a replied-to message in the same group or a message ID already archived for that group. **Single-message scans now request enforcement by default**; append `audit` to inspect without punishment (legacy `enforce` remains accepted). They report the stored status, category and violation score, and explicitly explain when a dry-run prevented deletion. Even an enforcement scan never bypasses group policy, a nonactionable category, configured threshold, a group's `/dry_run on` state, the 48-hour deletion window, Telegram permissions or duplicate-strike protections. **Bulk `/scan_all` still defaults to audit** and requires an explicit `enforce`. The Bot API cannot fetch arbitrary unarchived messages by ID. Missing policy or model failure is reported as unavailable, not a clean pass; transient errors retain Queue retries.

Use `/warnings USER_ID` to see that user's effective warning count **in the current group**, or `/warnings USER_ID 0` through `3` to set it. In private chat, include a negative group ID first, e.g. `/warnings -1001234567890 22 2`. The issuing user must be an administrator/creator of that **specific target group**; group commands cannot select a different group. The change never deletes prior violation evidence and never triggers punishment on its own. A new genuine violation after the adjustment adds to the set baseline; the manual baseline expires after seven days. Any still-pending automated ban for that group/user is canceled on adjustment, without overriding unrelated human restrictions.

Use `/threshold` to query or set the removal threshold (default `0.93`); only future reviews are affected. The model answers a typed `noul` yes/no violation question with a numeric probability (`answers.violation.noul`), rather than producing an `allow` decision. Rust computes `allow` below threshold, `needs_review` when the score meets threshold but the category is `none`, and deletion when the score meets threshold with an actionable category. Clef categories `other` and `adult` are normalized to `none` before decisions and storage, so neither can cause deletions, warnings or bans; both follow the same `allow`/`needs_review` threshold behavior as `none`. Dry-run, explicit group policy, Telegram permissions, deletion age and duplicate-punishment protections are unchanged. These model scores are not calibrated certainty estimates. `/dry-run on` cancels pending bans; `/logs off` does not affect punishment.

## Configuration and deployment

- Set `BOT_TOKEN` and `WEBHOOK_SECRET` as **Cloudflare/Wrangler secrets**, not repository or YAML values. The webhook secret is distinct from the Telegram bot token.
- `DRY_RUN=true` is the deployment default, inherited by groups without a dry-run override. `ENABLE_BAN=true` and `IMMEDIATE_BAN=false` are separate enforcement settings.
- `ARCHIVE_DAYS=0` means archived messages are **not automatically removed**. Set an appropriate positive retention period for your deployment.
- `BOT_USERNAME` is used to recognize commands addressed to this bot in groups.
- There is **no active `ADMIN_DM_CHAT_ID` recipient configuration**. Leaving it unset **does not** disable administrator DMs. Setting it does not select or change the current per-group recipients. Use `/logs off` separately for any group whose private review logs should be disabled.
- The persistent per-group `logs_enabled` and `dry_run` switches and `remove_threshold` value live in D1; they are **not** controlled by a global DM chat ID. New databases use the **current full schema** (`schema/current.sql`), applied only to an empty database via `npx wrangler d1 execute DB --remote --file schema/current.sql`. **Never run that bootstrap file on existing D1 data.** To enable `/warnings` safely on an existing deployment, first apply the *additive, idempotent* `schema/ensure_warning_adjustments.sql` with the authorized Cloudflare D1 connector (or `npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql`); it adds one table without modifying any existing rows. There is still no legacy migration chain and no automatic CI database modification. GitHub Actions builds/tests artifacts only; release the verified artifact using Cloudflare MCP after ensuring the extra table exists.

See `wrangler.jsonc`, `schema/current.sql` and `.github/workflows/build.yml` for current D1/Queues bindings, a fresh-database bootstrap, Rust fat-LTO CI and manual MCP deployment. Production must have the current schema, queues, AI binding, Telegram webhook and secrets configured before deploying; old schema layouts are not supported.

## Security and privacy

Do not commit production bot tokens, webhook secrets, Cloudflare credentials, private Telegram identifiers or real chat content. Keep credentials in Wrangler/Cloudflare Secrets; rotate exposed tokens. Only a verified administrator of a group may change that group's settings or read its managed data through commands.

**Default-on administrator DMs are a privacy-relevant behavior, even with `DRY_RUN=true`.** Telegram messages reviewed by the bot can be archived in D1 with message text, captions and media file IDs. Media bytes may be downloaded transiently for review. Routine admin logs disclose review metadata (including group and message IDs, the current group owner’s display name and numeric profile link, categories and scores); completed-action notices disclose enforcement details to the group. Public notices do not reproduce deleted message text. `/logs off` does not erase already sent messages, change D1 retention, stop platform logs or prevent public notices.

Group administrators should review notification and data-retention settings before adding the bot. If posting or DM delivery is denied by Telegram, notification attempts may fail even when moderation or archiving succeeds.


Passive review digests read final statuses with one D1 query per group and Queue batch (rather than per message); `/logs off` skips these reads. All digest chunks share one fresh administrator lookup. Each message still has its own inference, archive, acknowledgement and retry.

## Manual deployment through Cloudflare MCP

GitHub Actions runs builds and tests, then retains the Worker artifact with SHA-256 checksums. Neither pushes nor manual workflow dispatch deploy to Cloudflare. Actions does not use `CLOUDFLARE_API_TOKEN` or `CLOUDFLARE_ACCOUNT_ID` secrets.

To release, download the successful main-branch artifact and verify its checksums and source commit. Use the authenticated Cloudflare MCP connection to check D1 columns with read-only queries, then upload the unchanged `index.js` and binary `index_bg.wasm` as Worker modules. Preserve existing bindings, secrets, variables, compatibility settings and observability. Check the active deployment and `/healthz` after upload. Database contents, Queue consumers, Cron triggers and Telegram webhook configuration remain in place.

Private administrator digests and action logs include the current group name, numeric ID and a clickable group link. Public groups use their username; private groups use an existing Telegram invite link when available, otherwise a surviving reviewed-message link accessible to group members. No new invitations are generated. If Telegram metadata is unavailable, logs fall back to the group ID and available message link. Group titles are Markdown-escaped, and recipients remain the exact group's current human administrators. Probability scores use ten-step Markdown bars with exact percentages. Missing policy or inference errors show no fabricated score; empty categories and repeated group IDs are omitted. Each log also identifies the current creator by their escaped display name and a numeric `tg://user?id=...` profile link, using the same administrator snapshot without extra API requests. Missing creator metadata is labelled unavailable; a missing name falls back to the numeric user ID. Digest chunks reserve space for both headings.

Warning counts use the existing per-group, per-user seven-day strike records and show the full count even above three; the bar saturates at the three-warning threshold. Archived messages preserve user display names for later scans. Anonymous senders do not receive an attributed warning count.

Within a Queue delivery, successful policy lookups are reused separately for each group. Commands and membership changes invalidate that delivery’s cache; failed lookups are never cached. A description edited externally during the same delivery may remain stale until the next invalidation or delivery. Inference, archive writes and ack/retry remain per message and sequential.


## Mandatory D1 release preflight

Before manually uploading any Worker built from PR #11 or later, follow [the production D1 schema release gate](docs/manual-d1-release.md). The read-only check `python3 tools/check_d1_schema.py --remote` must exit 0 after the additive migration and immediately before upload. Green GitHub Actions does not establish production migration status. Never run `schema/current.sql` against existing D1 data.

The release check includes PR #13's `warning_edit_receipts` table and `apply_warning_edit_receipt` trigger. Its DDL is compared with the canonical schema and its behavior verified only against dummy in-memory data. Existing receipt tables without the trigger require manual reconciliation (not automatic migration).
