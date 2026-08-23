-- generated 1787471012 for table `commission_dispute`
CREATE TABLE IF NOT EXISTS "commission_dispute" (id INTEGER PRIMARY KEY AUTOINCREMENT, commission_waterfall_entry_id INTEGER, original_amount_cents INTEGER, adjusted_amount_cents INTEGER, justification TEXT, status TEXT);
