use std::env;
use tonic::{Request, Status};
use tonic::transport::Endpoint;
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtxInterceptor;
use aura_api_client::types::FetchFullWalletsInfoReq;

#[derive(Clone, Copy)]
struct NoCtx;
impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), _req: &mut tonic::Request<T>) -> Result<(), tonic::Status> { Ok(()) }
}

static API_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn auth_interceptor(mut request: Request<()>) -> Result<Request<()>, Status> {
    if let Some(key) = API_KEY.get() {
        if let Ok(val) = key.parse::<tonic::metadata::MetadataValue<_>>() {
            request.metadata_mut().insert("auth", val);
        }
    }
    Ok(request)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let api_key = env::var("AURA_API_KEY").expect("AURA_API_KEY not set");
    let _ = API_KEY.set(api_key);
    let channel = Endpoint::from_static("http://trade.aura.rehab:40051").connect().await.unwrap();
    let interceptor: fn(Request<()>) -> Result<Request<()>, Status> = auth_interceptor;
    let clients = AuraClients::<_, NoCtx>::new(channel, interceptor);
    let res = clients.aura().fetch_full_wallet_info((), Request::new(FetchFullWalletsInfoReq {})).await.unwrap();
    println!("{:#?}", res.into_inner());
}
