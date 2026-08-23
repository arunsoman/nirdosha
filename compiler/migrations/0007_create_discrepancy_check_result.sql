-- generated 1787471012 for table `discrepancy_check_result`
CREATE TABLE IF NOT EXISTS "discrepancy_check_result" (id INTEGER PRIMARY KEY AUTOINCREMENT, letter_of_credit_id INTEGER, po_quantity INTEGER, presented_quantity INTEGER, quantity_status TEXT, po_party_name TEXT, presented_party_name TEXT, party_status TEXT, overall_status TEXT);
