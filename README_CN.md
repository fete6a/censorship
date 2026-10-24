# Telegram Clef-omni 审核机器人（多群独立管理版）

**状态：** Rust 源码 + GitHub Actions LTO 编译方案。部署前请先确认 GitHub Actions 的 `Build Rust/Wasm` 结果成功。默认 `DRY_RUN=true`，即审核并归档，不自动删除。

**管理员私人审核日志默认开启。** 即使保持 dry-run，常规审核结果仍会尝试私聊发送给该群当前的人类管理员；实际删除与封禁完成后还会尝试在群内公告。机器人并非完全静默。

## 自动发现群组

- **无需设置固定 `GROUP_CHAT_ID`**。Bot API 推送的 `my_chat_member` 会在机器人被加入、晋升、降级或移出群组时触发；程序把群组及权限记录到 D1 `managed_chats`。
- 若机器人在 Webhook 配置前已加入某群，无法主动遍历过去加入的群。第一次收到该群的消息后，通过 `getMe + getChatMember` 尝试登记。
- 机器人需要成为管理员才能接收全量群消息（不必关闭 Privacy Mode），并至少拥有 **删除消息**（`can_delete_messages`）权限。**限制成员**（`can_restrict_members`）权限只能在超级群组启用禁言与封禁，否则仅能删除审核识别的违规消息。**群内处罚通知及所有命令回复还需要机器人能向群内发送消息**；仅有删除/限制成员权限，不代表 Telegram 一定接受 `sendMessage`。
- `getChat.description` 的文字就是处罚规则；群简介为空则不自动处罚。建议规则明确、简短并告诉用户。不同群组独立归档及处罚。
- 删除机器人或移除权限会将该群停用，并取消未到期的待封禁任务。

## 群内处罚通知与私聊日志

- 完成删除后，在原群以安全转义的 Telegram MarkdownV2 公布消息 ID、违规类别、可点击的用户名称（缺失时回退到用户 ID）、近 7 天警告次数及三格 Markdown 进度条；达到累计阈值时说明禁言 60 秒、待封禁及 `/cancel USER_ID` 取消方法。实际封禁成功后另发确认，不提前声称已封禁。
- 群内处罚通知仅尝试发送到实际发生处罚的群，不转发被删消息原文。审计与 dry-run 不发送声称已处罚的公告；只有 Telegram 真正确认封禁后才发布封禁完成通知。群内发消息失败不会撤销或重复已完成处罚，通知不单独重试。
- 管理员私聊审核日志默认开启，常规审核（包括通过、待核实、dry-run）会在 Queue 批次内**按群汇总**，以减少刷屏；历史扫描完成、处罚、取消状态仍独立通知。每批发送前通过 `getChatAdministrators` 重新查询**目标群当前的非机器人管理员**。管理员必须先主动私聊机器人（例如 `/start`），且未屏蔽机器人；Telegram 拒绝私聊或管理员查询失败时无法送达。运行错误一般记录在 Worker 控制台，不保证全部通过私聊推送。
- `/logs off` **仅关闭该群主动发送的管理员私人审核日志**，不关闭群内处罚通知、命令回复、D1 归档、Worker 控制台日志或私聊命令回复；不影响其他群。仅通知发送失败不会撤销处罚或重试整个审核。
- 所有支持的命令（包括 `/start`、`/help`）都在原聊天及原话题中回复，**即使已设置 `/logs off` 也会发送**。未知命令、参数错误、权限不足和群组未启用审核都有提示；发给其他机器人的命令不回复。帮助、未知及被拒绝的群内命令仍接受普通审核，不能用命令前缀绕过群规。除群内公开查询 `/rules` 外，管理操作都检查发送者是否为目标群管理员。

## 统一消息格式

机器人对外消息统一使用安全转义的 **Telegram MarkdownV2**：ℹ️ 查询/说明、✅ 设置成功、⚠️ 参数或权限问题、❌ 命令执行异常、🚨 删除与封禁公告、📋 分组审核摘要。命令回复统一中文字段（群组、状态、选项、消息 ID、模式、剩余时间、处置结果），不直接暴露内部 `on/off`、`pending`、`enforce` 等英文状态。审核摘要与单条扫描使用同一状态名称及分数进度条；审核失败不伪造分数。群规、用户名、群名均安全转义；被删除消息不生成无法访问的链接。命令回复保留原聊天及话题，管理员日志保留群组和群主上下文。

## 审核及处罚

