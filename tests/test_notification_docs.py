"""Static guardrails for bilingual documentation of Telegram notification behavior.

These inspect source defaults and send paths rather than treating either README
as the authority. They run in the existing Python unittest discovery CI step.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]


class NotificationDocumentationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        def read(path):
            return (ROOT / path).read_text(encoding="utf-8")

        cls.en = read("README.md")
        cls.cn = read("README_CN.md")
        cls.store = read("src/store.rs")
        cls.telegram = read("src/tg.rs")
        cls.moderation = read("src/moderation.rs")
        cls.schema = read("schema/current.sql")

    def test_private_log_default_matches_current_schema_and_both_readmes(self):
        function = re.search(
            r"pub async fn group_logs\b.*?\{(.*?)\n\}",
            self.store, re.DOTALL,
        )
        self.assertIsNotNone(function, "Cannot locate group_logs default")
        fallback = re.search(r"\.unwrap_or\(\s*(true|false)\s*\)", function.group(1))
        self.assertIsNotNone(fallback, "Cannot determine group_logs fallback")
        default_enabled = fallback.group(1) == "true"

        column = re.search(
            r"\blogs_enabled\s+INTEGER\s+NOT\s+NULL\s+DEFAULT\s+([01])\b",
            self.schema, re.IGNORECASE,
        )
        self.assertIsNotNone(column, "Cannot determine D1 logs_enabled default")
        self.assertEqual(int(column.group(1)), int(default_enabled),
                         "Unconfigured group and newly inserted group defaults differ")

        expected = (
            ("**Private administrator logs are on by default.**",
             "**管理员私人审核日志默认开启。**")
            if default_enabled else
            ("**Private administrator logs are off by default.**",
             "**管理员私人审核日志默认关闭。**")
        )
        unexpected = (
            ("**Private administrator logs are off by default.**",
             "**管理员私人审核日志默认关闭。**")
            if default_enabled else
            ("**Private administrator logs are on by default.**",
             "**管理员私人审核日志默认开启。**")
        )
        for doc, required, forbidden in zip((self.en, self.cn), expected, unexpected):
            self.assertIn(required, doc)
            self.assertNotIn(forbidden, doc)

    def test_private_log_recipients_and_log_switch_are_documented(self):
        delivery = self.telegram.split(
            "async fn send_admin_markdown(", 1
        )[1].split("pub async fn send_to_review_or_chat(", 1)[0]
        self.assertIn("group_logs(env,chat_id)", delivery)
        self.assertIn("getChatAdministrators", delivery)
        self.assertIn('member["user"]["is_bot"]==true', delivery)
        self.assertIn("post_markdown(env,user_id,", delivery)
        self.assertIn("send_admin_markdown(env,chat_id,", self.telegram)
        self.assertNotIn("ADMIN_DM_CHAT_ID",
                         self.store + self.telegram + self.moderation)
        for doc in (self.en, self.cn):
            self.assertIn("`/logs off`", doc)
            self.assertIn("`/logs on`", doc)
            self.assertIn("`ADMIN_DM_CHAT_ID`", doc)
            self.assertIn("`getChatAdministrators`", doc)
            self.assertIn("`/start`", doc)
        self.assertIn("current human administrators", self.en)
        self.assertIn("当前人类管理员", self.cn)

    def test_group_notices_remain_independent_of_private_logs(self):
        sender = self.telegram.split(
            "pub async fn reply_command(", 1
        )[1].split("async fn send_admin_markdown(", 1)[0]
        self.assertIn('api(env,"sendMessage",args)', sender)
        self.assertIn("post_markdown(env,chat_id,", sender)
        self.assertIn("send_group_action_notice(", sender)
        self.assertNotIn("group_logs", sender)
        self.assertIn('"parse_mode":"MarkdownV2"', self.telegram)
        review = self.moderation.split("pub async fn review(", 1)[1].split(
            "fn command_parts(", 1
        )[0]
        self.assertIn("tg::delete(env, chat_id, msg_id).await?;", review)
        self.assertIn("tg::send_group_action_notice(env,chat_id,msg_id,notice_user.as_ref(),", review)
        self.assertIn("tg::ban(env,chat_id,user_id).await", self.moderation)
        self.assertIn("tg::send_group_action_notice(env,chat_id,message_id,Some(&user),", self.moderation)
        self.assertIn("tg::reply_command_titled(env,msg,tone,title,text).await", self.moderation)
        self.assertIn("reply_msg(env,msg,", self.moderation)
        self.assertNotIn("tg::send(env,origin_chat_id,", self.moderation)
        self.assertIn("does not disable group action notices", self.en)
        self.assertIn("不关闭群内处罚通知", self.cn)
        self.assertIn("`/threshold`", self.en)
        self.assertIn("`/threshold`", self.cn)

    def test_binary_model_and_queue_batch_scope(self):
        import json
        config=json.loads((ROOT / "wrangler.jsonc").read_text())
        moderation=next(q for q in config["queues"]["consumers"]
                        if q["queue"]=="telegram-clef-moderation-events")
        self.assertEqual(moderation["max_batch_size"],20)
        self.assertEqual(moderation["max_batch_timeout"],10)
        self.assertIn('"violation": {',self.moderation)
        self.assertIn('"type": "noul"',self.moderation)
        questions=self.moderation.split("fn moderation_questions() -> Value",1)[1].split(
            "async fn classify(",1
        )[0]
        self.assertNotIn('"action": {',questions)
        self.assertIn("last_review_log", self.store)
        self.assertNotIn("send_to_review_or_chat(env, chat_id, &format!(",self.store)
        self.assertIn("10 秒",self.cn)
        self.assertIn("10 seconds",self.en)

    def test_permissions_privacy_and_security_claims(self):
        for doc in (self.en, self.cn):
            for prerequisite in ("can_delete_messages", "can_restrict_members",
                                 "sendMessage", "getChatAdministrators", "DRY_RUN=true"):
                self.assertIn(prerequisite, doc)
        for section in ("## Notifications", "## Administrator commands",
                        "## Configuration and deployment", "## Security and privacy"):
            self.assertIn(section, self.en)
        for obsolete in (
            "Silent by design",
            "No group notification privileges required",
            "No group warning messages",
            "the *only* unsolicited notification target",
        ):
            self.assertNotIn(obsolete, self.en)
        self.assertIn("D1 archiving", self.en)
        self.assertIn("D1 归档", self.cn)


if __name__ == "__main__":
    unittest.main()

