//! Fixed-origin email/SMS adapters. Credentials and provider bodies are never logged.
use crate::{Delivery, Provider, Receipt, Route, SendResult, Sender, read_private};
use anyhow::Result;
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use std::{io::Read, path::Path, time::Duration};

pub struct HttpSender {
    client: Client,
}
impl HttpSender {
    /// # Errors
    /// Returns HTTP client initialization errors. No requests are sent here.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .no_proxy()
                .build()?,
        })
    }
}
fn secret(path: &Path) -> Option<String> {
    let data = read_private(path).ok()?;
    if data.len() > 4096 {
        return None;
    }
    let text = String::from_utf8(data).ok()?.trim().to_owned();
    if text.is_empty() || text.bytes().any(|b| b.is_ascii_control()) {
        None
    } else {
        Some(text)
    }
}
fn body(response: Response) -> Option<Value> {
    let mut bytes = Vec::new();
    response.take(65_537).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 65_536 {
        None
    } else {
        serde_json::from_slice(&bytes).ok()
    }
}
fn email_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn sms_id(id: &str) -> bool {
    id.len() == 34
        && (id.starts_with("SM") || id.starts_with("MM"))
        && id[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn accepted(response: std::result::Result<Response, reqwest::Error>, email: bool) -> SendResult {
    let Ok(response) = response else {
        return SendResult::Unknown;
    };
    let status = response.status();
    if status.as_u16() == 429 {
        return SendResult::Retry;
    }
    if status.is_server_error() {
        return SendResult::Unknown;
    }
    if !status.is_success() {
        return SendResult::Rejected;
    }
    let Some(value) = body(response) else {
        return SendResult::Unknown;
    };
    let key = if email { "id" } else { "sid" };
    let Some(id) = value[key]
        .as_str()
        .filter(|s| if email { email_id(s) } else { sms_id(s) })
    else {
        return SendResult::Unknown;
    };
    SendResult::Accepted(id.into())
}
pub(crate) fn email_receipt(value: &Value, id: &str) -> Receipt {
    if value["id"] != id {
        return Receipt::Unknown;
    }
    match value["last_event"].as_str() {
        Some("delivered" | "opened" | "clicked") => Receipt::Delivered,
        Some("bounced" | "failed" | "canceled" | "complained" | "suppressed") => Receipt::Failed,
        Some("sent" | "queued" | "scheduled" | "delivery_delayed") => Receipt::Pending,
        _ => Receipt::Unknown,
    }
}
pub(crate) fn sms_receipt(value: &Value, id: &str, account: &str) -> Receipt {
    if value["sid"] != id || value["account_sid"] != account {
        return Receipt::Unknown;
    }
    match value["status"].as_str() {
        Some("delivered" | "read") => Receipt::Delivered,
        Some("failed" | "undelivered" | "canceled") => Receipt::Failed,
        Some("queued" | "accepted" | "sending" | "sent" | "scheduled") => Receipt::Pending,
        _ => Receipt::Unknown,
    }
}
impl Sender for HttpSender {
    fn send(&mut self, route: &Route, d: &Delivery) -> SendResult {
        let text = format!("{}; reminder={}", d.event.message(), d.generation);
        match &route.provider {
            Provider::Console => {
                println!("{text}");
                SendResult::Displayed
            }
            Provider::Resend {
                from,
                to,
                api_key_file,
            } => {
                let Some(key) = secret(api_key_file) else {
                    return SendResult::Rejected;
                };
                accepted(self.client.post("https://api.resend.com/emails").bearer_auth(key).header("Idempotency-Key",&d.id)
                    .json(&json!({"from":from,"to":[to],"subject":format!("Hypercarry {:?}: {:?}",d.event.kind.severity(),d.event.kind),"text":text})).send(),true)
            }
            Provider::Twilio {
                account_sid,
                from,
                to,
                auth_token_file,
            } => {
                let Some(key) = secret(auth_token_file) else {
                    return SendResult::Rejected;
                };
                accepted(
                    self.client
                        .post(format!(
                            "https://api.twilio.com/2010-04-01/Accounts/{account_sid}/Messages.json"
                        ))
                        .basic_auth(account_sid, Some(key))
                        .form(&[
                            ("From", from.as_str()),
                            ("To", to.as_str()),
                            ("Body", text.as_str()),
                        ])
                        .send(),
                    false,
                )
            }
        }
    }
    fn poll(&mut self, route: &Route, id: &str) -> Receipt {
        let response = match &route.provider {
            Provider::Console => return Receipt::Unknown,
            Provider::Resend { api_key_file, .. } => {
                if !email_id(id) {
                    return Receipt::Unknown;
                }
                let Some(key) = secret(api_key_file) else {
                    return Receipt::Unknown;
                };
                self.client
                    .get(format!("https://api.resend.com/emails/{id}"))
                    .bearer_auth(key)
                    .send()
            }
            Provider::Twilio {
                account_sid,
                auth_token_file,
                ..
            } => {
                if !sms_id(id) {
                    return Receipt::Unknown;
                }
                let Some(key) = secret(auth_token_file) else {
                    return Receipt::Unknown;
                };
                self.client.get(format!("https://api.twilio.com/2010-04-01/Accounts/{account_sid}/Messages/{id}.json")).basic_auth(account_sid,Some(key)).send()
            }
        };
        let Ok(response) = response else {
            return Receipt::Unknown;
        };
        if !response.status().is_success() {
            return Receipt::Unknown;
        }
        let Some(value) = body(response) else {
            return Receipt::Unknown;
        };
        match &route.provider {
            Provider::Resend { .. } => email_receipt(&value, id),
            Provider::Twilio { account_sid, .. } => sms_receipt(&value, id, account_sid),
            Provider::Console => Receipt::Unknown,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acceptance_never_counts_as_delivery_and_wrong_identity_is_refused() {
        assert_eq!(
            email_receipt(&json!({"id":"fixture","last_event":"sent"}), "fixture"),
            Receipt::Pending
        );
        assert_eq!(
            email_receipt(&json!({"id":"fixture","last_event":"delivered"}), "fixture"),
            Receipt::Delivered
        );
        assert_eq!(
            sms_receipt(
                &json!({"sid":"fixture","account_sid":"ACfixture","status":"sent"}),
                "fixture",
                "ACfixture"
            ),
            Receipt::Pending
        );
        assert_eq!(
            sms_receipt(
                &json!({"sid":"fixture","account_sid":"other","status":"delivered"}),
                "fixture",
                "ACfixture"
            ),
            Receipt::Unknown
        );
        assert!(!email_id("../../secrets"));
        assert!(!sms_id("SM../../secrets"));
    }
}
