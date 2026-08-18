use tonic::{Request, transport::Channel};
use aura_api_client::client::AuraClients;
use aura_api_client::types::Ping;
use aura_api_client::client_ext::UserCtxInterceptor;

#[derive(Clone, Copy)]
struct NoCtx;
impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), _req: &mut Request<T>) -> Result<(), tonic::Status> { Ok(()) }
}

#[tokio::main]
async fn main() {
    let pnl: fastnum::UD128 = fastnum::UD128::from(1u32);
    let _x = pnl.dummy_method_to_trigger_error();
    
    let val: decisol::Wsol = decisol::Wsol::from(1u64);
    let _y = val.dummy_method_to_trigger_error();
}
