use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use teloxide::{prelude::*, utils::command::BotCommands};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use log::{info, error};

use tonic::transport::Channel;
use tonic::{Request, Status, service::Interceptor};
use tokio_stream::StreamExt;

// aura_api_client
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtx;
use aura_api_client::types::{UserActionEventSub, Ping};

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

#[derive(Clone)]
struct LimitOrder {
    id: usize,
    token: String,
    amount_usd: f64,
    target_mcap: String,
    tip_fee: f64,
    prio_fee: f64,
}

#[derive(Clone, PartialEq)]
enum EditField {
    None,
    // Limit Buy (Baru)
    BuyAmount,
    BuyMcap,
    BuyTip,
    BuyPrio,
    // Auto Limit
    AutoTip,
    AutoPrio,
    AutoActTime,
    AutoPnl,
    // History
    HistAmount(usize),
    HistMcap(usize),
    HistTip(usize),
    HistPrio(usize),
}

struct BotState {
    #[allow(dead_code)]
    aura_api_key: String,
    mode: AppMode,
    limiter: Arc<governor::DefaultDirectRateLimiter>,
    edit_field: EditField,

    // Auto Limit Sell Settings
    auto_limit_active: bool,
    limit_tip_fee: f64,
    limit_prio_fee: f64,
    limit_act_time: String,
    limit_target_pnl: String,

    // Manual Limit Buy Settings (mode pembuatan order baru)
    active_token: Option<String>,
    buy_amount_usd: f64,
    buy_target_mcap: String,
    buy_tip_fee: f64,
    buy_prio_fee: f64,

    // History of limit orders
    orders: Vec<LimitOrder>,
    next_order_id: usize,

    // Active chats for notifications
    active_chats: std::collections::HashSet<ChatId>,

    // Client gRPC Aura
    aura_clients: Option<AuraClients<fn(Request<()>) -> Result<Request<()>, Status>, UserCtx>>,
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
    pretty_env_logger::init();
    info!("Memulai Aura Custom Bot...");

    let bot = Bot::from_env();
    let api_key = env::var("AURA_API_KEY").unwrap_or_else(|_| "DUMMY_KEY".to_string());
    let _ = API_KEY.set(api_key.clone());
    let initial_mode = match env::var("AURA_MODE").unwrap_or_default().to_uppercase().as_str() {
        "MAINNET" => AppMode::Mainnet,
        _ => AppMode::Simulation,
    };

    let quota = Quota::per_second(nonzero!(4u32));
    let limiter = Arc::new(RateLimiter::direct(quota));

    // Setup Aura gRPC Client
    let mut aura_clients_opt = None;
    if api_key != "DUMMY_KEY" {
        match Channel::from_static("http://trade.aura.rehab:40051").connect().await {
            Ok(channel) => {
                let interceptor: fn(Request<()>) -> Result<Request<()>, Status> = auth_interceptor;
                let clients = AuraClients::new(channel, interceptor);
                aura_clients_opt = Some(clients);
                info!("Berhasil terhubung ke Aura gRPC (trade.aura.rehab:40051)");
            }
            Err(e) => {
                error!("Gagal koneksi ke gRPC Aura: {:?}", e);
            }
        }
    }

    let state = Arc::new(Mutex::new(BotState {
        aura_api_key: api_key,
        mode: initial_mode,
        limiter,
        edit_field: EditField::None,
        auto_limit_active: false,
        limit_tip_fee: 0.0015,
        limit_prio_fee: 0.0015,
        limit_act_time: "0s".to_string(),
        limit_target_pnl: "50%".to_string(),
        active_token: None,
        buy_amount_usd: 2.0,
        buy_target_mcap: "50 Mcap".to_string(),
        buy_tip_fee: 0.001,
        buy_prio_fee: 0.001,
        orders: Vec::new(),
        next_order_id: 1,
        active_chats: std::collections::HashSet::new(),
        aura_clients: aura_clients_opt.clone(),
    }));

