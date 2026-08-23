-- generated 1787471012 for table `approval`
CREATE TABLE IF NOT EXISTS "approval" (id INTEGER PRIMARY KEY AUTOINCREMENT, action_type TEXT, risk_tier TEXT, initiator_subject TEXT, sla_hours INTEGER, payload_note TEXT, status TEXT, required_eyes INTEGER);
