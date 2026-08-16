use teloxide::{prelude::*, utils::command::BotCommands};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tokio::sync::Mutex;
use std::sync::Arc;
use std::env;
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use log::info;

#[derive(Clone, PartialEq)]
enum AppMode {
    Simulation,
    Mainnet,
}

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Perintah bot ini:")]
enum Command {
    #[command(description = "Mulai bot dan tampilkan Menu Utama.")]
    Start,
    #[command(description = "Ubah ke mode Simulasi.")]
    ModeSimulasi,
    #[command(description = "Ubah ke mode Mainnet (ASLI/LIVE).")]
    ModeMainnet,
    #[command(description = "Simulasi trigger limit buy kena.")]
    SimulateBuy,
}

// State untuk menyimpan konfigurasi bot
struct BotState {
    #[allow(dead_code)]
    aura_api_key: String,
    mode: AppMode,
    auto_limit_active: bool,
    limit_tip_fee: f64,
    limit_prio_fee: f64,
    // Rate limiter: Max 4 requests per second
    limiter: Arc<governor::DefaultDirectRateLimiter>,
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    info!("Memulai Aura Custom Bot...");

    let bot = Bot::from_env(); 

    let api_key = env::var("AURA_API_KEY").unwrap_or_else(|_| "DUMMY_KEY".to_string());
    
    let initial_mode = match env::var("AURA_MODE").unwrap_or_default().to_uppercase().as_str() {
        "MAINNET" => AppMode::Mainnet,
        _ => AppMode::Simulation,
    };

    let quota = Quota::per_second(nonzero!(4u32));
    let limiter = Arc::new(RateLimiter::direct(quota));

    let state = Arc::new(Mutex::new(BotState {
        aura_api_key: api_key,
        mode: initial_mode,
        auto_limit_active: false,
        limit_tip_fee: 0.0015,
        limit_prio_fee: 0.0015,
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

// Fungsi pembantu untuk membuat Keyboard Menu Utama
fn make_main_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("🔄 Swap Sell", "menu_swapsell"),
            InlineKeyboardButton::callback("🤖 Auto Limit Order", "menu_autolimit"),
        ]
    ])
}

// Fungsi pembantu untuk membuat Keyboard Menu Swap Sell
fn make_swapsell_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            // Tombol ini hanya sebagai display (callback "none"), karena fee di-fix.
            InlineKeyboardButton::callback("⚡ Tip | 0.0015 SOL", "none"),
            InlineKeyboardButton::callback("⛽ P.Fee | 0.0015 SOL", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("🏄‍♂️ Slippage | 95%", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("<< Back", "menu_main"),
        ]
    ])
}

