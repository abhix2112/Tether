//! `tether` binary. Built in M3; this is the M0 scaffold.

fn banner() -> String {
    format!("tether {} (scaffold)", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_the_binary() {
        assert!(banner().starts_with("tether "));
    }
}
