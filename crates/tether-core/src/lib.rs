//! Pure, deterministic decision logic for Tether (SPEC §6.1).
//!
//! Everything here is shared by the gateway and the simulator, so what is simulated is what ships.
//! Rules (SPEC §0.5): no I/O, no wall clock (time is passed in as [`Millis`]), no unseeded randomness.

/// Milliseconds on a caller-supplied clock. Core never reads a clock itself.
pub type Millis = u64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_is_plain_u64() {
        let t: Millis = 1_500;
        assert_eq!(t + 500, 2_000);
    }
}
