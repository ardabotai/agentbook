/// Returns the current time as milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_reasonable() {
        let ms = now_ms();
        // Should be after 2024-01-01 and before 2100-01-01
        assert!(ms > 1_704_067_200_000);
        assert!(ms < 4_102_444_800_000);
    }
}