// Fungsi pembantu untuk membuat Keyboard Menu Auto Limit
fn make_autolimit_keyboard(is_active: bool, tip: f64, prio: f64) -> InlineKeyboardMarkup {
    let status_text = if is_active { "🟢 ON" } else { "🔴 OFF" };
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("🤖 Auto Limit | {}", status_text), "toggle_autolimit"),
        ],
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", tip), "cycle_limit_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", prio), "cycle_limit_prio"),
        ],
        vec![
            InlineKeyboardButton::callback("🏄‍♂️ Slippage | 95%", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("<< Back", "menu_main"),
        ]
    ])
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
            st.limiter.until_ready().await;
            let mode_str = if st.mode == AppMode::Mainnet { "MAINNET" } else { "SIMULASI" };
            
            let text = format!("👋 **Selamat datang di Custom Aura Bot!**\nMode saat ini: `{}`\n\nSilakan pilih menu pengaturan di bawah ini:", mode_str);
            bot.send_message(msg.chat.id, text)
                .reply_markup(make_main_menu_keyboard())
                .await?;
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
        Command::SimulateBuy => {
            let text = "🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15\n\nApa yang ingin Anda lakukan?";
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "execute_swap_sell")],
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
        let mut st_locked = state.lock().await;
        st_locked.limiter.until_ready().await;

        let chat_id = if let Some(msg) = &q.message {
            msg.chat().id
        } else {
            return Ok(());
        };
        let msg_id = if let Some(msg) = &q.message {
            msg.id()
        } else {
            return Ok(());
        };

        match data.as_str() {
            "menu_main" => {
                let mode_str = if st_locked.mode == AppMode::Mainnet { "MAINNET" } else { "SIMULASI" };
                let text = format!("👋 **Selamat datang di Custom Aura Bot!**\nMode saat ini: `{}`\n\nSilakan pilih menu pengaturan di bawah ini:", mode_str);
                
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_main_menu_keyboard())
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_swapsell" => {
                let text = "⚙️ **Pengaturan Swap Sell**\nSemua pengaturan di bawah ini sudah **Fixed (Terkunci)** sesuai permintaan Anda untuk menghindari kerugian karena salah klik.";
                
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_swapsell_keyboard())
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_autolimit" => {
                let text = "⚙️ **Pengaturan Auto Limit Order**\nSlippage terkunci di 95%. Silakan tekan tombol **Auto Limit** untuk Menghidupkan/Mematikan fitur ini.";
                
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(st_locked.auto_limit_active, st_locked.limit_tip_fee, st_locked.limit_prio_fee))
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "toggle_autolimit" => {
                st_locked.auto_limit_active = !st_locked.auto_limit_active;
                let text = "⚙️ **Pengaturan Auto Limit Order**\nSlippage terkunci di 95%. Silakan tekan tombol **Auto Limit** untuk Menghidupkan/Mematikan fitur ini.";
                
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(st_locked.auto_limit_active, st_locked.limit_tip_fee, st_locked.limit_prio_fee))
                    .await?;
                bot.answer_callback_query(q.id).text("Status Auto Limit diperbarui!").await?;
            }
            "cycle_limit_tip" => {
                // Rotasi nilai Tip: 0.001 -> 0.0015 -> 0.002 -> 0.003 -> 0.005 -> 0.001
                let vals = [0.001, 0.0015, 0.002, 0.003, 0.005];
                let mut next_val = vals[0];
                for (i, &v) in vals.iter().enumerate() {
                    if (st_locked.limit_tip_fee - v).abs() < f64::EPSILON {
                        next_val = vals[(i + 1) % vals.len()];
                        break;
                    }
                }
                st_locked.limit_tip_fee = next_val;
                
                let text = "⚙️ **Pengaturan Auto Limit Order**\nSlippage terkunci di 95%. Silakan tekan tombol **Auto Limit** untuk Menghidupkan/Mematikan fitur ini.";
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(st_locked.auto_limit_active, st_locked.limit_tip_fee, st_locked.limit_prio_fee))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("Tip diubah ke {} SOL", next_val)).await?;
            }
            "cycle_limit_prio" => {
                // Rotasi nilai P.Fee: 0.001 -> 0.0015 -> 0.002 -> 0.003 -> 0.005 -> 0.001
                let vals = [0.001, 0.0015, 0.002, 0.003, 0.005];
                let mut next_val = vals[0];
                for (i, &v) in vals.iter().enumerate() {
                    if (st_locked.limit_prio_fee - v).abs() < f64::EPSILON {
                        next_val = vals[(i + 1) % vals.len()];
                        break;
                    }
                }
                st_locked.limit_prio_fee = next_val;
                
                let text = "⚙️ **Pengaturan Auto Limit Order**\nSlippage terkunci di 95%. Silakan tekan tombol **Auto Limit** untuk Menghidupkan/Mematikan fitur ini.";
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(st_locked.auto_limit_active, st_locked.limit_tip_fee, st_locked.limit_prio_fee))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("P.Fee diubah ke {} SOL", next_val)).await?;
            }
            "execute_swap_sell" => {
                let is_mainnet = st_locked.mode == AppMode::Mainnet;
                // Mengambil nilai fix
                let prio_fee = 0.0015;
                let tip = 0.0015;
                let slippage = 95;

                let response_text = if is_mainnet {
                    format!("⚠️ [MAINNET] Mengirim perintah SWAP SELL ke gRPC Aura!\nPriority Fee: {} SOL\nTip: {} SOL\nSlippage: {}%", prio_fee, tip, slippage)
                } else {
                    format!("🟢 [SIMULASI] Swap Sell diproses!\nPriority Fee: {} SOL\nTip: {} SOL\nSlippage: {}%\n(Aman, tidak ada transaksi sungguhan).", prio_fee, tip, slippage)
                };
                
                bot.send_message(chat_id, response_text).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "refresh_pnl" => {
                let is_mainnet = st_locked.mode == AppMode::Mainnet;
                let dummy_pnl = if is_mainnet { "+25.0% (Mainnet Data)" } else { "+12.5% (Simulated Data)" };
                let updated_text = format!("🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15\n\n📊 PNL Saat ini: {}\n\nApa yang ingin Anda lakukan?", dummy_pnl);
                
                let keyboard = InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "execute_swap_sell")],
                    vec![InlineKeyboardButton::callback("🔄 Refresh PNL", "refresh_pnl")],
                ]);

                bot.edit_message_text(chat_id, msg_id, updated_text)
                    .reply_markup(keyboard)
                    .await?;
                
                bot.answer_callback_query(q.id).text("PNL Berhasil diperbarui!").await?;
            }
            "none" => {
                // Tombol yang bersifat pasif / hanya informasi
                bot.answer_callback_query(q.id).text("Tombol ini dikunci (Fixed Setting).").await?;
            }
            _ => {}
        }
    }
    Ok(())
}
