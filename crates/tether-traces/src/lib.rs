//! AgentX/Weka trace loader and synthesizers (SPEC §6.4). Built in M1.

/// Token-block granularity of the AgentX/Weka `hash_ids` (SPEC ADR-5).
pub const TRACE_BLOCK_SIZE: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_block_size_matches_spec() {
        assert_eq!(TRACE_BLOCK_SIZE, 64);
    }
}
