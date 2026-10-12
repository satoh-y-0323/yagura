ALTER TABLE plan_task ADD COLUMN instruction_notice_state TEXT CHECK (instruction_notice_state IN ('pending', 'delivered', 'window_closed'));
ALTER TABLE plan_task ADD COLUMN instruction_notice_at INTEGER;
