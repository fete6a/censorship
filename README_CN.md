# Telegram Clef-omni 审核机器人（静默多群版）

**状态：** Rust 源码 + GitHub Actions LTO 编译方案。部署前请先确认 GitHub Actions 的 `Build Rust/Wasm` 结果成功。默认 `DRY_RUN=true`，即审核并归档，不自动删除。

## 自动发现群组

- **无需设置固定 `GROUP_CHAT_ID`**。Bot API 推送的 `my_chat_member` 会在机器人被加入、晋升、降级或移出群组时触发；程序把群组及权限记录到 D1 `managed_chats`。
- 若机器人在 Webhook 配置前已加入某群，无法主动遍历过去加入的群。第一次收到该群的消息后，通过 `getMe + getChatMember` 尝试登记。
- 机器人需要成为管理员才能接收全量群消息（不必关闭 Privacy Mode），并至少拥有 **删除消息** 权限。**限制成员** 权限只能在超级群组启用禁言与封禁，否则仅能删除审核识别的违规消息。
- `getChat.description` 的文字就是处罚规则；群简介为空则不自动处罚。建议规则明确、简短并告诉用户。不同群组独立归档及处罚。
- 删除机器人或移除权限会将该群停用，并取消未到期的待封禁任务。

## 绝不向群组发送消息

- 全部 `sendMessage` 目标都必须是正数 Telegram 私聊 ID；程序对负数群组 ID 直接跳过通知。没有群内发言权限也能审核和删除。
- 删除、警告、封禁的审计记录放到 Cloudflare D1 `violations`、`archived_messages` 和 `pending_bans`；不会给群成员发送公开警告信息。
- 设置可选变量 `ADMIN_DM_CHAT_ID`（正数且管理员已私聊 `/start`）才会收到异常/处罚提醒；留空则不发送任何自动通知。
- 管理员命令可以在群内发出（不会收到群内回复），**建议在机器人私聊执行命令**，例如：`/cancel -1001234567890 12345678`。机器人每次验证发送者是否为目标群管理员。

## 审核及处罚

- 文字、照片、音频、视频通过 Cloudflare Workers AI `@cf/cloudflare/clef-omni` 审核。OGG/Opus 语音需要额外配置独立 HTTPS 转码服务（见 `transcoder/`）。大文件或模型不可用：**不自动处罚**。
- 判断标准以群简介为准，要求较高置信度。不在群简介禁止范围内的内容不得因此模型自带的“喜好”被处罚。
- 清晰违规：先删除消息并在 D1 记录警告。7 天累计 3 次（默认）会触发禁言 60 秒（超级群组）。到期且管理员未取消则自动封禁。
- 管理员始终不被自动封禁；其他管理员也可以在 Telegram UI 手动解除禁言，或通过 `/cancel` 取消待封禁处罚。
- 必须设置 `DRY_RUN=false` 才启用真实删除和处罚。建议先运行真实流量审计，再开启。

## 管理员命令（私聊优先）

| 群内命令（静默） | 私聊命令 | 功能 |
| --- | --- | --- |
| `/rules` | `/rules -100GROUP_ID` | 读取群规则 |
| `/cancel USER_ID` | `/cancel -100GROUP_ID USER_ID` | 取消待封禁，尝试恢复发言权限 |
| `/pending USER_ID` | `/pending -100GROUP_ID USER_ID` | 查看剩余取消期 |
| 回复消息 `/scan` | `/scan_id -100GROUP_ID MESSAGE_ID` | 重新审查，不处罚 |
| `/scan_all` | `/scan_all -100GROUP_ID` | 扫描机器人已归档的所有消息，默认仅审计 |
| `/scan_all enforce` | `/scan_all -100GROUP_ID enforce` | 对仍在可删除期限内的违规消息尝试执行处罚 |
| `/scan_status` | `/scan_status -100GROUP_ID` | 查询进度 |
| `/scan_stop` | `/scan_stop -100GROUP_ID` | 停止扫描 |

Telegram 官方 Bot API 无法遍历机器人加入前的消息、也无法任意拉取旧消息 ID。`tools/history-import/` 提供桌面版文本 JSON 离线导入工具，具体导入方式需由数据持有人授权。旧媒体不在导入范围内。Telegram 对大部分消息有 48 小时删除限制。

## GitHub Actions 构建

推送到 `main` 会自动触发 `.github/workflows/build.yml`：下载 Rust stable 和 `wasm32-unknown-unknown` target、安装 `worker-build`、运行 SQLite/静态测试、使用 `Cargo.toml` 的 `lto="fat"` 构建 Wasm，并上传构建产物 Artifacts。无需在 GitHub 保存 BOT_TOKEN。

构建 LTO 配置：

```toml
[profile.release]
opt-level = "s"
lto = "fat"
codegen-units = 1
strip = "debuginfo"
panic = "abort"
```

GitHub Workflow 可选手动触发部署，但只有在设置 **GitHub Actions Secrets** `CLOUDFLARE_API_TOKEN`、`CLOUDFLARE_ACCOUNT_ID`，并创建好 D1 和 Queues、把正确数据库 UUID 写入 `wrangler.jsonc` 后才能成功。不要把 Bot Token 写入公开 Git 仓库或 Actions YAML。Cloudflare 端通过 `wrangler secret put BOT_TOKEN` 与 `wrangler secret put WEBHOOK_SECRET` 保存。

首次启动所需资源（本地命令，或由有权限的 Cloudflare connector 创建）：

```sh
npx wrangler d1 create telegram-clef-moderation
npx wrangler queues create telegram-clef-moderation-events
npx wrangler queues create telegram-clef-ban-timers
npx wrangler queues create telegram-clef-moderation-dlq
npx wrangler d1 migrations apply telegram-clef-moderation --remote
```

设置完 Cloudflare Secrets 与 Worker URL 后，用自己的终端注册 Webhook（`drop_pending_updates=false` 避免遗漏待处理更新）：

```sh
read -r -s -p 'BOT_TOKEN: ' BOT_TOKEN; echo
read -r -s -p 'WEBHOOK_SECRET: ' WEBHOOK_SECRET; echo
WORKER_URL='https://YOUR-WORKER.workers.dev'
curl -fsS "https://api.telegram.org/bot${BOT_TOKEN}/setWebhook" \
  --data-urlencode "url=${WORKER_URL}/telegram" \
  --data-urlencode "secret_token=${WEBHOOK_SECRET}" \
  --data-urlencode 'allowed_updates=["message","edited_message","my_chat_member","chat_member"]' \
  --data-urlencode 'drop_pending_updates=false'
unset BOT_TOKEN WEBHOOK_SECRET
```

## 数据与风险

- 消息文字、caption 和 Telegram 媒体 `file_id` 将保存在 D1；媒体字节在审核时临时下载给 Workers AI，并不长期保存。`ARCHIVE_DAYS=0` 表示不自动清理归档，请根据隐私需求修改。
- Bot Token 是管理员凭据，**必须通过 BotFather 轮换用户曾在聊天中贴出的密钥**，不可上传到 GitHub。Webhook secret 另用随机字符串。
- `ADMIN_DM_CHAT_ID` 只能填写私聊 ID，机器人不能主动给从未发起过聊天的用户发送 DM。
- 本项目没有内置 Telegram UI，它通过 Telegram Bot API 和 Cloudflare Rust Workers 运行；请先做真实端到端误判、权限、取消期测试。
