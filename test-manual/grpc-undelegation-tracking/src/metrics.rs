use anyhow::{anyhow, Result};

#[derive(Debug, Default, Clone)]
pub struct MetricsSnapshot {
    pub pubkey_updates_count: u64,
    pub account_updates_count: u64,
}

pub async fn fetch_metrics(endpoint: &str) -> Result<MetricsSnapshot> {
    let response = reqwest::get(endpoint).await?;
    let text = response.text().await?;

    let pubkey_updates_count = parse_metric(
        &text,
        "mbv_transaction_subscription_pubkey_updates_count",
    )?;
    let account_updates_count = parse_metric(
        &text,
        "mbv_transaction_subscription_account_updates_count",
    )?;

    Ok(MetricsSnapshot {
        pubkey_updates_count,
        account_updates_count,
    })
}

pub fn parse_metric(text: &str, metric_name: &str) -> Result<u64> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with(metric_name) {
            let value_str =
                line.split_whitespace().last().ok_or_else(|| {
                    anyhow!("No value found for metric: {}", metric_name)
                })?;
            let value: f64 = value_str.parse().map_err(|e| {
                anyhow!("Failed to parse metric value '{}': {}", value_str, e)
            })?;
            return Ok(value as u64);
        }
    }
    Err(anyhow!("Metric not found: {}", metric_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_METRICS: &str = r#"
# HELP mbv_transaction_subscription_pubkey_updates_count Total pubkey updates
# TYPE mbv_transaction_subscription_pubkey_updates_count counter
mbv_transaction_subscription_pubkey_updates_count 42
# HELP mbv_transaction_subscription_account_updates_count Total account updates
# TYPE mbv_transaction_subscription_account_updates_count counter
mbv_transaction_subscription_account_updates_count 100
"#;

    #[test]
    fn test_parse_metric_pubkey_updates() {
        let result = parse_metric(
            SAMPLE_METRICS,
            "mbv_transaction_subscription_pubkey_updates_count",
        );
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_parse_metric_account_updates() {
        let result = parse_metric(
            SAMPLE_METRICS,
            "mbv_transaction_subscription_account_updates_count",
        );
        assert_eq!(result.unwrap(), 100);
    }

    #[test]
    fn test_parse_metric_not_found() {
        let result = parse_metric(SAMPLE_METRICS, "nonexistent_metric");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_metric_with_float() {
        let text = "some_metric 123.456";
        let result = parse_metric(text, "some_metric");
        assert_eq!(result.unwrap(), 123);
    }
}
