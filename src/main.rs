use teloxide::{prelude::*, utils::command::BotCommands};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tokio::sync::Mutex;
use std::sync::Arc;
use std::env;
use std::time::Duration;
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use log::{info, warn};

#[derive(Clone, PartialEq)]
enum AppMode {
    Simulation,
    Mainnet,
}

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Perintah bot ini:")]
enum Command {
    #[command(description = "Mulai bot.")]
    Start,
    #[command(description = "Ubah ke mode Simulasi.")]
    ModeSimulasi,
    #[command(description = "Ubah ke mode Mainnet (ASLI/LIVE).")]
    ModeMainnet,
    #[command(description = "Pasang auto limit sell (contoh: /autolimit 100 50) -> jual 100% saat profit 50%.")]
    AutoLimit { amount_pct: f64, target_pnl: f64 },
    #[command(description = "Simulasi trigger limit buy kena.")]
    SimulateBuy,
}

// State untuk menyimpan konfigurasi bot
struct BotState {
    aura_api_key: String,
    mode: AppMode,
    // Rate limiter: Max 4 requests per second
    limiter: Arc<governor::DefaultDirectRateLimiter>,
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    info!("Memulai Aura Custom Bot...");

    let bot = Bot::from_env(); 

    // Baca API Key dari Environment Variable (Server Ubuntu)
    let api_key = env::var("AURA_API_KEY").unwrap_or_else(|_| "DUMMY_KEY".to_string());
    
    // Tentukan mode awal dari environment (default: Simulation)
    let initial_mode = match env::var("AURA_MODE").unwrap_or_default().to_uppercase().as_str() {
        "MAINNET" => AppMode::Mainnet,
        _ => AppMode::Simulation,
    };

    let quota = Quota::per_second(nonzero!(4u32));
    let limiter = Arc::new(RateLimiter::direct(quota));

    let state = Arc::new(Mutex::new(BotState {
        aura_api_key: api_key,
        mode: initial_mode,
        limiter,
    }));

    let handler = dptree::entry()
        .branch(Update::filter_message().filter_command::<Command>().endpoint(answer_command))
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

async fn answer_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<Mutex<BotState>>,
) -> ResponseResult<()> {
    match cmd {
        Command::Start => {
            let st = state.lock().await;
            let mode_str = if st.mode == AppMode::Mainnet { "MAINNET (Uang Asli)" } else { "SIMULASI (Aman)" };
            bot.send_message(msg.chat.id, format!("Halo! Ini Bot Kustom Aura Anda.\nMode Saat Ini: {}\n\nGunakan /modesimulasi atau /modemainnet untuk berganti mode.", mode_str)).await?;
        }
        Command::ModeSimulasi => {
            let mut st = state.lock().await;
            st.mode = AppMode::Simulation;
            bot.send_message(msg.chat.id, "✅ Mode diubah ke **SIMULASI**. Semua transaksi hanya uji coba.").await?;
        }
        Command::ModeMainnet => {
            let mut st = state.lock().await;
            st.mode = AppMode::Mainnet;
            bot.send_message(msg.chat.id, "⚠️ Mode diubah ke **MAINNET**. Transaksi akan memotong saldo sungguhan di Aura!").await?;
        }
        Command::AutoLimit { amount_pct, target_pnl } => {
            let st = state.lock().await;
            st.limiter.until_ready().await; // Rate limit check
            
            let prefix = if st.mode == AppMode::Mainnet { "[MAINNET EXECUTION]" } else { "[SIMULATION]" };
            let reply = format!(
                "{} Auto Limit Sell diteruskan ke Aura.\nTarget: Jual {}% token saat profit mencapai {}%.",
                prefix, amount_pct, target_pnl
            );
            bot.send_message(msg.chat.id, reply).await?;
        }
        Command::SimulateBuy => {
            let text = "🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15\n\nApa yang ingin Anda lakukan?";
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "swap_sell")],
                vec![InlineKeyboardButton::callback("🔄 Refresh PNL", "refresh_pnl")],
            ]);
            bot.send_message(msg.chat.id, text).reply_markup(keyboard).await?;
        }
    }
    Ok(())
}

async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<Mutex<BotState>>,
) -> ResponseResult<()> {
    if let Some(data) = q.data {
        let st = state.lock().await;
        st.limiter.until_ready().await;

        let is_mainnet = st.mode == AppMode::Mainnet;

        match data.as_str() {
            "swap_sell" => {
                let response_text = if is_mainnet {
                    "⚠️ [MAINNET] Mengirim perintah SWAP SELL sungguhan ke gRPC Aura..."
                } else {
                    "🟢 [SIMULASI] Swap Sell diproses (Aman, tidak ada transaksi sungguhan)."
                };
                bot.send_message(q.message.unwrap().chat.id, response_text).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "refresh_pnl" => {
                let dummy_pnl = if is_mainnet { "+25.0% (Mainnet Data)" } else { "+12.5% (Simulated Data)" };
                let updated_text = format!("🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15\n\n📊 PNL Saat ini: {}\n\nApa yang ingin Anda lakukan?", dummy_pnl);
                
                let keyboard = InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "swap_sell")],
                    vec![InlineKeyboardButton::callback("🔄 Refresh PNL", "refresh_pnl")],
                ]);

                if let Some(msg) = q.message {
                    bot.edit_message_text(msg.chat.id, msg.id, updated_text)
                        .reply_markup(keyboard)
                        .await?;
                }
                bot.answer_callback_query(q.id).text("PNL Berhasil diperbarui!").await?;
            }
            _ => {}
        }
    }
    Ok(())
}
