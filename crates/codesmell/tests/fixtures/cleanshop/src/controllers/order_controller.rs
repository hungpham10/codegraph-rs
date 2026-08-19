pub struct OrderController;

impl OrderController {
    pub fn place_order(&self, svc: &OrderService, id: i32) -> i32 {
        svc.place(id)
    }
}
