pub fn dispatch_order(order_id: u64) -> String {
    format!("Legacy dispatch: {order_id}")
}

pub fn validate_order(quantity: u32) -> bool {
    quantity >= 10
}
