-- generated 1787471012 for table `discounting_bundle`
CREATE TABLE IF NOT EXISTS "discounting_bundle" (id INTEGER PRIMARY KEY AUTOINCREMENT, invoice_id_a INTEGER, invoice_id_b INTEGER, invoice_id_c INTEGER, advance_rate_bps INTEGER, total_advance_cents INTEGER, interest_cents INTEGER, status TEXT);