- 纯文本通过 Cloudflare Workers AI `@cf/cloudflare/clef-flash` 审核；带照片、音频或视频的消息（包括附带文字说明）继续使用 `@cf/cloudflare/clef-omni`。实时审核、单条扫描和历史扫描共用此路由；不支持、损坏或超限的媒体不会降级为仅审核说明文字。两条路由使用相同问题、解析、阈值与群组保护。模型仅输出 `violation.noul` 违规概率、违规类别及严重度，**不再输出 allow/review/remove 动作，由 Rust 自动决定审核状态**。OGG/Opus 语音需要额外配置独立 HTTPS 转码服务（见 `transcoder/`）。大文件或模型不可用：**不自动处罚**。
- 判断标准以群简介为准，要求较高置信度。不在群简介禁止范围内的内容不得因此模型自带的“喜好”被处罚。
- 清晰违规：先删除消息并在 D1 记录警告。7 天累计 3 次（默认）会触发禁言 60 秒（超级群组）。到期且管理员未取消则自动封禁。
- 管理员始终不被自动封禁；其他管理员也可以在 Telegram UI 手动解除禁言，或通过 `/cancel` 取消待封禁处罚。
- 群管理员用 `/dry-run off` 为本群启用真实删除和处罚（未设置时沿用全局 `DRY_RUN`）。建议先运行真实流量审计，再开启。

## 管理员命令（私聊优先）

群内 `/rules` 向所有成员（含匿名身份）回复当前群简介中的规则，无需参数；审核未启用时也可查询，群简介为空时明确提示。回复保留原话题。私聊 `/rules 群组ID` 仍需验证目标群管理员身份；群内额外参数不用于指定其他群，并继续接受普通审核。

群内或私聊 `/start`、`/help` 查看命令。其他管理操作均通过 Telegram `getChatMember` 检查操作者是否为**目标群组**的管理员，再读取该群的数据；A 群管理员不会因此获得 B 群权限。私聊必须指定负数群组 ID，群内命令只作用于当前群。

| 群内命令 | 私聊命令 | 功能 |
| --- | --- | --- |
| `/threshold 0.75` | `/threshold -100GROUP_ID 0.75` | 仅本群管理员设置删除阈值；省略数值查询，支持 `75%`；群内公开回复 |
| `/dry-run off`（或 `/dry_run off`） | `/dry_run -100GROUP_ID off` | 为当前群启用实际处罚；`on` 恢复审计并取消本群待封禁任务；省略参数查询 |
| `/logs off` | `/logs -100GROUP_ID off` | 只关闭当前群的管理员主动私聊日志；`on` 恢复；省略参数查询；不关闭群公告 |
| `/rules` | `/rules -100GROUP_ID` | 群内所有成员可读取本群规则；私聊仍限目标群管理员 |
| `/cancel USER_ID` | `/cancel -100GROUP_ID USER_ID` | 仅取消尚未进入处理阶段的自动封禁任务；**不自动解禁** |
| `/pending USER_ID` | `/pending -100GROUP_ID USER_ID` | 查看剩余取消期 |
| `/warnings USER_ID [0-3]` | `/warnings -100GROUP_ID USER_ID [0-3]` | 查询或设置指定用户在目标群近 7 天警告次数；仅目标群管理员可操作 |
| 回复消息 `/scan [audit\|enforce]` 或 `/scan MESSAGE_ID [audit\|enforce]` | `/scan_id -100GROUP_ID MESSAGE_ID [audit\|enforce]` | 单条扫描默认按规则执行处罚；追加 `audit` 才仅审计 |
| `/scan_all [audit\|enforce]` | `/scan_all -100GROUP_ID [audit\|enforce]` | 扫描机器人已归档的消息，默认按规则尝试处罚；`audit` 仅审计，`enforce` 显式执行 |
| `/scan_status` | `/scan_status -100GROUP_ID` | 查询进度 |
| `/scan_stop` | `/scan_stop -100GROUP_ID` | 停止扫描 |

`/scan`、`/scan_id` 单条扫描**默认请求执行处罚**；使用 `/scan audit` 明确仅审计，原 `enforce` 参数仍兼容。返回实际保存的审核状态、类别与概率。若本群开启 `dry-run`，仍不会真正删除/封禁，回复会提示模拟模式；同样保留群规、阈值、用户类型、48 小时删除窗口、权限与防重复警告检查。**批量 `/scan_all` 同样默认请求执行处罚**；追加 `audit` 才仅审计，`enforce` 仍可显式指定。若目标群开启 dry-run，则所有扫描都不会执行实际处罚。同群已有运行中的历史扫描时，再发 `/scan_all` 会收到冲突提示，不会重置原任务的模式和进度；可用 `/scan_status` 查询或 `/scan_stop` 停止后重启。不同群组的历史扫描任务相互独立。目标必须是当前群回复的消息或该群已归档的正整数消息 ID，不能跨群读取。缺少群规或模型失败不误报成功；服务异常保留队列重试。

