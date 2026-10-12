UPDATE chat_delivery SET unknown_reason = NULL
WHERE unknown_reason IS NOT NULL AND state != 'undelivered_unknown';

CREATE TABLE chat_delivery_new (
    room_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    window_id TEXT NOT NULL,
    state TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    unknown_reason TEXT,
    PRIMARY KEY (room_id, seq, window_id),
    CHECK (unknown_reason IS NULL OR state = 'undelivered_unknown')
);

INSERT INTO chat_delivery_new (room_id, seq, window_id, state, at_ms, unknown_reason)
SELECT room_id, seq, window_id, state, at_ms, unknown_reason FROM chat_delivery;

DROP TABLE chat_delivery;

ALTER TABLE chat_delivery_new RENAME TO chat_delivery;
