// test_client.rs — example / scratch test (not compiled as part of main binary)
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtxInterceptor;
use tonic::transport::Channel;

#[derive(Clone, Copy)]
struct NoCtx;

impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), _req: &mut tonic::Request<T>) -> Result<(), tonic::Status> {
        Ok(())
    }
}

#[allow(dead_code)]
async fn test() {
    let channel = Channel::from_static("http://trade.aura.rehab:40051").connect().await.unwrap();
    // Use an identity interceptor (no-op fn) + NoCtx for per-call payload
    let clients = AuraClients::<fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status>, NoCtx>::new(
        channel,
        |r| Ok(r),
    );
    let _aura = clients.aura();
    // To call methods: _aura.user_ping(req, ()).await
}
