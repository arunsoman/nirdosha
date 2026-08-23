-- generated 1787471012 for table `invoice`
CREATE TABLE IF NOT EXISTS "invoice" (id INTEGER PRIMARY KEY AUTOINCREMENT, purchase_order_id INTEGER, invoice_number TEXT, tax_id TEXT, supplier_code TEXT, face_value_cents INTEGER, fraud_score_bps INTEGER, eligibility_status TEXT);
