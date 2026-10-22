-- docs/25 §3.2: a verdict is taken back by a verdict of its own, so history keeps both.
ALTER TABLE verdicts DROP CONSTRAINT verdicts_decision_check;
ALTER TABLE verdicts ADD CONSTRAINT verdicts_decision_check
    CHECK (decision IN ('accept', 'accept_with_edits', 'reject', 'defer', 'investigate', 'undo'));
