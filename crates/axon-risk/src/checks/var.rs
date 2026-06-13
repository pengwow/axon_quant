pub fn calculate_var(returns: &[f64], confidence: f64) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let mut sorted = returns.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((1.0 - confidence) * sorted.len() as f64) as usize;
    let index = index.min(sorted.len() - 1);
    // VaR is the loss at given confidence level; clamp to 0 if no loss
    (-sorted[index]).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var_basic() {
        let returns = vec![
            -0.05, -0.03, -0.01, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07,
        ];
        let var = calculate_var(&returns, 0.95);
        assert!(var > 0.0);
    }

    #[test]
    fn test_var_empty_returns() {
        let var = calculate_var(&[], 0.95);
        assert_eq!(var, 0.0);
    }

    #[test]
    fn test_var_all_positive() {
        let returns = vec![0.01, 0.02, 0.03, 0.04, 0.05];
        let var = calculate_var(&returns, 0.95);
        // No loss when all returns positive
        assert_eq!(var, 0.0);
    }

    #[test]
    fn test_var_all_negative() {
        let returns = vec![-0.05, -0.04, -0.03, -0.02, -0.01];
        let var = calculate_var(&returns, 0.95);
        assert_eq!(var, 0.05);
    }

    proptest::proptest! {
        #[test]
        fn prop_var_non_negative_for_non_empty(returns in proptest::collection::vec(-1.0f64..1.0f64, 1..100)) {
            let var = calculate_var(&returns, 0.95);
            // VaR should be non-negative (loss measure)
            assert!(var >= 0.0 || var.is_nan());
        }
    }
}
