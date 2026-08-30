use std::collections::BTreeMap;
use std::collections::btree_map;
use std::sync::Arc;

use crate::gpio::GPIOBackend;
use crate::gpio::GPIOClusterError;
use crate::gpio::GPIOPinConfig;
use crate::handlers::response::cluster_error_response;
use crate::protocol::request::BiasMode;
use crate::protocol::request::DriveMode;
use crate::protocol::request::PinSelector;
use crate::protocol::request::TargetConfigRequest;
use crate::protocol::response::ResponseMessage;
use crate::session::context::SessionContext;

#[derive(Debug, Default)]
pub struct InitHandler;

impl InitHandler {
    pub async fn handle<B>(
        session: &mut SessionContext<B::Cluster>,
        backend: &B,
        request_id: &str,
        target: &BTreeMap<String, TargetConfigRequest>,
    ) -> ResponseMessage
    where
        B: GPIOBackend,
        B::Error: GPIOClusterError,
    {
        if let Err(error) = session.ensure_can_init() {
            return error.into_response(request_id);
        }

        let mut pins_with_config = Vec::with_capacity(target.len() * 2);
        // TODO: other checks can be done here
        for (target_name, target_config) in target {
            match target_config {
                TargetConfigRequest::Input { pin, bias } => {
                    let cfg = GPIOPinConfig::Input {
                        bias: bias.unwrap_or(BiasMode::AsIs),
                    };
                    match pin {
                        PinSelector::Single(p) => {
                            pins_with_config.push((p.as_str(), cfg));
                        }
                        PinSelector::Combined(pins) => {
                            for p in pins.iter() {
                                pins_with_config.push((p.as_str(), cfg.clone()));
                            }
                        }
                    }
                }
                TargetConfigRequest::Output { pin, drive } => {
                    let cfg = GPIOPinConfig::Output {
                        drive: drive.unwrap_or(DriveMode::PushPull),
                    };
                    match pin {
                        PinSelector::Single(p) => {
                            pins_with_config.push((p.as_str(), cfg));
                        }
                        PinSelector::Combined(pins) => {
                            for p in pins.iter() {
                                pins_with_config.push((p.as_str(), cfg.clone()));
                            }
                        }
                    }
                }
                TargetConfigRequest::Trigger { pin, edge } => {
                    let cfg = GPIOPinConfig::Trigger { edge: *edge };
                    pins_with_config.push((pin.as_str(), cfg));
                }
            }
        }

        let cluster = match backend.cluster(pins_with_config.into_iter()).await {
            Ok(cluster) => cluster,
            Err(error) => return cluster_error_response(request_id, error),
        };

        let cache = match TargetsCache::build(&cluster, target) {
            Ok(cache) => cache,
            Err(error) => return error.into_response(request_id),
        };

        session.set_initialized(Arc::new(InitializedSession {
            init_request_id: request_id.to_owned(),
            cluster,
            cache,
        }));
        ResponseMessage::ok(request_id)
    }
}
