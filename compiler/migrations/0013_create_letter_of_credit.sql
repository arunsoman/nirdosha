-- generated 1787471012 for table `letter_of_credit`
CREATE TABLE IF NOT EXISTS "letter_of_credit" (id INTEGER PRIMARY KEY AUTOINCREMENT, purchase_order_id INTEGER, lc_number TEXT, status TEXT, amount_cents INTEGER, currency TEXT);
