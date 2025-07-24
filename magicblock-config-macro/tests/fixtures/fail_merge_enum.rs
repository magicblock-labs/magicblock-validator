use magicblock_config_macro::Mergeable;

#[derive(Default, Mergeable)]
enum TestEnum {
    A(u32),
    B(String),
}

fn main() {}
