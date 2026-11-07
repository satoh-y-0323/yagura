CREATE TABLE delivery_marks (
    window_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (window_id, body)
);
CREATE TRIGGER windows_take_their_delivery_marks AFTER DELETE ON windows
BEGIN
    DELETE FROM delivery_marks WHERE window_id = OLD.window_id;
END;
