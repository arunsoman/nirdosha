-- generated 1787471012 for table `virtual_iban`
CREATE TABLE IF NOT EXISTS "virtual_iban" (id INTEGER PRIMARY KEY AUTOINCREMENT, buyer_counterparty_id INTEGER, seller_counterparty_id INTEGER, iban_number TEXT, partner_bank_note TEXT, status TEXT);
