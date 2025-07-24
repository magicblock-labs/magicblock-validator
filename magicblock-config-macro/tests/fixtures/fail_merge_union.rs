use magicblock_config_macro::Mergeable;

#[derive(Default, Mergeable)]
union TestUnion {
    a: u32,
    b: String,
}

fn main() {}
