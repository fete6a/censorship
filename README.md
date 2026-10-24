# Telegram Clef-omni Moderator

A **Rust/WebAssembly Cloudflare Worker** that discovers Telegram groups where it is an administrator, uses each group's `getChat.description` as its moderation policy, reviews text and media with `@cf/cloudflare/clef-omni`, archives Bot API updates in D1, and can delete messages, warn users, mute them for 60 seconds, and ban them after the administrator cancellation window.

**This bot is not silent.** Private review logs are enabled by default for each managed group, including when moderation is in dry-run mode. Successful enforcement also triggers public notices in the affected group. Group `/threshold` commands produce public replies. These are separate notification paths.

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
- `/logs off` stops unsolicited **private review logs for that group only**; `/logs on` re-enables them. It **does not disable group action notices**, public `/threshold` replies, D1 archiving, Worker console logs, or replies to commands issued in a private chat. Turning logs off for one group does not affect another group.

### Public messages in the affected group

- **After successful deletion**, the bot immediately attempts a MarkdownV2 notice with the deleted message ID, category, clickable numeric user mention, and where applicable the strike count, 60-second mute, pending ban and `/cancel USER_ID` instructions. A failed mute is reported as such, not as a completed ban.
- **Only after Telegram confirms a ban** does the bot send a separate completed-ban announcement. Notifications use escaped Telegram MarkdownV2 and generated numeric-ID links; links to deleted messages are deliberately omitted. Notices stay in the affected group and do not quote the deleted message's text.
- Dry-run and audit reviews never claim that a deletion or ban happened. Posting is **best effort**: if Telegram rejects a group notice, the already-completed moderation action is not rolled back or repeated to retry the notice.
- `/threshold` status and update commands issued in a group also send a **public** confirmation, even with `/logs off`. Other group administrator commands generally produce no group reply; their effects may still result in enforcement notices.

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

Send `/start` or `/help` **privately** to see the command help. Omitting `on`/`off` queries the current logs/dry-run setting. In a group, `/threshold` confirms publicly; the other settings commands do not announce their result there.

The default removal threshold is `0.93`; only future reviews are affected. The model answers a typed `noul` yes/no violation question with a numeric probability (`answers.violation.noul`), rather than producing an `allow` decision. Rust computes `allow` below threshold, `needs_review` when the score meets threshold but the category is `none`, and deletion when the score meets threshold with an actionable category. Dry-run, explicit group policy, Telegram permissions, deletion age and duplicate-punishment protections are unchanged. These model scores are not calibrated certainty estimates. `/dry-run on` cancels pending bans; `/logs off` does not affect punishment.

## Configuration and deployment

- Set `BOT_TOKEN` and `WEBHOOK_SECRET` as **Cloudflare/Wrangler secrets**, not repository or YAML values. The webhook secret is distinct from the Telegram bot token.
- `DRY_RUN=true` is the deployment default, inherited by groups without a dry-run override. `ENABLE_BAN=true` and `IMMEDIATE_BAN=false` are separate enforcement settings.
- `ARCHIVE_DAYS=0` means archived messages are **not automatically removed**. Set an appropriate positive retention period for your deployment.
- `BOT_USERNAME` is used to recognize commands addressed to this bot in groups.
- There is **no active `ADMIN_DM_CHAT_ID` recipient configuration**. Leaving it unset **does not** disable administrator DMs. Setting it does not select or change the current per-group recipients. Use `/logs off` separately for any group whose private review logs should be disabled.
- The persistent per-group `logs_enabled` and `dry_run` switches and `remove_threshold` value live in D1; they are **not** controlled by a global DM chat ID. The project supports **only the current schema** (`schema/current.sql`) and no longer includes legacy migrations or in-place schema upgrades. To initialize a **new empty** D1 database once, configure the binding and run `npx wrangler d1 execute DB --remote --file schema/current.sql`. **Never run this bootstrap file on an existing D1 database.** Existing deployments must already match this schema. Manual GitHub Actions deployments verify required columns with read-only D1 queries and fail before `wrangler deploy` if the schema is outdated. Ordinary PR/push builds do not access production D1 or deploy.

See `wrangler.jsonc`, `schema/current.sql` and `.github/workflows/build.yml` for current D1/Queues bindings, a fresh-database bootstrap, Rust fat-LTO CI and optional manual deployment. Production must have the current schema, queues, AI binding, Telegram webhook and secrets configured before deploying; old schema layouts are not supported.

## Security and privacy

Do not commit production bot tokens, webhook secrets, Cloudflare credentials, private Telegram identifiers or real chat content. Keep credentials in Wrangler/Cloudflare Secrets; rotate exposed tokens. Only a verified administrator of a group may change that group's settings or read its managed data through commands.

**Default-on administrator DMs are a privacy-relevant behavior, even with `DRY_RUN=true`.** Telegram messages reviewed by the bot can be archived in D1 with message text, captions and media file IDs. Media bytes may be downloaded transiently for review. Routine admin logs disclose review metadata (including group and message IDs, categories and scores); completed-action notices disclose enforcement details to the group. Public notices do not reproduce deleted message text. `/logs off` does not erase already sent messages, change D1 retention, stop platform logs or prevent public notices.

Group administrators should review notification and data-retention settings before adding the bot. If posting or DM delivery is denied by Telegram, notification attempts may fail even when moderation or archiving succeeds.


Passive review digests read final statuses with one D1 query per group and Queue batch (rather than per message); `/logs off` skips these reads. All digest chunks share one fresh administrator lookup. Each message still has its own inference, archive, acknowledgement and retry.
