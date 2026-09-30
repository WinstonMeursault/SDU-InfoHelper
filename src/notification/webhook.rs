use super::{Alert, DeliveryReceipt, NotifyError, check_response};
use crate::settings::monitoring::WebhookSettings;
use reqwest::{
    blocking::Client,
    header::{HeaderName, HeaderValue},
};
use std::str::FromStr;

pub(super) fn send(
    client: &Client,
    config: &WebhookSettings,
    alert: &Alert,
) -> Result<DeliveryReceipt, NotifyError> {
    let mut request = client.post(&config.url).json(alert);
    for (name, value) in &config.headers {
        let name = HeaderName::from_str(name)
            .map_err(|_| NotifyError::new("Webhook header 无效。", false))?;
        let mut value = HeaderValue::from_str(value)
            .map_err(|_| NotifyError::new("Webhook header 无效。", false))?;
        value.set_sensitive(true);
        request = request.header(name, value);
    }
    let response = request.send().map_err(NotifyError::network)?;
    check_response(&response)?;
    Ok(DeliveryReceipt)
}
