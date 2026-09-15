//! Notifications service — in-memory notification store exposed via JSON-RPC.
//!
//! Methods:
//!   notifications.list        {}                                          -> Vec<Notification>
//!   notifications.add         { title, body, app_id?, icon?, urgency? }   -> Notification
//!   notifications.dismiss     { id }                                      -> ()
//!   notifications.dismiss_all {}                                          -> ()
//!   notifications.action      { id, action_id }                           -> ()

use std::sync::Mutex;

use chrono::Utc;
use yantrik_ipc_contracts::control_surface::{act_json, describe_json, Action, Param, View};
use yantrik_ipc_contracts::notifications::*;
use yantrik_service_sdk::prelude::*;

fn main() {
    ServiceBuilder::new("notifications")
        .handler(NotificationsHandler::new())
        .run();
}

struct NotificationsHandler {
    store: Mutex<Vec<Notification>>,
}

impl NotificationsHandler {
    fn new() -> Self {
        Self {
            store: Mutex::new(Vec::new()),
        }
    }
}

impl ServiceHandler for NotificationsHandler {
    fn service_id(&self) -> &str {
        "notifications"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        match method {
            "notifications.list" => {
                let store = self.store.lock().unwrap();
                Ok(serde_json::to_value(store.as_slice()).unwrap())
            }
            "notifications.add" => {
                let title = params["title"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'title' parameter".to_string(),
                    })?
                    .to_string();

                let body = params["body"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'body' parameter".to_string(),
                    })?
                    .to_string();

                let app_id = params["app_id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();

                let icon = params["icon"].as_str().map(|s| s.to_string());

                let urgency = match params["urgency"].as_str() {
                    Some("low") | Some("Low") => Urgency::Low,
                    Some("critical") | Some("Critical") => Urgency::Critical,
                    _ => Urgency::Normal,
                };

                let notification = Notification {
                    id: uuid7::uuid7().to_string(),
                    title,
                    body,
                    icon,
                    urgency,
                    source_app: app_id,
                    timestamp: Utc::now().to_rfc3339(),
                    actions: Vec::new(),
                };

                let mut store = self.store.lock().unwrap();
                store.push(notification.clone());
                Ok(serde_json::to_value(notification).unwrap())
            }
            "notifications.dismiss" => {
                let id = params["id"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'id' parameter".to_string(),
                    })?;

                let mut store = self.store.lock().unwrap();
                store.retain(|n| n.id != id);
                Ok(serde_json::Value::Null)
            }
            "notifications.dismiss_all" => {
                let mut store = self.store.lock().unwrap();
                store.clear();
                Ok(serde_json::Value::Null)
            }
            "notifications.action" => {
                let id = params["id"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'id' parameter".to_string(),
                    })?;

                let action_id = params["action_id"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'action_id' parameter".to_string(),
                    })?;

                let store = self.store.lock().unwrap();
                let notification = store.iter().find(|n| n.id == id);

                match notification {
                    Some(n) => {
                        if n.actions.iter().any(|a| a.id == action_id) {
                            tracing::info!(
                                notification_id = id,
                                action_id = action_id,
                                "Action triggered"
                            );
                            Ok(serde_json::Value::Null)
                        } else {
                            Err(ServiceError {
                                code: -32602,
                                message: format!("Action '{action_id}' not found on notification '{id}'"),
                            })
                        }
                    }
                    None => Err(ServiceError {
                        code: -32602,
                        message: format!("Notification '{id}' not found"),
                    }),
                }
            }
            // The agent-facing surface: what the machine is trying to tell the user, right now,
            // in one line and a small list — without opening the notification centre.
            "app.describe" => Ok(describe_json(
                "notifications",
                &self.describe_view(),
                &notification_actions(),
            )),
            "app.act" => self.act(&params),
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method}"),
            }),
        }
    }
}

impl NotificationsHandler {
    /// Everything the machine is currently trying to tell the user, newest first, with the
    /// urgent ones counted out front so a caller reading one line knows whether to look closer.
    fn describe_view(&self) -> View {
        let store = self.store.lock().unwrap();
        let total = store.len();
        let critical = store.iter().filter(|n| matches!(n.urgency, Urgency::Critical)).count();

        let summary = if total == 0 {
            "Notifications — nothing pending".to_string()
        } else if critical > 0 {
            format!("Notifications — {total} pending, {critical} critical")
        } else {
            format!("Notifications — {total} pending")
        };

        // Newest first: a notification list read top-down should start with what just happened.
        let items: Vec<serde_json::Value> = store
            .iter()
            .rev()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "title": n.title,
                    "body": n.body,
                    "app": n.source_app,
                    "urgency": urgency_str(&n.urgency),
                    "at": n.timestamp,
                })
            })
            .collect();

        View::new(summary)
            .with("count", total as i64)
            .with("critical", critical as i64)
            .with("notifications", serde_json::Value::Array(items))
    }

    /// Dispatch `app.act`. Dismissing clears what the user has seen; it is `standard`, not
    /// `dangerous` — a dismissed notification is a message read, not work destroyed.
    fn act(&self, params: &serde_json::Value) -> Result<serde_json::Value, ServiceError> {
        let action = params["action"].as_str().unwrap_or("").trim();
        let args = params.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        match action {
            "dismiss" => {
                let id = args["id"].as_str().ok_or_else(|| ServiceError {
                    code: -32602,
                    message: "`dismiss` needs argument `id`".to_string(),
                })?;
                let removed = {
                    let mut store = self.store.lock().unwrap();
                    let before = store.len();
                    store.retain(|n| n.id != id);
                    before - store.len()
                };
                if removed == 0 {
                    return Err(ServiceError {
                        code: -32602,
                        message: format!("no notification with id `{id}`"),
                    });
                }
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "dismissed": id }),
                    &self.describe_view(),
                ))
            }
            "dismiss_all" => {
                let cleared = {
                    let mut store = self.store.lock().unwrap();
                    let n = store.len();
                    store.clear();
                    n
                };
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "dismissed": cleared }),
                    &self.describe_view(),
                ))
            }
            "" => Err(ServiceError {
                code: -32602,
                message: "act needs a non-empty `action`".to_string(),
            }),
            other => Err(ServiceError {
                code: -32601,
                message: format!("unknown action `{other}`; this service offers: dismiss, dismiss_all"),
            }),
        }
    }
}

/// What the notifications service can be asked to do.
fn notification_actions() -> Vec<Action> {
    vec![
        Action::new("dismiss", "Dismiss one notification by id")
            .arg(Param::text("id").describe("The notification id, as shown in the notifications list")),
        Action::new("dismiss_all", "Dismiss every pending notification"),
    ]
}

/// The urgency as the short word the rest of the UI uses.
fn urgency_str(u: &Urgency) -> &'static str {
    match u {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::Critical => "critical",
    }
}