    // Start UserActivity Stream and Ping if client is available
    if let Some(clients) = aura_clients_opt {
        let clients_ping = clients.clone();
        
        // 1. Ping Loop (every 10 seconds)
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            let mut ping_count = 0;
            loop {
                interval.tick().await;
                ping_count += 1;
                let req = Request::new(Ping { count: ping_count });
                let _ = clients_ping.aura().user_ping(req).await;
            }
        });

        // 2. UserActivity Stream Listener
        let st_clone = state.clone();
        let bot_clone = bot.clone();
        tokio::spawn(async move {
            loop {
                info!("Menyambungkan UserActivity Stream...");
                let req = Request::new(UserActionEventSub {});
                match clients.aura().user_activity(req).await {
                    Ok(resp) => {
                        let mut stream = resp.into_inner();
                        info!("UserActivity Stream tersambung!");
                        while let Some(msg) = stream.next().await {
                            match msg {
                                Ok(action) => {
                                    // Kirim notifikasi ke semua chat yang aktif
                                    let text = format!("🔔 **Aura Update**\n```\n{:?}\n```", action);
                                    let chats = st_clone.lock().await.active_chats.clone();
                                    for chat in chats {
                                        let _ = bot_clone.send_message(chat, &text).await;
                                    }
                                }
                                Err(e) => {
                                    error!("Stream message error: {:?}", e);
                                    break; // keluar untuk reconnect
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Gagal subscribe UserActivity: {:?}", e);
                    }
                }
                // Tunggu sebelum mencoba koneksi ulang
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    let handler = dptree::entry()
        .branch(Update::filter_message().filter_command::<Command>().endpoint(answer_command))
        .branch(Update::filter_message().filter(|msg: Message| {
            msg.text().is_some() && !msg.text().unwrap().starts_with('/')
        }).endpoint(handle_text_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

// ─── Keyboard Builders ────────────────────────────────────────────────────────

fn make_main_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("🔄 Swap Sell", "menu_swapsell"),
            InlineKeyboardButton::callback("🤖 Auto Limit Order", "menu_autolimit"),
        ],
        vec![InlineKeyboardButton::callback("📋 Limit Order History", "menu_history")],
    ])
}

fn make_swapsell_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("⚡ Tip | 0.0015 SOL", "none"),
            InlineKeyboardButton::callback("⛽ P.Fee | 0.0015 SOL", "none"),
        ],
        vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 95%", "none")],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

fn make_autolimit_keyboard(st: &BotState) -> InlineKeyboardMarkup {
    let status_text = if st.auto_limit_active { "🟢 ON" } else { "🔴 OFF" };
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("🤖 Auto Limit Sell | {}", status_text),
            "toggle_autolimit",
        )],
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", st.limit_tip_fee), "edit_auto_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", st.limit_prio_fee), "edit_auto_prio"),
        ],
        vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 90%", "none")],
        vec![
            InlineKeyboardButton::callback(format!("⏰ Act.Time | {}", st.limit_act_time), "edit_auto_acttime"),
            InlineKeyboardButton::callback("⌛ Order Expiry | 6d", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("Side | SELL%", "none"),
            InlineKeyboardButton::callback("TakeProfit", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("Activation | Instant", "none"),
            InlineKeyboardButton::callback("💰 100.0%", "none"),
        ],
        vec![InlineKeyboardButton::callback(
            format!("🎯 Target PNL | {}", st.limit_target_pnl),
            "edit_auto_pnl",
        )],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

fn make_limitbuy_keyboard(st: &BotState) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", st.buy_tip_fee), "edit_buy_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", st.buy_prio_fee), "edit_buy_prio"),
        ],
        vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 90%", "none")],
        vec![
            InlineKeyboardButton::callback("Side | BUY", "none"),
            InlineKeyboardButton::callback("Dip", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("Activation | Instant", "none"),
            InlineKeyboardButton::callback(format!("💰 {:.2} $", st.buy_amount_usd), "edit_buy_amount"),
        ],
        vec![InlineKeyboardButton::callback(
            format!("🎯 Target | {}", st.buy_target_mcap),
            "edit_buy_mcap",
        )],
        vec![InlineKeyboardButton::callback("📥 Place Order 📥", "place_limit_buy")],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

fn make_history_keyboard(orders: &[LimitOrder]) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    if orders.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback("📭 Belum ada order", "none")]);
    } else {
        for o in orders.iter() {
            let short = format!("{}...{}", &o.token[..4], &o.token[o.token.len()-4..]);
            rows.push(vec![InlineKeyboardButton::callback(
                format!("#{} | {} | {} | ${:.2}", o.id, short, o.target_mcap, o.amount_usd),
                format!("order_view_{}", o.id),
            )]);
        }
    }
    rows.push(vec![InlineKeyboardButton::callback("<< Back", "menu_main")]);
    InlineKeyboardMarkup::new(rows)
}

