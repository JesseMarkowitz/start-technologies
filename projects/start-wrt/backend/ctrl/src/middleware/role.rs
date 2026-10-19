//! The role gate (design D7): an adopted access point serves a short list of
//! methods and refuses the rest. Nothing the Core controls is reachable on
//! the access point, not even to read.

use std::borrow::Cow;

use axum::extract::Request;
use axum::response::Response;
use rpc_toolkit::yajrc::{RpcError, RpcMethod};
use rpc_toolkit::{Context, Middleware, RpcRequest, RpcResponse};
use serde::Deserialize;

use crate::auth::error_code;
use crate::satellite::{self, RouterRole};

/// What an access point answers: its role and state, the adoption code, its
/// firmware, and a restart.
pub const ALLOWED: &[&str] = &[
    "satellite.get-role",
    "satellite.status",
    "satellite.adoption.code",
    "system.info",
    "system.restart",
    "auth.logout",
];

#[derive(Default, Deserialize, visit_rs::VisitFields)]
pub struct NoMetadata {}
rpc_toolkit::reflect_ts!(NoMetadata);

#[derive(Clone, Default)]
pub struct RoleGate;

fn is_access_point() -> bool {
    let role = satellite::load_role();
    role.role == RouterRole::Satellite && role.core_key.is_some()
}

pub fn allowed(method: &str) -> bool {
    ALLOWED.contains(&method)
}

impl<C: Context> Middleware<C> for RoleGate {
    type Metadata = NoMetadata;

    async fn process_http_request(&mut self, _: &C, _: &mut Request) -> Result<(), Response> {
        Ok(())
    }

    async fn process_rpc_request(
        &mut self,
        _: &C,
        _: Self::Metadata,
        request: &mut RpcRequest,
    ) -> Result<(), RpcResponse> {
        if allowed(request.method.as_str()) || !is_access_point() {
            return Ok(());
        }
        Err(RpcResponse::from(RpcError {
            code: error_code::AUTHORIZATION,
            message: Cow::Borrowed("not available on an access point; use its Core"),
            data: None,
        }))
    }

    async fn process_rpc_response(&mut self, _: &C, _: &mut RpcResponse) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_is_read_only_plus_restart() {
        assert!(allowed("satellite.status"));
        assert!(allowed("system.restart"));
        assert!(!allowed("wifi.set"));
        assert!(!allowed("profiles.list"));
        assert!(!allowed("satellite.unpair"));
    }
}
