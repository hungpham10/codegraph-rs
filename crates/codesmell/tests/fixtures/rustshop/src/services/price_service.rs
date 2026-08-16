pub struct PriceService;

impl PriceService {
    // Long business-logic method (>30 lines) with a caller in tests,
    // so it is exercised but also flagged for length.
    pub fn compute_big(&self, base: i32) -> i32 {
        let mut total = base;
        total += 1;
        total += 2;
        total += 3;
        total += 4;
        total += 5;
        total += 6;
        total += 7;
        total += 8;
        total += 9;
        total += 10;
        total += 11;
        total += 12;
        total += 13;
        total += 14;
        total += 15;
        total += 16;
        total += 17;
        total += 18;
        total += 19;
        total += 20;
        total += 21;
        total += 22;
        total += 23;
        total += 24;
        total += 25;
        total += 26;
        total += 27;
        total += 28;
        total += 29;
        total += 30;
        total += 31;
        total += 32;
        total
    }

    // Business logic (>20 lines) with NO test reference -> missing test.
    pub fn unreached_logic(&self, base: i32) -> i32 {
        let mut total = base;
        total += 1;
        total += 2;
        total += 3;
        total += 4;
        total += 5;
        total += 6;
        total += 7;
        total += 8;
        total += 9;
        total += 10;
        total += 11;
        total += 12;
        total += 13;
        total += 14;
        total += 15;
        total += 16;
        total += 17;
        total += 18;
        total += 19;
        total += 20;
        total += 21;
        total
    }
}
