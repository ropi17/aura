use std::str::FromStr;

#[tokio::main]
async fn main() {
    let sol_price_usd = 150.0;
    let price_usd = 0.2;
    let price_in_sol = price_usd / sol_price_usd;
    println!("price_usd: {}", price_usd);
    println!("price_in_sol: {:.12}", price_in_sol);
    
    let scale: u64 = 1_000_000_000_000;
    let scaled = (price_in_sol * scale as f64) as u64;
    println!("scaled: {}", scaled);
}
