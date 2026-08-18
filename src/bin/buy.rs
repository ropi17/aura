use std::env;
use std::str::FromStr;
use tonic::{Request, Status};
use tonic::transport::Endpoint;
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtxInterceptor;
use aura_api_client::types::{
    MarketTrade, SwapAmount, TxnProcessors, UserNonceStrategy, ApiOrders, TradeFilters,
    FetchFullWalletsInfoReq
};
use solana_address::Address;
use decisol::{Lamports, QuoteLamports, Wsol};
use fastnum::UD128;

#[derive(Clone, Copy)]
struct NoCtx;

impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), _req: &mut tonic::Request<T>) -> Result<(), tonic::Status> {
        Ok(())
    }
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
    let _ = API_KEY.set(api_key.clone());

    let endpoint = Endpoint::from_static("http://trade.aura.rehab:40051");
    let channel = endpoint.connect().await.unwrap();
    let interceptor: fn(Request<()>) -> Result<Request<()>, Status> = auth_interceptor;
    let clients = AuraClients::<_, NoCtx>::new(channel, interceptor);
    
    let mut trade_client = clients.aura();
    
    let wallet_res = trade_client.fetch_full_wallet_info((), Request::new(FetchFullWalletsInfoReq {})).await.unwrap();
    let wallet = wallet_res.into_inner().wallets.get(0).cloned().unwrap();
    println!("Menggunakan wallet: {}", wallet);

    let sol_price_usd = 140.0;
    let price_usd = 0.20;
    let price_in_sol = price_usd / sol_price_usd;
    let amount_lamports = (price_in_sol * 1_000_000_000.0) as u64;

    let tip_lamports = (0.0005 * 1_000_000_000.0) as u64;
    let prio_lamports = (0.005 * 1_000_000_000.0) as u64;

    let mint = Address::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap(); // USDC

    let procs = TxnProcessors {
        jito_validators: false, jito_bundled: false, aura: true, bloxroute: false,
        nozomi: false, next_block: false, slot0: false, astra: false,
        block_razor: false, node1: false, tpu_penetrator: false, helius: true,
        stellium: true, soyas: true, falcon: true, raiden: true, circular: true,
        flash_block: true, moon: true, blocksprint: true, aura_revert: false,
        landx: true, manka: true, blockrush: true,
    };

    let wsol = Wsol::from(amount_lamports);
    let quote_amount = QuoteLamports::from(wsol);

    let req = MarketTrade {
        wallet: Some(wallet),
        amount: SwapAmount::Buy(quote_amount),
        mint,
        slippage: fastnum::UD128::from(100_000u64) / fastnum::UD128::from(1_000_000u64),
        tip: Lamports::from(tip_lamports),
        priority_fee: Lamports::from(prio_lamports),
        procs: Some(procs),
        nonce: UserNonceStrategy::Hybrid,
        slot_latency: None,
        expire_at: None,
        rpc_nonce: None,
        max_price_impact: None,
        limit_orders: ApiOrders { orders: vec![] },
        filters: TradeFilters {
            min_mcap: None,
            max_mcap: None,
        },
    };

    println!("Mengirim request MarketTrade...");
    let res = trade_client.trade((), Request::new(req)).await;
    match res {
        Ok(r) => println!("Trade response: {:?}", r.into_inner()),
        Err(e) => println!("Trade error: {:?}", e),
    }
}
