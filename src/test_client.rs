use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtx;
use tonic::transport::Channel;
use aura_api_client::types::MarketTrade;

async fn test() {
    let channel = Channel::from_static("http://trade.aura.rehab:40051").connect().await.unwrap();
    let clients = AuraClients::<(), UserCtx>::new(channel, ()); // () is not an interceptor
    let aura = clients.aura();
    let _ = aura.trade();
}