`/warnings USER_ID` 仅查询当前群该用户近 7 天的有效警告次数；`/warnings USER_ID 2` 设为 2 次（允许 0–3），私聊需用 `/warnings -100GROUP_ID USER_ID 2`。**必须通过目标群实时管理员身份验证**，群内无法指定别的群，D1 所有读写均按 `(chat_id,user_id)` 隔离。调整不会删除原违规记录，也不会立刻禁言或封禁；调整后新违规继续累计，人工基数 7 天后过期。若存在该用户在该群尚处于 `pending` 的自动封禁，会通过原子数据库操作取消；若已经进入 `processing` 或已调用 Telegram，则无法原子撤销，回复会明确提示。`/cancel` 也只取消 `pending` 状态的任务：**不再调用 `getChatMember` 检查后自动解禁，也不会恢复群默认权限**。机器人本身设置的 60 秒禁言等待 Telegram 自行到期；如管理员另行延长或修改限制，应保留人工设置。若封禁 API 失败并准备重新排队，会再次检查是否已被警告调整覆盖，防止旧任务重新出现。

Telegram 官方 Bot API 无法遍历机器人加入前的消息、也无法任意拉取旧消息 ID。`tools/history-import/` 提供桌面版文本 JSON 离线导入工具，具体导入方式需由数据持有人授权。旧媒体不在导入范围内。Telegram 对大部分消息有 48 小时删除限制。

## 10 秒聚合与 Telegram 消息排版

Cloudflare `MODERATION_QUEUE` 配置为 `max_batch_timeout=10`、`max_batch_size=20`：消息最多等待 10 秒，或满 20 条时提前下发。每条仍**单独审核、单独处罚、单独确认或重试**，不同群组的数据与管理员收件人隔离；不会把不同消息正文合成一个 AI 请求。默认开启的管理员常规审核日志按群汇总为精简摘要，必要时拆分以遵守 Telegram 消息长度上限。编辑消息在同一批次内只显示最终状态。处罚公告与用户禁言、封禁不等待日志批处理。

所有发出的机器人消息使用 Telegram MarkdownV2 标题、图标和特殊字符转义；现存超级群消息链接形如 `https://t.me/c/<internal-group-id>/<message-id>`，删除后的消息只显示编号，不提供失效链接。用户提及使用由数字 ID 构造的链接，不允许原消息内容注入 Markdown。

## GitHub Actions 构建

推送到 `main` 会自动触发 `.github/workflows/build.yml`：下载 Rust stable 和 `wasm32-unknown-unknown` target、安装 `worker-build`、运行 SQLite/静态测试、使用 `Cargo.toml` 的 `lto="fat"` 构建 Wasm，并上传构建产物 Artifacts。无需在 GitHub 保存 BOT_TOKEN。

不提交依赖锁文件或固定补丁版本；每次构建按清单中的兼容版本范围解析依赖，`cargo install worker-build` 检查最新版本。CI 还会用 Node 24 执行实际构建的 Wasm，覆盖 Queue JSON 载荷、Webhook 鉴权及跨群管理权限。

构建 LTO 配置：

```toml
[profile.release]
opt-level = "s"
lto = "fat"
codegen-units = 1
strip = "debuginfo"
panic = "abort"
```

GitHub Workflow 只构建、测试并保存带 SHA-256 校验文件的产物；推送 main 或手动触发 Workflow 都不会部署 Cloudflare，不再使用 Actions 的 `CLOUDFLARE_API_TOKEN`、`CLOUDFLARE_ACCOUNT_ID`。发布改用已连接的 Cloudflare MCP。不要把 Bot Token 写入公开 Git 仓库或 Actions YAML。Cloudflare 端的 `BOT_TOKEN`、`WEBHOOK_SECRET` 等现有密钥保留。新建空 D1 仍用 `schema/current.sql` 一次性初始化；现有数据库需在部署前**手动**执行幂等的 `schema/ensure_warning_adjustments.sql`，建立 `warning_adjustments`、`warning_edit_receipts` 及 `apply_warning_edit_receipt` 触发器，保留原始违规历史。收据触发器原子应用后续警告调整和待封禁取消。还需按预检输出补齐扫描租约字段；扫描 ALTER 不可重复执行。GitHub Actions 不自动更改生产数据库。

