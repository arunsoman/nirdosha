-- generated 1787471012 for table `audit_log`
CREATE TABLE IF NOT EXISTS "audit_log" (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_type TEXT, entity_id INTEGER, actor_subject TEXT, action TEXT, note TEXT, prev_hash TEXT, hash TEXT);
