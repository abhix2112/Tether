//! `tether-sim` binary. Built in M1; this is the M0 scaffold.

fn banner() -> String {
    format!("tether-sim {} (scaffold)", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_the_binary() {
        assert!(banner().starts_with("tether-sim "));
    }
}