首次启动所需资源（本地命令，或由有权限的 Cloudflare connector 创建）：

```sh
npx wrangler d1 create telegram-clef-moderation
npx wrangler queues create telegram-clef-moderation-events
npx wrangler queues create telegram-clef-ban-timers
npx wrangler queues create telegram-clef-moderation-dlq
npx wrangler d1 execute DB --remote --file schema/current.sql
```

上面 `schema/current.sql` **仅允许对新建空 D1 数据库执行一次**，绝不能用于已有数据的数据库。已运行旧版本的 D1 应先执行 `npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql` 或通过 Cloudflare MCP 执行该文件中的完整增量 SQL（包含收据触发器），并仅补齐预检输出的扫描 ALTER；不能只建立一张表就部署。已有收据表却缺少触发器时必须先人工对账，禁止直接补建后重放处罚。设置完 Cloudflare Secrets 与 Worker URL 后，用自己的终端注册 Webhook（`drop_pending_updates=false` 避免遗漏待处理更新）：

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
- **通知与隐私：** 默认私聊审核日志会披露群组 ID、消息 ID、群主显示名称及数字 ID 个人资料链接、状态、分类、置信度等元数据；处罚公告会披露违规消息 ID、类别以及适用时的用户 ID 和处罚状态，但不会转发原文。`/logs off` 不会删除已发送日志、不影响 D1 归档与保留期、不关闭 Worker 控制台日志或群内公告。机器人被移出群、无群内发言能力、无法获取管理员列表，或管理员未主动私聊/已屏蔽机器人时，对应通知可能无法送达。
- 本项目没有内置 Telegram UI，它通过 Telegram Bot API 和 Cloudflare Rust Workers 运行；请先做真实端到端误判、权限、取消期和通知送达测试。

## 每群设置和管理员日志

群内 `/dry-run off`（菜单别名 `/dry_run off`）仅为当前群开启实际处罚；`on` 恢复审计并取消该群待执行封禁。私聊使用 `/dry_run -100GROUP_ID off`。未设置时沿用全局 `DRY_RUN=true`；即使处于 dry-run，私人审核日志默认仍会发送尝试。各群独立设置。

日志默认开启，涵盖常规审核状态（包括放行、审计、dry-run）及部分处罚/扫描/取消状态；不承诺每一种运行异常都私聊推送。机器人每批通过 `getChatAdministrators` 查询本群当前人类管理员，跳过机器人账号；对方必须先主动私聊机器人（例如 `/start`），且 Telegram 允许私聊送达。群内 `/logs off` 关闭该群私人审核日志，`/logs on` 恢复；私聊使用 `/logs -100GROUP_ID off`。发送失败不会撤销已完成的审核或处罚。**全局 `ADMIN_DM_CHAT_ID` 已不参与收件人选择，留空也不会关闭默认私人日志。** `/logs off` 也不会阻止群内删除警告、待封禁或成功封禁通知及群内命令回复。

`/threshold` 删除阈值默认 0.93（93%），可用 `/threshold 0.75` 或 `/threshold 75%` 设置。模型的二元 `answers.violation.noul` 代表根据群简介判断的违规概率；**代码**负责判定：分数低于本群阈值自动标记 `allow`，达到阈值但类别为 `none` 则标记 `needs_review`，达到阈值且类别有效才进入删除流程。模型返回 `other` 或 `adult` 时会先规范化为 `none`，同样按阈值放行或人工复核，不会触发删除、警告或封禁。模型不再单独输出 `allow` 等最终动作。依然遵守群规、dry-run、48 小时删除窗口、权限和防重复处罚限制。模型概率未经严格校准，不能直接视为真实违规率。仅影响之后的审核，不自动重审旧消息或修改已有封禁任务。群内命令不可指定其他群，私聊必须验证目标群管理员身份。


被动审核摘要在每个 Queue 批次按群一次查询最终状态，不再逐消息读取；`/logs off` 时跳过这些查询。同群摘要的多个分段共用一次最新管理员查询。每条消息仍分别推理、归档、确认和重试。

## 通过 Cloudflare MCP 手动部署与群组导航

下载 main 成功构建的 Worker 产物，核对 SHA-256 和源提交；先记录当前 Worker 版本与 D1 Time Travel 恢复书签。通过已授权 Cloudflare MCP 执行 `python3 tools/check_d1_schema.py --print-inspection-sql` 打印的只读元数据及 PRAGMA 批次，将成功结果保存为 JSON，并用 `--from-json response.json --pre-migration` 校验。按输出补齐增量迁移，再去掉 `--pre-migration` 复查（必须退出 0），然后将原始 `index.js` 和二进制 `index_bg.wasm` 作为模块上传，不重新打包。保留现有绑定、变量、密钥、兼容性与可观测性配置，发布后核对活动版本和 `/healthz`。数据库内容、Queue 消费者、Cron 和 Telegram Webhook 配置保留。

