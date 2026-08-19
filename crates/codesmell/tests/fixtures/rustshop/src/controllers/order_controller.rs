pub struct OrderController;

impl OrderController {
    // async method not named *Async + too many parameters (6 > 4) +
    // calls the repository directly (controller -> repository denied).
    pub async fn place_order(
        &self,
        repo: &OrderRepo,
        a: i32,
        b: i32,
        c: i32,
        d: i32,
        e: i32,
        f: i32,
    ) -> i32 {
        repo.save_order(1);
        repo.save_order(2);
        repo.save_order(3);
        0
    }
}
