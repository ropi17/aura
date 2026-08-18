use std::str::FromStr;
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtxInterceptor;
use tonic::transport::Channel;
use aura_api_client::types::{
    ApiLimitOrder, ApiOrders, Direction, OrderEventTrigger,
    OrderState, RawOrder, SwapAmount, Target, TxnProcessors,
    UpdateTokenLimitOrders, UserNonceStrategy,
};

#[derive(Clone, Copy)]
struct NoCtx;

impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), mut req: &mut tonic::Request<T>) -> Result<(), tonic::Status> {
        req.metadata_mut().insert("auth", "6xuv81QgXfT188g2asvyo6aDc9Zusd1tzFDsUN9nidVX".parse().unwrap());
        Ok(())
    }
}

async fn fetch_sol_price_usd() -> f64 { 150.0 }

#[tokio::main]
async fn main() {
    let token = "5jm4gnwpt62kphmheet6rjzzfjvsfnxnqdpeugwp2u9q";
    let mint_addr = solana_address::Address::from_str(token).unwrap();
    let channel = Channel::from_static("http://trade.aura.rehab:40051").connect().await.unwrap();
    let clients = AuraClients::<fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status>, NoCtx>::new(
        channel,
        |r| Ok(r),
    );
    let mut aura = clients.limit_orders();
    
    let sol_price_usd = fetch_sol_price_usd().await;
    let price_usd = 0.2;
    let price_in_sol = price_usd / sol_price_usd;
    
    let scale: u64 = 1_000_000_000_000;
    let scaled = (price_in_sol * scale as f64) as u64;
    let price_ud128 = fastnum::UD128::from(scaled) / fastnum::UD128::from(scale);
    
    let mut trade_client = clients.aura();
    let wallet_res = trade_client.fetch_full_wallet_info((), tonic::Request::new(aura_api_client::types::FetchFullWalletsInfoReq {})).await.unwrap();
    let wallet_addr = wallet_res.into_inner().wallets.get(0).cloned().unwrap();
    println!("Wallet addr: {:?}", wallet_addr);
    
    let aura_target = Target::Price { price: price_ud128, direction: Direction::Below };
    
    let slippage_val = fastnum::UD128::from(150_000u64) / fastnum::UD128::from(1_000_000u64);
    
    let api_order = ApiLimitOrder {
        state: OrderState::Api { id: None, expire_dur: None, activate_dur: None },
        order: RawOrder {
            slippage: slippage_val,
            tip: decisol::Lamports::from(1000000u64),
            fee: decisol::Lamports::from(1000000u64),
            target: aura_target,
            amount: SwapAmount::Buy(decisol::QuoteLamports::Lamports(decisol::Lamports::from(10000000u64))),
            procs: TxnProcessors {
                jito_validators: false, jito_bundled: false, aura: true, bloxroute: false,
                nozomi: false, next_block: false, slot0: false, astra: false, block_razor: false,
                node1: false, tpu_penetrator: false, helius: true, stellium: true, soyas: true,
                falcon: true, raiden: true, circular: true, flash_block: true, moon: true,
                blocksprint: true, aura_revert: false, landx: true, manka: true, blockrush: true,
            },
            nonce: UserNonceStrategy::Hybrid,
            slot_latency: 0,
        },
        trigger: OrderEventTrigger::Immediate,
        wallet: wallet_addr,
    };
    
    let req = tonic::Request::new(UpdateTokenLimitOrders {
        mint: mint_addr,
        orders: ApiOrders { orders: vec![api_order] },
    });
    
    match aura.place_limit_orders((), req).await {
        Ok(res) => println!("Success: {:?}", res),
        Err(e) => println!("Error: {:?}", e),
    }
}
