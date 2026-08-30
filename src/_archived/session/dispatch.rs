use crate::gpio::GPIOBackend;
use crate::gpio::GPIOCluster;
use crate::handlers::get::GetHandler;
use crate::handlers::init::InitHandler;
use crate::handlers::set::SetHandler;
use crate::protocol::request::RequestMessage;
use crate::protocol::request::RequestPayload;
use crate::protocol::response::ResponseMessage;
use crate::session::SessionContext;

pub async fn handle_request<B>(
    session: &mut SessionContext<B::Cluster>,
    backend: &B,
    request: RequestMessage,
) -> ResponseMessage
where
    B: GPIOBackend,
    B::Cluster: GPIOCluster,
{
    let request_id = request.id;
    match request.payload {
        RequestPayload::Init { target } => {
            InitHandler::handle(session, backend, &request_id, &target).await
        }
        RequestPayload::Get { target } => GetHandler::handle(session, &request_id, &target).await,
        RequestPayload::Set { target } => {
            SetHandler::default()
                .handle(session, &request_id, &target)
                .await
        }
    }
}
