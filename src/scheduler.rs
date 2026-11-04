use std::time::Duration;

pub fn retry_delay(failed_ordinal: u8) -> Option<Duration> {
    match failed_ordinal {
        1..=5 => Some(Duration::from_secs(10 << (failed_ordinal - 1))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_bound_the_budget() {
        let got: Vec<_> = (1..=6)
            .map(|n| retry_delay(n).map(|d| d.as_secs()))
            .collect();
        assert_eq!(
            got,
            [Some(10), Some(20), Some(40), Some(80), Some(160), None]
        );
        assert!(retry_delay(0).is_none());
        assert!(retry_delay(7).is_none());
    }
}
