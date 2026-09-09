pub fn validate_order(quantity: u32) -> bool {
    quantity > 0
}

pub fn calculate_subtotal(price: u64, quantity: u32) -> u64 {
    price * u64::from(quantity)
}

pub fn calculate_tax(subtotal: u64) -> u64 {
    subtotal * 8 / 100
}

pub fn calculate_shipping(weight: u32) -> u64 {
    500 + u64::from(weight) * 20
}

pub fn apply_discount(subtotal: u64, discount: u64) -> u64 {
    subtotal.saturating_sub(discount)
}

pub fn reserve_stock(available: u32, requested: u32) -> Option<u32> {
    available.checked_sub(requested)
}

pub fn release_stock(available: u32, returned: u32) -> u32 {
    available.saturating_add(returned)
}

pub fn validate_address(address: &str) -> bool {
    !address.trim().is_empty()
}

pub fn format_receipt(order_id: u64) -> String {
    format!("Order {order_id}")
}

pub fn select_shipping_service(express: bool) -> &'static str {
    if express { "priority" } else { "standard" }
}

pub fn estimate_delivery_days(express: bool) -> u32 {
    if express { 1 } else { 5 }
}

pub fn dispatch_order(order_id: u64) -> String {
    let receipt = format_receipt(order_id);
    format!("Dispatched: {receipt}")
}

pub fn cancel_order(order_id: u64) -> String {
    format!("Cancelled: {order_id}")
}

pub fn refund_order(total: u64, fee: u64) -> u64 {
    total.saturating_sub(fee)
}

pub fn archive_order(order_id: u64) -> String {
    format!("archive/{order_id}")
}

pub fn describe_international_shipping_restrictions(country: &str) -> String {
    format!("Check shipping restrictions for {country}")
}
