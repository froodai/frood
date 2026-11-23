-- One bot for two installations (docs/34 W4): the labels and the other installation's Mini App,
-- as the channel's module says them, for the Mini App's switcher.
ALTER TABLE channel_about ADD COLUMN link JSONB;
