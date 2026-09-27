//! Account model catalogue and quota usage, both served by the management host
//! as AWS JSON calls, the same way the official Kiro CLI reads them.

use serde_json::{json, Value};
use std::time::Duration;

use crate::auth::KiroAuth;
use crate::utils::{ide_user_agent, kiro_headers};

fn region(auth: &KiroAuth) -> String {
    let arn = auth.profile_arn().unwrap_or_default();
    let parts: Vec<&str> = arn.split(':').collect();
    if parts.len() >= 4 && parts[2] == "codewhisperer" && !parts[3].is_empty() {
        return parts[3].to_owned();
    }
    auth.api_region.clone()
}

async fn management_call(
    auth: &KiroAuth,
    http: &reqwest::Client,
    target: &str,
    label: &str,
    mut body: Value,
    extra: &[(&str, &str)],
) -> Result<Value, String> {
    let token = auth.access_token().await.map_err(|e| e.to_string())?;
    let arn = auth.request_profile_arn().or_else(|| auth.profile_arn());
    let mut params: Vec<(String, String)> = vec![("origin".into(), "AI_EDITOR".into())];
    for (k, v) in extra {
        params.push(((*k).into(), (*v).into()));
    }
    if let Some(a) = &arn {
        params.push(("profileArn".into(), a.clone()));
        body["profileArn"] = json!(a);
    }
    let mut req = http
        .post(format!("https://management.{}.kiro.dev/", region(auth)))
        .timeout(Duration::from_secs(20))
        .query(&params)
        .json(&body);
    for (k, v) in kiro_headers(&token) {
        let v = match k {
            "x-amz-target" => target.to_owned(),
            "User-Agent" => ide_user_agent(label),
            _ => v,
        };
        req = req.header(k, v);
    }
    let resp = req
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("management host answered {}", status.as_u16()));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

pub async fn fetch_available_models(auth: &KiroAuth, http: &reqwest::Client) -> Option<Vec<Value>> {
    match management_call(
        auth,
        http,
        "KiroControlPlaneBearerService.ListAvailableModels",
        "kirocontrolplanebearer",
        json!({"origin": "AI_EDITOR"}),
        &[],
    )
    .await
    {
        Ok(v) => {
            let models: Vec<Value> = v
                .get("models")?
                .as_array()?
                .iter()
                .filter(|m| {
                    m.get("modelId")
                        .is_some_and(|x| x.as_str().is_some_and(|s| !s.is_empty()))
                })
                .cloned()
                .collect();
            (!models.is_empty()).then_some(models)
        }
        Err(e) => {
            tracing::debug!("[Models] Could not list models: {e}");
            None
        }
    }
}

fn number(v: Option<&Value>) -> Option<f64> {
    v.filter(|x| x.is_number()).and_then(Value::as_f64)
}

pub async fn fetch_account_usage(
    auth: &KiroAuth,
    http: &reqwest::Client,
    stored_arn: Option<String>,
) -> Result<Value, String> {
    if auth.request_profile_arn().is_none() && stored_arn.is_none() {
        return Err("profile ARN is not available yet for this account".into());
    }
    let payload = management_call(
        auth,
        http,
        "AmazonCodeWhispererService.GetUsageLimits",
        "codewhispererruntime",
        json!({"origin": "AI_EDITOR", "isEmailRequired": true}),
        &[("isEmailRequired", "true")],
    )
    .await?;
    let breakdowns = payload
        .get("usageBreakdownList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let b = breakdowns
        .iter()
        .find(|e| e.get("resourceType").and_then(Value::as_str) == Some("AGENTIC_REQUEST"))
        .or(breakdowns.first())
        .cloned()
        .unwrap_or(json!({}));
    let sub = payload
        .get("subscriptionInfo")
        .cloned()
        .unwrap_or(json!({}));
    let over = payload
        .get("overageConfiguration")
        .cloned()
        .unwrap_or(json!({}));
    let current = number(b.get("currentUsageWithPrecision")).or(number(b.get("currentUsage")));
    let limit = number(b.get("usageLimitWithPrecision")).or(number(b.get("usageLimit")));
    let pick = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(json!({
        "email": pick(payload.pointer("/userInfo/email")),
        "subscriptionTitle": pick(sub.get("subscriptionTitle")).or(pick(sub.get("type"))).unwrap_or("Unknown".into()),
        "subscriptionType": pick(sub.get("type")).unwrap_or("Unknown".into()),
        "resourceType": pick(b.get("resourceType")).unwrap_or("AGENTIC_REQUEST".into()),
        "currentUsage": current,
        "usageLimit": limit,
        "usagePercent": match (current, limit) { (Some(c), Some(l)) if l > 0.0 => Some(c / l * 100.0), _ => None },
        "unit": pick(b.get("unit")).unwrap_or_default(),
        "overageStatus": pick(over.get("overageStatus")).unwrap_or("UNKNOWN".into()),
        "overageUsed": number(b.get("currentOveragesWithPrecision")).or(number(b.get("currentOverages"))),
        "overageRate": number(b.get("overageRate")),
        "nextDateReset": payload.get("nextDateReset").cloned().unwrap_or(Value::Null),
        "daysUntilReset": payload.get("daysUntilReset").cloned().unwrap_or(Value::Null),
    }))
}
