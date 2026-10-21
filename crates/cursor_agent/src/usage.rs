use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use futures::AsyncReadExt as _;
use gpui_util::ResultExt as _;
use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest, RequestTimeout};
use serde_json::Value;

use crate::{CURSOR_API_URL, Credentials, authenticated_request, parse_unary_payload};

const USAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The account's usage in the current billing cycle, as Cursor's dashboard reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CursorUsage {
    pub plan_name: Option<String>,
    pub billing_cycle_end: Option<SystemTime>,
    pub plan: Option<PlanUsage>,
    pub spend_limit: Option<SpendLimitUsage>,
}

/// Amounts are in cents and percentages run from 0 to 100.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanUsage {
    pub included_spend: f64,
    pub bonus_spend: f64,
    pub limit: f64,
    pub total_percent_used: f64,
    pub api_percent_used: f64,
    pub auto_percent_used: f64,
}

/// Amounts are in cents.
#[derive(Clone, Debug, PartialEq)]
pub struct SpendLimitUsage {
    pub used: f64,
    pub limit: f64,
}

impl SpendLimitUsage {
    pub fn percent_used(&self) -> f64 {
        (self.used / self.limit * 100.0).clamp(0.0, 100.0)
    }
}

impl CursorUsage {
    /// How much of the cycle's allowance is used, from 0 to 100, or `None` when the account
    /// has neither an included allowance nor a spend limit.
    pub fn percent_used(&self) -> Option<f64> {
        match (&self.plan, &self.spend_limit) {
            (Some(plan), _) => Some(plan.total_percent_used.clamp(0.0, 100.0)),
            (None, Some(spend_limit)) => Some(spend_limit.percent_used()),
            (None, None) => None,
        }
    }
}

/// Protobuf's JSON mapping writes 64-bit integers as strings, so numbers may arrive either way.
fn number(value: &Value, key: &str) -> Option<f64> {
    match value.get(key)? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
    .filter(|number: &f64| number.is_finite())
}

fn timestamp(value: &Value, key: &str) -> Option<SystemTime> {
    let millis = number(value, key).filter(|millis| *millis > 0.0)?;
    Some(UNIX_EPOCH + Duration::from_millis(millis as u64))
}

fn plan_usage(value: &Value) -> Option<PlanUsage> {
    if !value.is_object() {
        return None;
    }
    let included_spend = number(value, "includedSpend").unwrap_or(0.0);
    let limit = number(value, "limit").unwrap_or(0.0);
    let total_percent_used = number(value, "totalPercentUsed").unwrap_or_else(|| {
        if limit > 0.0 {
            (included_spend / limit * 100.0).min(100.0)
        } else {
            0.0
        }
    });
    Some(PlanUsage {
        included_spend,
        bonus_spend: number(value, "bonusSpend").unwrap_or(0.0),
        limit,
        total_percent_used,
        api_percent_used: number(value, "apiPercentUsed").unwrap_or(0.0),
        auto_percent_used: number(value, "autoPercentUsed").unwrap_or(0.0),
    })
}

fn spend_limit_usage(value: &Value) -> Option<SpendLimitUsage> {
    let limit = number(value, "individualLimit").filter(|limit| *limit > 0.0)?;
    let used = match number(value, "individualRemaining") {
        Some(remaining) => (limit - remaining).max(0.0),
        None => number(value, "totalSpend").unwrap_or(0.0),
    };
    Some(SpendLimitUsage { used, limit })
}

pub(crate) fn usage_from_payloads(period: &Value, plan_info: Option<&Value>) -> CursorUsage {
    let plan_info = plan_info.map(|info| info.get("planInfo").unwrap_or(info));
    CursorUsage {
        plan_name: plan_info
            .and_then(|info| info.get("planName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        billing_cycle_end: timestamp(period, "billingCycleEnd")
            .or_else(|| plan_info.and_then(|info| timestamp(info, "billingCycleEnd"))),
        plan: period.get("planUsage").and_then(plan_usage),
        spend_limit: period.get("spendLimitUsage").and_then(spend_limit_usage),
    }
}

async fn post_dashboard(
    http_client: &dyn HttpClient,
    credentials: &Credentials,
    method: &str,
) -> Result<Value> {
    let builder = HttpRequest::builder()
        .method(Method::POST)
        .uri(format!(
            "{CURSOR_API_URL}/aiserver.v1.DashboardService/{method}"
        ))
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1")
        .extension(RequestTimeout(USAGE_TIMEOUT));
    let request = authenticated_request(builder, credentials).body(AsyncBody::from("{}"))?;
    let mut response = http_client.send(request).await?;
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    if !response.status().is_success() {
        let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
        bail!("HTTP {} {}", response.status(), detail.trim());
    }
    parse_unary_payload(&body)
}

pub async fn fetch_usage(
    http_client: &dyn HttpClient,
    credentials: &Credentials,
) -> Result<CursorUsage> {
    let (period, plan_info) = futures::join!(
        post_dashboard(http_client, credentials, "GetCurrentPeriodUsage"),
        post_dashboard(http_client, credentials, "GetPlanInfo"),
    );
    let period = period.context("Failed to load Cursor usage")?;
    // The plan only supplies its name, so the usage still shows without it.
    let plan_info = plan_info.context("Failed to load Cursor plan").log_err();
    Ok(usage_from_payloads(&period, plan_info.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_plan_usage() {
        let period = json!({
            "billingCycleEnd": "1767225600000",
            "planUsage": {
                "includedSpend": 1234,
                "bonusSpend": "50",
                "limit": 2000,
                "totalPercentUsed": 61.7,
                "apiPercentUsed": 40,
                "autoPercentUsed": 21.7
            },
            "spendLimitUsage": { "individualLimit": "5000", "individualRemaining": 4000 }
        });
        let plan_info = json!({ "planInfo": { "planName": "Pro" } });
        let usage = usage_from_payloads(&period, Some(&plan_info));

        assert_eq!(usage.plan_name.as_deref(), Some("Pro"));
        assert_eq!(
            usage.billing_cycle_end,
            Some(UNIX_EPOCH + Duration::from_millis(1_767_225_600_000))
        );
        assert_eq!(
            usage.plan,
            Some(PlanUsage {
                included_spend: 1234.0,
                bonus_spend: 50.0,
                limit: 2000.0,
                total_percent_used: 61.7,
                api_percent_used: 40.0,
                auto_percent_used: 21.7,
            })
        );
        assert_eq!(
            usage.spend_limit,
            Some(SpendLimitUsage {
                used: 1000.0,
                limit: 5000.0,
            })
        );
        assert_eq!(usage.percent_used(), Some(61.7));
    }

    #[test]
    fn falls_back_to_the_spend_limit() {
        let period = json!({ "spendLimitUsage": { "individualLimit": 2000, "totalSpend": 500 } });
        let usage = usage_from_payloads(&period, None);

        assert_eq!(usage.plan, None);
        assert_eq!(usage.percent_used(), Some(25.0));
        assert_eq!(usage_from_payloads(&json!({}), None).percent_used(), None);
    }
}
