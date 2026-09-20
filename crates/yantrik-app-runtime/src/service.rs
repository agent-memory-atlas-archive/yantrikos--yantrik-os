//! Reach a service that is only started when something needs it.
//!
//! The implementation moved to `yantrik_ipc_transport::service` and this module is the same three
//! functions under the name apps already call them by. It moved because the built-in companion
//! now reaches the calendar the way an app does, and the companion runs inside the shell process:
//! this crate pulls in Slint, and a mind should not have to link a widget toolkit to ask the
//! machine to start a service. The logic never needed anything from here — a `SyncRpcClient` and
//! a socket address — so it sits beside them instead.
//!
//! Nothing about the app-facing API changed. `service::client("calendar")` still asks the shell
//! to start the calendar and hands back a client for it.

pub use yantrik_ipc_transport::service::{client, ensure, is_up};
