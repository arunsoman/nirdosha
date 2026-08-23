-- generated 1787471012 for table `exception_queue_item`
CREATE TABLE IF NOT EXISTS "exception_queue_item" (id INTEGER PRIMARY KEY AUTOINCREMENT, payment_ref TEXT, amount_cents INTEGER, source_feed TEXT, reason TEXT, status TEXT);
