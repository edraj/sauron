-- Why a refresh token was revoked, so a concurrent-refresh race can be told
-- apart from an actual replay.
--
-- Rotating refresh tokens treat "this token was already used" as theft and kill
-- the whole family. But two dashboard tabs share one token in localStorage and
-- refresh on the same 15-minute timer, so they race routinely: the loser looks
-- exactly like a replay, the family is revoked, and BOTH tabs are logged out —
-- including the winner, whose brand-new token had just been issued.
--
-- A grace window alone would be unsafe: within it, a token revoked by an
-- explicit logout would still be redeemable, defeating logout. Recording the
-- reason lets the grace apply only to rotation.
ALTER TABLE refresh_tokens ADD COLUMN revoked_reason TEXT;

-- Existing revoked rows predate the distinction. Leave them NULL: NULL never
-- qualifies for the grace window, so they fall through to replay detection,
-- which is the conservative direction.
