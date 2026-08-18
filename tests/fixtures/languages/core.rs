mod outer {
    pub struct Nested;

    impl Nested {
        pub fn render(&self) {}
    }
}

fn duplicate() {}
fn duplicate() {}

fn wrapper() {
    struct Hidden;
    fn local() {}
    local();
}