fn make_order_detail_keyboard(o: &LimitOrder) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", o.tip_fee), format!("edit_hist_tip_{}", o.id)),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", o.prio_fee), format!("edit_hist_prio_{}", o.id)),
        ],
        vec![InlineKeyboardButton::callback(format!("💰 Jumlah | {:.2} $", o.amount_usd), format!("edit_hist_amount_{}", o.id))],
        vec![InlineKeyboardButton::callback(format!("🎯 Target | {}", o.target_mcap), format!("edit_hist_mcap_{}", o.id))],
        vec![
            InlineKeyboardButton::callback("🗑 Hapus Order", format!("delete_order_{}", o.id)),
            InlineKeyboardButton::callback("<< Back", "menu_history"),
        ],
    ])
}

// ─── Messages & Formatters ────────────────────────────────────────────────────

async fn send_limitbuy_menu(bot: &Bot, chat_id: ChatId, st: &BotState) -> ResponseResult<()> {
    if let Some(token) = &st.active_token {
        let short = format!("{}...{}", &token[..6], &token[token.len()-4..]);
        let text = format!("🏦 **Token:** `{}`\n\n*Silakan klik tombol yang ingin diubah, lalu ketik nilainya.*", short);
        bot.send_message(chat_id, text).reply_markup(make_limitbuy_keyboard(st)).await?;
    }
    Ok(())
}

fn order_detail_text(o: &LimitOrder) -> String {
    let short = format!("{}...{}", &o.token[..6], &o.token[o.token.len()-4..]);
    format!(
        "📋 **Detail Order #{}**\n\nToken: `{}`\nFull: `{}`\n\n💰 Jumlah Beli: ${:.2}\n🎯 Target Mcap: {}\n⚡ Tip: {} SOL\n⛽ P.Fee: {} SOL\n🏄‍♂️ Slippage: 90%\n\n*Klik tombol yang ingin diedit, lalu balas dengan nominal barunya.*",
        o.id, short, o.token, o.amount_usd, o.target_mcap, o.tip_fee, o.prio_fee
    )
}

fn parse_number(text: &str) -> Option<f64> {
    text.to_lowercase()
        .replace("sol", "")
        .replace("$", "")
        .trim()
        .parse::<f64>()
        .ok()
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

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
            let mode = if st.mode == AppMode::Mainnet { "MAINNET" } else { "SIMULASI" };
            bot.send_message(msg.chat.id, format!("👋 **Selamat datang!**\nMode: `{}`\n\nPilih menu atau **Paste Address Token** untuk Limit Buy.", mode))
                .reply_markup(make_main_menu_keyboard()).await?;
        }
        Command::ModeSimulasi => {
            state.lock().await.mode = AppMode::Simulation;
            bot.send_message(msg.chat.id, "✅ Mode **SIMULASI**.").await?;
        }
        Command::ModeMainnet => {
            state.lock().await.mode = AppMode::Mainnet;
            bot.send_message(msg.chat.id, "⚠️ Mode **MAINNET** aktif!").await?;
        }
        Command::SimulateBuy => {
            bot.send_message(msg.chat.id, "🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15")
                .reply_markup(InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "execute_swap_sell")],
                    vec![InlineKeyboardButton::callback("🔄 Refresh PNL", "refresh_pnl")],
                ])).await?;
        }
    }
    Ok(())
}

