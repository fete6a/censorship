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

### Public messages in the affected group

- **After successful deletion**, the bot immediately attempts a MarkdownV2 notice with the deleted message ID, category, a clickable user display name (numeric ID fallback), the seven-day warning count as a three-step Markdown bar, and, where applicable, the 60-second mute, pending ban and `/cancel USER_ID` instructions. A failed mute is reported as such, not as a completed ban.
- **Only after Telegram confirms a ban** does the bot send a separate completed-ban announcement. Notifications use escaped Telegram MarkdownV2 and generated numeric-ID links; links to deleted messages are deliberately omitted. Notices stay in the affected group and do not quote the deleted message's text.
- Dry-run and audit reviews never claim that a deletion or ban happened. Posting is **best effort**: if Telegram rejects a group notice, the already-completed moderation action is not rolled back or repeated to retry the notice.
- All supported commands, including `/start` and `/help`, reply in their originating chat and forum topic, even with `/logs off`. Unknown commands, invalid arguments, permission denials and disabled groups receive feedback. Commands addressed to another bot are ignored. Help, unknown and denied group commands still undergo normal moderation.

## Telegram permissions and delivery requirements

1. **Receiving and moderating:** the bot must be an administrator in the target group to receive all relevant group updates (without disabling Privacy Mode). `can_delete_messages` is required for automatic deletion; without it, moderation is paused. The group must be registered or rediscovered through updates.
2. **Muting and banning:** a **supergroup** and `can_restrict_members` are required in addition to deletion permission. With deletion permission alone, the bot can delete but cannot automatically mute or ban. Punishment additionally requires enforcement to be enabled and the group description to contain applicable rules.
3. **Public notices:** the bot must still have access to the group **and be able to send messages there** (`sendMessage`). Delete/restrict permissions do not guarantee that public notices can be posted. If Telegram refuses delivery, the action can still have succeeded; the notice is not retried.
4. **Private logs:** `getChatAdministrators` must succeed for the target group, and each human administrator must have initiated a private chat with the bot and permit its messages. The bot cannot initiate a DM conversation with an administrator who has never contacted it. Delivery failures do not undo an archived review or completed punishment.
5. **Webhook:** configure the Bot API webhook with an independent `secret_token` and `allowed_updates=["message","edited_message","my_chat_member","chat_member"]`, so group discovery, message reviews and administrator cancellations receive the necessary updates.

## Administrator commands

All management commands check the issuing user's `getChatMember` administrator/creator status **in the exact target group** before accessing its data. Private commands require a **negative group ID**; group commands always target their originating group. Group settings apply only to that group.

| Purpose | Command in the group | Command in a private chat |
| --- | --- | --- |
| Removal threshold (query or set) | `/threshold [0.75 or 75%]` | `/threshold -100GROUP_ID [0.75 or 75%]` |
| Enforcement mode (query or set) | `/dry-run [on\|off]` (also `/dry_run`) | `/dry_run -100GROUP_ID [on\|off]` |
| Private review logs (query or set) | `/logs [on\|off]` | `/logs -100GROUP_ID [on\|off]` |
| Read group policy | `/rules` | `/rules -100GROUP_ID` |
| Cancel pending ban | `/cancel USER_ID` | `/cancel -100GROUP_ID USER_ID` |
| Inspect pending ban | `/pending USER_ID` | `/pending -100GROUP_ID USER_ID` |
| Re-review archived or replied-to message | Reply with `/scan` or use `/scan MESSAGE_ID` | `/scan_id -100GROUP_ID MESSAGE_ID` |
| Scan archived messages (audit by default) | `/scan_all [enforce]` | `/scan_all -100GROUP_ID [enforce]` |
| Scan progress / stop | `/scan_status` / `/scan_stop` | `/scan_status -100GROUP_ID` / `/scan_stop -100GROUP_ID` |

Send `/start` or `/help` in a group or private chat to see command help. Omitting `on`/`off` queries the current logs/dry-run setting. Replies are independent of private-log settings; a failed reply does not replay the completed management action.

`/scan` and `/scan_id` accept a replied-to message in the same group or a message ID already archived for that group. The bot sends a start notice and then the stored review status, category and violation probability. Missing policy or failed inference is reported as unavailable, never as a clean pass. Scans default to audit; append `enforce` to request enforcement under the existing dry-run and permission checks. The Bot API cannot fetch an arbitrary unarchived message by ID. Transient command failures produce a generic error reply and retain Queue retry behavior.

Use `/threshold` to query or set the removal threshold (default `0.93`); only future reviews are affected. The model answers a typed `noul` yes/no violation question with a numeric probability (`answers.violation.noul`), rather than producing an `allow` decision. Rust computes `allow` below threshold, `needs_review` when the score meets threshold but the category is `none`, and deletion when the score meets threshold with an actionable category. Dry-run, explicit group policy, Telegram permissions, deletion age and duplicate-punishment protections are unchanged. These model scores are not calibrated certainty estimates. `/dry-run on` cancels pending bans; `/logs off` does not affect punishment.

## Configuration and deployment