管理员私聊摘要和处罚日志增加当前群名、群 ID 与可点击链接：公开群使用用户名链接；私有群优先使用已存在的 Telegram 邀请链接，否则链接到仍存在的已审核消息，供群成员进入。不会新建邀请链接。元数据不可用时回退到群 ID 和可用消息链接。群名安全转义，收件人仍只限目标群当前人类管理员。

管理员私聊摘要及独立日志还显示当前群主名称和 `tg://user?id=...` 个人资料链接，复用同一次 `getChatAdministrators` 查询，不增加 API 请求。群主名称安全转义；名称缺失时显示用户 ID，群主资料不可用时明确标注。摘要拆分预留群名与群主标题空间。

警告次数直接使用当前群、当前用户近 7 天的违规记录，达到三次后条形填满，超过三次仍显示实际次数。归档保留用户显示名称，供重审显示；匿名身份不归属具体用户次数。管理员日志的违规概率使用十格 Markdown 进度条及准确百分比；缺少群规或模型失败不展示虚构分数，省略空类别、重复群 ID、内部状态码与重复处罚说明。

同一次 Queue 消费内按群复用成功获取的群规；命令和成员变更会清空本批缓存，失败查询不缓存。若群简介在批次处理中被外部修改，缓存可能短暂保留旧值，直到失效或下一批次。模型推理、归档、确认和重试仍逐条顺序处理。


## 生产 D1 发布前校验

GitHub Actions 只构建测试，不修改生产 D1 或自动发布。发布 Worker 前，先确认生产数据库绑定及备份，在已授权操作机运行 `python3 tools/check_d1_schema.py --remote --pre-migration`。退出码 3 表示经过验证的旧版结构只缺少可安全添加的警告相关对象或扫描租约字段（包括 PR #11 之后缺少收据表和触发器的情况），此时手工运行 `schema/ensure_warning_adjustments.sql`；退出码 2 表示其它结构错误或校验失败，必须阻止发布。迁移后以及上传 Worker 前，再运行 `python3 tools/check_d1_schema.py --remote`，必须退出 0。不要在旧数据库运行完整 `schema/current.sql`。如果原版本已在 Telegram 删除后写入违规但查询失败，不能简单重放处罚，需要人工对账。详情见 [手工发布检查](docs/manual-d1-release.md)。

发布预检同时检查 `warning_edit_receipts` 和 `apply_warning_edit_receipt` 的结构及定义，并在本地临时 SQLite 数据库中验证触发器行为。已有收据表却缺少触发器时，可能存在已写入但未生效的命令，必须返回不兼容（退出码 2），禁止直接当作普通增量迁移。详情见 [迁移兼容性矩阵](docs/manual-d1-release.md)。

### 历史扫描的代次隔离与取消边界

历史扫描在 D1 中使用递增的 `generation` 与每次 Cron 原子领取的 `lease_id`。停止会使旧代次失效，立即以 `audit` 重启也不会接受旧执行模式任务的进度、普通历史审核结果或完成写入；重叠 Cron 无法领取同一任务。租约最长 20 分钟，异常中断后可自动恢复。完成通知包含代次，便于识别 Telegram 延迟送达的旧通知。

现有数据库必须先检查 `scan_jobs` 列，只执行预检打印的缺失 ALTER。`schema/ensure_scan_fencing.sql` 不幂等；部分执行后不要从头重跑。新数据库直接使用 `schema/current.sql`。不要在现有数据库上运行初始化脚本；CI 不修改生产 D1。

停止无法原子取消已经发往 Telegram 的删除/禁言请求。发生在停止之后才返回成功的外部操作仍会写入处罚事实记录，以避免重复处罚；但旧任务不得修改新扫描代次的游标与统计。

联合发布预检也要求扫描租约字段完整。`--pre-migration` 会打印尚缺少的扫描 `ALTER` 语句；只执行这些语句，不要重复运行已部分执行的扫描迁移。停止后已发出的禁言可能仍然完成，但过期扫描不能再创建待封禁任务。

警告编辑以 Telegram 更新 ID 去重。清理旧违规后，新违规 ID 仍高于有效人工基线，避免重用 rowid 导致漏计。不要独立重建或 VACUUM 违规表，除非同时保留其与警告基线的顺序关系。
