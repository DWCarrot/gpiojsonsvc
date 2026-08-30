use crate::gpio::GPIOClusterError;
use crate::protocol::response::ResponseMessage;

pub fn cluster_error_response<E>(request_id: &str, error: E) -> ResponseMessage
where
    E: GPIOClusterError,
{
    ResponseMessage::error(request_id, error.to_string())
}
