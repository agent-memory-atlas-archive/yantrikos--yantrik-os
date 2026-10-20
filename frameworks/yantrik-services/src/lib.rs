//! The platform's services, as an app calls them (design/app-framework-2026-10-21.md, §5).
//!
//! One typed client per service domain. Every method blocks with a deadline of its own and
//! answers a typed [`ServiceError`]: unreachable, timed out, refused by the service with its
//! sentence, or an answer of a shape this client does not know. Each domain is a trait
//! (`weather::WeatherApi`), so the feature that calls it is tested against a fake with no
//! socket. App code calls these from a job, never the UI thread.

use std::time::Duration;

use serde::de::DeserializeOwned;
use yantrik_ipc_transport::{RpcError, SyncRpcClient};

pub mod weather;

/// Why a call to a service did not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    /// Nothing answers at the service's socket, and it could not be started.
    #[error("the {service} service is not running")]
    Unreachable { service: &'static str },
    /// It did not answer within the deadline.
    #[error("the {service} service did not answer within {seconds} s")]
    Timeout { service: &'static str, seconds: u64 },
    /// It answered, and would not: its own sentence.
    #[error("{sentence}")]
    Refused { code: i32, sentence: String },
    /// It answered with something this client does not read.
    #[error("the {service} service answered in a shape this app does not read: {what}")]
    WrongShape { service: &'static str, what: String },
}

impl ServiceError {
    /// Whether the person can do anything about it (start the service, fix the request) or the
    /// app should just say so and carry on.
    pub fn is_transient(&self) -> bool {
        matches!(self, ServiceError::Unreachable { .. } | ServiceError::Timeout { .. })
    }
}

/// One call to a service: started if it is not up, then the method with its deadline, and the
/// answer decoded into `T`.
pub(crate) fn call<T: DeserializeOwned>(
    service: &'static str,
    method: &str,
    params: serde_json::Value,
    deadline: Duration,
) -> Result<T, ServiceError> {
    if yantrik_ipc_transport::service::ensure(service).is_err() {
        return Err(ServiceError::Unreachable { service });
    }
    let answer = SyncRpcClient::for_service(service)
        .with_timeout(deadline)
        .call(method, params)
        .map_err(|e| classify(service, deadline, e))?;
    serde_json::from_value(answer).map_err(|e| ServiceError::WrongShape {
        service,
        what: shape_of(&e),
    })
}

/// The transport's error, told apart: a service that is not there, one that was slow, one that
/// said no.
fn classify(service: &'static str, deadline: Duration, e: RpcError) -> ServiceError {
    let text = e.message.to_ascii_lowercase();
    if text.contains("timed out") || text.contains("timeout") {
        ServiceError::Timeout { service, seconds: deadline.as_secs() }
    } else if text.contains("connection refused")
        || text.contains("no such file")
        || text.contains("not running")
        || text.contains("could not connect")
    {
        ServiceError::Unreachable { service }
    } else {
        ServiceError::Refused { code: e.code, sentence: e.message }
    }
}

/// A decode error by its kind and place, never quoting the value (a service answer may carry
/// anything).
fn shape_of(e: &serde_json::Error) -> String {
    use serde_json::error::Category;
    let kind = match e.classify() {
        Category::Io => "could not be read",
        Category::Syntax => "is not valid JSON",
        Category::Eof => "ends early",
        Category::Data => "has a value of the wrong kind",
    };
    if e.line() > 0 {
        format!("{kind} (line {}, column {})", e.line(), e.column())
    } else {
        kind.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(code: i32, message: &str) -> RpcError {
        RpcError { code, message: message.into(), data: None }
    }

    #[test]
    fn the_transports_errors_are_told_apart() {
        let d = Duration::from_secs(5);
        assert_eq!(
            classify("weather", d, rpc(-1, "request timed out after 5s")),
            ServiceError::Timeout { service: "weather", seconds: 5 }
        );
        assert_eq!(
            classify("weather", d, rpc(-1, "Connection refused (os error 111)")),
            ServiceError::Unreachable { service: "weather" }
        );
        assert_eq!(
            classify("weather", d, rpc(-32602, "Missing 'query' parameter")),
            ServiceError::Refused { code: -32602, sentence: "Missing 'query' parameter".into() }
        );
        assert!(ServiceError::Unreachable { service: "weather" }.is_transient());
        assert!(!ServiceError::Refused { code: 1, sentence: "no".into() }.is_transient());
        assert_eq!(
            ServiceError::Timeout { service: "weather", seconds: 5 }.to_string(),
            "the weather service did not answer within 5 s"
        );
    }

    #[test]
    fn a_wrong_shape_is_described_without_its_values() {
        let e = serde_json::from_value::<u32>(serde_json::json!("sk-SECRET")).unwrap_err();
        let said = shape_of(&e);
        assert!(!said.contains("SECRET"));
        assert!(said.contains("wrong kind"));
    }
}