- Set `BOT_TOKEN` and `WEBHOOK_SECRET` as **Cloudflare/Wrangler secrets**, not repository or YAML values. The webhook secret is distinct from the Telegram bot token.
- `DRY_RUN=true` is the deployment default, inherited by groups without a dry-run override. `ENABLE_BAN=true` and `IMMEDIATE_BAN=false` are separate enforcement settings.
- `ARCHIVE_DAYS=0` means archived messages are **not automatically removed**. Set an appropriate positive retention period for your deployment.
- `BOT_USERNAME` is used to recognize commands addressed to this bot in groups.
- There is **no active `ADMIN_DM_CHAT_ID` recipient configuration**. Leaving it unset **does not** disable administrator DMs. Setting it does not select or change the current per-group recipients. Use `/logs off` separately for any group whose private review logs should be disabled.
- The persistent per-group `logs_enabled` and `dry_run` switches and `remove_threshold` value live in D1; they are **not** controlled by a global DM chat ID. The project supports **only the current schema** (`schema/current.sql`) and no longer includes legacy migrations or in-place schema upgrades. To initialize a **new empty** D1 database once, configure the binding and run `npx wrangler d1 execute DB --remote --file schema/current.sql`. **Never run this bootstrap file on an existing D1 database.** Existing deployments must already match this schema. Successful main-branch push builds automatically deploy their tested artifact in a separate job. Deployment verifies required columns with read-only D1 queries and fails before publishing if the schema is outdated. Pull-request builds never access production D1 or deployment secrets.

See `wrangler.jsonc`, `schema/current.sql` and `.github/workflows/build.yml` for current D1/Queues bindings, a fresh-database bootstrap, Rust fat-LTO CI and automatic deployment from main (with optional manual dispatch). Production must have the current schema, queues, AI binding, Telegram webhook and secrets configured before deploying; old schema layouts are not supported.

## Security and privacy

Do not commit production bot tokens, webhook secrets, Cloudflare credentials, private Telegram identifiers or real chat content. Keep credentials in Wrangler/Cloudflare Secrets; rotate exposed tokens. Only a verified administrator of a group may change that group's settings or read its managed data through commands.

**Default-on administrator DMs are a privacy-relevant behavior, even with `DRY_RUN=true`.** Telegram messages reviewed by the bot can be archived in D1 with message text, captions and media file IDs. Media bytes may be downloaded transiently for review. Routine admin logs disclose review metadata (including group and message IDs, the current group owner’s display name and numeric profile link, categories and scores); completed-action notices disclose enforcement details to the group. Public notices do not reproduce deleted message text. `/logs off` does not erase already sent messages, change D1 retention, stop platform logs or prevent public notices.

Group administrators should review notification and data-retention settings before adding the bot. If posting or DM delivery is denied by Telegram, notification attempts may fail even when moderation or archiving succeeds.


Passive review digests read final statuses with one D1 query per group and Queue batch (rather than per message); `/logs off` skips these reads. All digest chunks share one fresh administrator lookup. Each message still has its own inference, archive, acknowledgement and retry.

## Automatic CI deployment

Every successful push build on `main` uploads a checksummed Worker artifact, then a separate deployment job downloads that same artifact and publishes it to Cloudflare. The deploy job uses repository Actions secrets `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`; the Telegram token and webhook secret remain in Cloudflare. Pull requests run tests only. Manual deployment remains available from `main` with the `deploy` input enabled.

Deployment never recompiles Rust or bundles the tested JavaScript: it removes the custom build command from an ephemeral copy of the Wrangler configuration and uses `--no-bundle`. SHA-256 checks run before and after upload, main-head checks skip superseded builds, and production deployment jobs are serialized. Existing variables are retained with `--keep-vars`; D1 checks are read-only, followed by a live health check. The Actions summary records the deployed source commit.

Private administrator digests and action logs include the current group name, numeric ID and a clickable group link. Public groups use their username; private groups use an existing Telegram invite link when available, otherwise a surviving reviewed-message link accessible to group members. No new invitations are generated. If Telegram metadata is unavailable, logs fall back to the group ID and available message link. Group titles are Markdown-escaped, and recipients remain the exact group's current human administrators. Probability scores use ten-step Markdown bars with exact percentages. Missing policy or inference errors show no fabricated score; empty categories and repeated group IDs are omitted. Each log also identifies the current creator by their escaped display name and a numeric `tg://user?id=...` profile link, using the same administrator snapshot without extra API requests. Missing creator metadata is labelled unavailable; a missing name falls back to the numeric user ID. Digest chunks reserve space for both headings.

Warning counts use the existing per-group, per-user seven-day strike records and show the full count even above three; the bar saturates at the three-warning threshold. Archived messages preserve user display names for later scans. Anonymous senders do not receive an attributed warning count.

Within a Queue delivery, successful policy lookups are reused separately for each group. Commands and membership changes invalidate that delivery’s cache; failed lookups are never cached. A description edited externally during the same delivery may remain stale until the next invalidation or delivery. Inference, archive writes and ack/retry remain per message and sequential.
