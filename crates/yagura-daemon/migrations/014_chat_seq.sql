CREATE TABLE chat_seq (
    room_id TEXT PRIMARY KEY,
    last_seq INTEGER NOT NULL
);

INSERT INTO chat_seq (room_id, last_seq)
SELECT room_id, MAX(seq) FROM chat_message GROUP BY room_id;
