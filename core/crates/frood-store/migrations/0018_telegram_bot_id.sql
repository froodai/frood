-- The Telegram bot's id, as its module says: Mini App launch data is signed by Telegram over
-- it (docs/32 M2), so the core checks a launch without the bot's token.
ALTER TABLE channel_about ADD COLUMN telegram_bot_id BIGINT;
