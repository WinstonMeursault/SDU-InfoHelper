use super::{Alert, DeliveryReceipt, NotifyError, check_response};
use crate::settings::monitoring::PushDeerSettings;
use reqwest::blocking::Client;
use serde_json::Value;
use std::io::Read;

const MAX_RESPONSE: u64 = 65536;

pub(super) fn send(
    client: &Client,
    config: &PushDeerSettings,
    alert: &Alert,
) -> Result<DeliveryReceipt, NotifyError> {
    let endpoint = format!("{}/message/push", config.endpoint.trim_end_matches('/'));
    let message = match &alert.location {
        Some(location) => format!("{}\n{}", location.label(), alert.message),
        None => alert.message.clone(),
    };
    let response = client
        .post(endpoint)
        .form(&[
            ("pushkey", config.pushkey.as_str()),
            ("text", alert.title.as_str()),
            ("desp", message.as_str()),
            ("type", "text"),
        ])
        .send()
        .map_err(NotifyError::network)?;
    check_response(&response)?;
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| NotifyError::new("无法读取 PushDeer 响应。", true))?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(NotifyError::new("PushDeer 响应超出大小限制。", true));
    }
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|_| NotifyError::new("PushDeer 响应格式无效。", true))?;
    validate_result(&payload)?;
    Ok(DeliveryReceipt)
}

fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn successful_result(value: &Value) -> bool {
    // Official Apple push results are JSON-encoded strings with a positive counts field.
    if let Some(text) = value.as_str() {
        return serde_json::from_str::<Value>(text).is_ok_and(|value| successful_result(&value));
    }
    if let Some(counts) = value.get("counts").and_then(integer) {
        return counts > 0;
    }
    value.get("success").and_then(Value::as_bool) == Some(true)
        || value.get("code").and_then(integer) == Some(0)
}

fn validate_result(payload: &Value) -> Result<(), NotifyError> {
    if payload.get("code").and_then(integer) != Some(0) {
        // Do not echo error messages: servers may reflect the submitted PushKey.
        return Err(NotifyError::new(
            "PushDeer 未接受推送，请检查 PushKey 和设备配置。",
            payload.get("code").and_then(integer) == Some(80502),
        ));
    }
    let results = payload.pointer("/content/result").and_then(Value::as_array);
    if !results.is_some_and(|results| results.iter().any(successful_result)) {
        return Err(NotifyError::new(
            "PushDeer 未返回成功推送结果，请检查接收设备。",
            true,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_positive_device_results_only() {
        for payload in [
            json!({"code":0,"content":{"result":["{\"counts\":1}"]}}),
            json!({"code":"0","content":{"result":[{"counts":0},{"counts":1}]}}),
        ] {
            assert!(validate_result(&payload).is_ok());
        }
        for payload in [
            json!({"code":1,"error":"test-secret"}),
            json!({"code":0,"content":{"result":[]}}),
            json!({"code":0,"content":{"result":["{\"counts\":0}"]}}),
            json!({"code":0,"content":{"result":["not-json"]}}),
            json!({"code":0}),
        ] {
            let error = validate_result(&payload).unwrap_err();
            assert!(!error.to_string().contains("test-secret"));
        }
    }
}