async fn handle_text_message(
    bot: Bot,
    msg: Message,
    state: Arc<Mutex<BotState>>,
) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        let mut st = state.lock().await;
        st.limiter.until_ready().await;
        let text_trim = text.trim();
        let lower = text_trim.to_lowercase();
        let chat_id = msg.chat.id;

        // Register active chat
        st.active_chats.insert(chat_id);

        // Cek Address Token dulu
        let is_base58 = text_trim.chars().all(|c| c.is_alphanumeric());
        if text_trim.len() >= 32 && text_trim.len() <= 50 && !text_trim.contains(' ') && is_base58 {
            st.active_token = Some(text_trim.to_string());
            st.edit_field = EditField::None;
            send_limitbuy_menu(&bot, chat_id, &st).await?;
            return Ok(());
        }

        // Tangani input berdasarkan field yang sedang diedit
        match st.edit_field.clone() {
            EditField::BuyAmount => {
                if let Some(val) = parse_number(&lower) {
                    st.buy_amount_usd = val;
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                }
            }
            EditField::BuyMcap => {
                st.buy_target_mcap = text_trim.to_string();
                st.edit_field = EditField::None;
                send_limitbuy_menu(&bot, chat_id, &st).await?;
            }
            EditField::BuyTip => {
                if let Some(val) = parse_number(&lower) {
                    st.buy_tip_fee = val;
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                }
            }
            EditField::BuyPrio => {
                if let Some(val) = parse_number(&lower) {
                    st.buy_prio_fee = val;
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                }
            }
            EditField::AutoTip => {
                if let Some(val) = parse_number(&lower) {
                    st.limit_tip_fee = val;
                    st.edit_field = EditField::None;
                    bot.send_message(chat_id, format!("Tip Auto Limit diubah ke {} SOL", val))
                        .reply_markup(make_autolimit_keyboard(&st)).await?;
                }
            }
            EditField::AutoPrio => {
                if let Some(val) = parse_number(&lower) {
                    st.limit_prio_fee = val;
                    st.edit_field = EditField::None;
                    bot.send_message(chat_id, format!("P.Fee Auto Limit diubah ke {} SOL", val))
                        .reply_markup(make_autolimit_keyboard(&st)).await?;
                }
            }
            EditField::AutoActTime => {
                st.limit_act_time = text_trim.to_string();
                st.edit_field = EditField::None;
                bot.send_message(chat_id, format!("Act.Time Auto Limit diubah ke {}", text_trim))
                    .reply_markup(make_autolimit_keyboard(&st)).await?;
            }
            EditField::AutoPnl => {
                st.limit_target_pnl = lower.replace("%pnl", "%").trim().to_uppercase();
                st.edit_field = EditField::None;
                bot.send_message(chat_id, format!("Target PNL Auto Limit diubah ke {}", st.limit_target_pnl))
                    .reply_markup(make_autolimit_keyboard(&st)).await?;
            }
            EditField::HistAmount(id) => {
                if let Some(val) = parse_number(&lower) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.amount_usd = val; }
                    st.edit_field = EditField::None;
                    if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o)).await?;
                    }
                }
            }
            EditField::HistMcap(id) => {
                if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.target_mcap = text_trim.to_string(); }
                st.edit_field = EditField::None;
                if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                    bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o)).await?;
                }
            }
            EditField::HistTip(id) => {
                if let Some(val) = parse_number(&lower) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.tip_fee = val; }
                    st.edit_field = EditField::None;
                    if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o)).await?;
                    }
                }
            }
            EditField::HistPrio(id) => {
                if let Some(val) = parse_number(&lower) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.prio_fee = val; }
                    st.edit_field = EditField::None;
                    if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o)).await?;
                    }
                }
            }
            EditField::None => {
                // Ignore text if we are not editing anything, 
                // UN Kecuali fallback global kalau user ngetik "5$" tanpa nge-klik tombol edit Amount dulu (untuk UX lama)
                if lower.ends_with('$') {
                    if let Some(val) = parse_number(&lower) {
                        st.buy_amount_usd = val;
                        if st.active_token.is_some() { send_limitbuy_menu(&bot, chat_id, &st).await?; }
                    }
                } else if lower.contains("mcap") {
                    st.buy_target_mcap = text_trim.to_string();
                    if st.active_token.is_some() { send_limitbuy_menu(&bot, chat_id, &st).await?; }
                } else if lower.ends_with("sol") {
                    if let Some(val) = parse_number(&lower) {
                        st.buy_tip_fee = val;
                        st.buy_prio_fee = val;
                        if st.active_token.is_some() { send_limitbuy_menu(&bot, chat_id, &st).await?; }
                    }
                }
            }
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
        let mut st = state.lock().await;
        st.limiter.until_ready().await;
        let chat_id = if let Some(msg) = &q.message { msg.chat.id } else { return Ok(()); };
        let msg_id  = if let Some(msg) = &q.message { msg.id }      else { return Ok(()); };

        // Register active chat
        st.active_chats.insert(chat_id);

        // Prefix routing for history order view & delete
        if data.starts_with("order_view_") {
            let id: usize = data.trim_start_matches("order_view_").parse().unwrap_or(0);
            if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                bot.edit_message_text(chat_id, msg_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o)).await?;
            }
            bot.answer_callback_query(q.id).await?;
            return Ok(());
        }
        if data.starts_with("delete_order_") {
            let id: usize = data.trim_start_matches("delete_order_").parse().unwrap_or(0);
            st.orders.retain(|o| o.id != id);
            bot.edit_message_text(chat_id, msg_id, "Order dihapus.").reply_markup(make_history_keyboard(&st.orders)).await?;
            bot.answer_callback_query(q.id).await?;
            return Ok(());
        }

        // Prefix routing for history edit fields
        if data.starts_with("edit_hist_amount_") {
            let id: usize = data.trim_start_matches("edit_hist_amount_").parse().unwrap_or(0);
            st.edit_field = EditField::HistAmount(id);
            bot.answer_callback_query(q.id).text("Ketik jumlah baru!").await?;
            bot.send_message(chat_id, "✏️ Ketik jumlah baru (misal: 5)").await?;
            return Ok(());
        }
        if data.starts_with("edit_hist_mcap_") {
            let id: usize = data.trim_start_matches("edit_hist_mcap_").parse().unwrap_or(0);
            st.edit_field = EditField::HistMcap(id);
            bot.answer_callback_query(q.id).text("Ketik target baru!").await?;
            bot.send_message(chat_id, "✏️ Ketik target baru (misal: 100 mcap)").await?;
            return Ok(());
        }
        if data.starts_with("edit_hist_tip_") {
            let id: usize = data.trim_start_matches("edit_hist_tip_").parse().unwrap_or(0);
            st.edit_field = EditField::HistTip(id);
            bot.answer_callback_query(q.id).text("Ketik Tip baru!").await?;
            bot.send_message(chat_id, "✏️ Ketik Tip baru (misal: 0.005)").await?;
            return Ok(());
        }
        if data.starts_with("edit_hist_prio_") {
            let id: usize = data.trim_start_matches("edit_hist_prio_").parse().unwrap_or(0);
            st.edit_field = EditField::HistPrio(id);
            bot.answer_callback_query(q.id).text("Ketik P.Fee baru!").await?;
            bot.send_message(chat_id, "✏️ Ketik P.Fee baru (misal: 0.005)").await?;
            return Ok(());
        }

        // Exact match routing
        match data.as_str() {
            "menu_main" => {
                bot.edit_message_text(chat_id, msg_id, "👋 **Menu Utama**").reply_markup(make_main_menu_keyboard()).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_swapsell" => {
                bot.edit_message_text(chat_id, msg_id, "⚙️ **Swap Sell (Terkunci)**").reply_markup(make_swapsell_keyboard()).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_autolimit" => {
                bot.edit_message_text(chat_id, msg_id, "⚙️ **Auto Limit Sell**\nKlik tombol lalu ketik nilainya.").reply_markup(make_autolimit_keyboard(&st)).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_history" => {
                bot.edit_message_text(chat_id, msg_id, "📋 **History Order**").reply_markup(make_history_keyboard(&st.orders)).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "toggle_autolimit" => {
                st.auto_limit_active = !st.auto_limit_active;
                bot.edit_message_reply_markup(chat_id, msg_id).reply_markup(make_autolimit_keyboard(&st)).await?;
                bot.answer_callback_query(q.id).await?;
            }
            // Auto Limit Edits
            "edit_auto_tip" => {
                st.edit_field = EditField::AutoTip;
                bot.answer_callback_query(q.id).text("Menunggu input tip...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Tip baru (contoh: 0.005)").await?;
            }
            "edit_auto_prio" => {
                st.edit_field = EditField::AutoPrio;
                bot.answer_callback_query(q.id).text("Menunggu input p.fee...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai P.Fee baru (contoh: 0.005)").await?;
            }
            "edit_auto_acttime" => {
                st.edit_field = EditField::AutoActTime;
                bot.answer_callback_query(q.id).text("Menunggu input act.time...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Act.Time baru (contoh: 5s, 10s)").await?;
            }
            "edit_auto_pnl" => {
                st.edit_field = EditField::AutoPnl;
                bot.answer_callback_query(q.id).text("Menunggu input target PNL...").await?;
                bot.send_message(chat_id, "✏️ Ketik Target PNL baru (contoh: 100%)").await?;
            }
            // Buy Limit Edits
            "edit_buy_amount" => {
                st.edit_field = EditField::BuyAmount;
                bot.answer_callback_query(q.id).text("Menunggu input jumlah...").await?;
                bot.send_message(chat_id, "✏️ Ketik jumlah USD beli baru (contoh: 5)").await?;
            }
            "edit_buy_mcap" => {
                st.edit_field = EditField::BuyMcap;
                bot.answer_callback_query(q.id).text("Menunggu input target...").await?;
                bot.send_message(chat_id, "✏️ Ketik target Mcap baru (contoh: 100 mcap)").await?;
            }
            "edit_buy_tip" => {
                st.edit_field = EditField::BuyTip;
                bot.answer_callback_query(q.id).text("Menunggu input tip...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Tip baru (contoh: 0.005)").await?;
            }
            "edit_buy_prio" => {
                st.edit_field = EditField::BuyPrio;
                bot.answer_callback_query(q.id).text("Menunggu input p.fee...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai P.Fee baru (contoh: 0.005)").await?;
            }
            "place_limit_buy" => {
                if let Some(token) = &st.active_token {
                    let o = LimitOrder {
                        id: st.next_order_id,
                        token: token.clone(),
                        amount_usd: st.buy_amount_usd,
                        target_mcap: st.buy_target_mcap.clone(),
                        tip_fee: st.buy_tip_fee,
                        prio_fee: st.buy_prio_fee,
                    };
                    st.orders.push(o);
                    st.next_order_id += 1;
                    bot.answer_callback_query(q.id).text("Order disimpan!").await?;
                    bot.send_message(chat_id, "🟢 Limit Buy Order disimpan. Cek Limit Order History.").await?;
                }
            }
            "execute_swap_sell" => {
                bot.send_message(chat_id, "🟢 Swap Sell Dieksekusi!").await?;
                bot.answer_callback_query(q.id).await?;
            }
            "refresh_pnl" => {
                bot.answer_callback_query(q.id).text("PNL Refresh.").await?;
            }
            "none" => {
                bot.answer_callback_query(q.id).text("Terkunci (Fixed Setting).").await?;
            }
            _ => {}
        }
    }
    Ok(())
}
