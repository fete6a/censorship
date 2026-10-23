# Silent Telegram Clef-omni Moderator

A **Rust/WebAssembly Cloudflare Worker** that automatically registers Telegram groups where it becomes an admin, uses each group's `getChat.description` as its moderation policy, analyzes text/images/audio/video via `@cf/cloudflare/clef-omni`, archives Bot API updates in D1, and enforces delete → warnings → 60-second mute → ban unless an admin cancels.

**Silent by design:** the bot never calls `sendMessage` with a group chat ID. Private admin commands and optional alerts are available; enable them by messaging the bot privately first.

- Autodiscovery: `my_chat_member` events and first-message fallback. Bot API has **no** method to list all previously joined groups.
- Full permission for automatic ban: supergroup admin with `can_delete_messages` and `can_restrict_members`. With delete permission only, bot can moderate/delete but does **not** mute or ban.
- No group notification privileges required; no group warning messages. Warnings/strikes persist in D1.
- Default `DRY_RUN=true`: classification/archive only until explicitly enabled.
- Legacy Telegram Desktop JSON importer available for text-only history; Bot API cannot fetch unobserved past messages.

**Full Chinese setup and security instructions:** [README_CN.md](README_CN.md).

## Commands

In a group: `/cancel USER_ID`, `/pending USER_ID`, `/rules`, `/scan [ID]`, `/scan_all`, `/scan_status`, `/scan_stop` (commands work but there will be no group replies).

Private DM: `/cancel -100GROUP_ID USER_ID`, `/pending -100GROUP_ID USER_ID`, `/rules -100GROUP_ID`, `/scan_id -100GROUP_ID MESSAGE_ID`, `/scan_all -100GROUP_ID`, `/scan_status -100GROUP_ID`, `/scan_stop -100GROUP_ID`. All commands verify `getChatMember` admin status in the target registered group.

## Security

No bot token, Telegram identifiers, Cloudflare API credentials, or real chat content is committed to the repository. Store secrets using Wrangler / Cloudflare Secrets. An optional positive `ADMIN_DM_CHAT_ID` is the *only* unsolicited notification target. Never set it to a group ID. Configure the Telegram webhook with `allowed_updates=["message","edited_message","my_chat_member","chat_member"]` and an independent `secret_token`.

See `.github/workflows/build.yml` for Rust fat-LTO build and artifact upload. Deployment is opt-in and requires Cloudflare credentials and provisioned D1/Queues.
