ALTER TABLE videos ADD COLUMN is_short INTEGER NOT NULL DEFAULT 0;
-- ss で始まる既存動画をショートに設定
UPDATE videos SET is_short = 1 WHERE id LIKE 'ss%';
