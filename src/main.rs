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

// ─── Limit Order entry ───────────────────────────────────────────────────────
#[derive(Clone)]
struct LimitOrder {
    id: usize,
    token: String,
    amount_usd: f64,
    target_mcap: String,
    tip_fee: f64,
    prio_fee: f64,
}

// State yang sedang di-edit (menampung index order yang di-edit)
#[derive(Clone, PartialEq)]
enum EditField {
    None,
    Amount,
    Mcap,
    Tip,
    Prio,
    // Untuk Auto Limit edit
    LimitPnl,
}

struct BotState {
    #[allow(dead_code)]
    aura_api_key: String,
    mode: AppMode,
    auto_limit_active: bool,
    limit_tip_fee: f64,
    limit_prio_fee: f64,
    // Auto Limit Sell — pengaturan yang bisa diubah user
    limit_act_time: String,      // format: "0s", "5s", "10s", dll
    limit_target_pnl: String,    // format: "50%", "100%", dll
    limiter: Arc<governor::DefaultDirectRateLimiter>,

    // Manual Limit Buy Settings (mode pembuatan order baru)
    active_token: Option<String>,
    buy_amount_usd: f64,
    buy_target_mcap: String,
    buy_tip_fee: f64,
    buy_prio_fee: f64,

    // History of limit orders
    orders: Vec<LimitOrder>,
    next_order_id: usize,

    // Which order is currently being edited, and which field
    editing_order_id: Option<usize>,
    edit_field: EditField,
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
        limit_act_time: "0s".to_string(),
        limit_target_pnl: "50%".to_string(),
        limiter,
        active_token: None,
        buy_amount_usd: 2.0,
        buy_target_mcap: "50 Mcap".to_string(),
        buy_tip_fee: 0.001,
        buy_prio_fee: 0.001,
        orders: Vec::new(),
        next_order_id: 1,
        editing_order_id: None,
        edit_field: EditField::None,
    }));

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

// ─── Keyboard builders ────────────────────────────────────────────────────────

fn make_main_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("🔄 Swap Sell", "menu_swapsell"),
            InlineKeyboardButton::callback("🤖 Auto Limit Order", "menu_autolimit"),
        ],
        vec![
            InlineKeyboardButton::callback("📋 Limit Order History", "menu_history"),
        ],
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

fn make_autolimit_keyboard(is_active: bool, tip: f64, prio: f64, act_time: &str, target_pnl: &str) -> InlineKeyboardMarkup {
    let status_text = if is_active { "🟢 ON" } else { "🔴 OFF" };
    InlineKeyboardMarkup::new(vec![
        // Sakelar ON/OFF
        vec![InlineKeyboardButton::callback(
            format!("🤖 Auto Limit Sell | {}", status_text),
            "toggle_autolimit",
        )],
        // Bisa diubah
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", tip), "cycle_limit_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", prio), "cycle_limit_prio"),
        ],
        // Fixed display sesuai Aura
        vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 90%", "none")],
        vec![
            InlineKeyboardButton::callback(format!("⏰ Act.Time | {}", act_time), "cycle_limit_acttime"),
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
        // Target PNL — bisa diubah dengan ketik n%pnl
        vec![InlineKeyboardButton::callback(
            format!("🎯 Target PNL | {}", target_pnl),
            "set_limit_pnl",
        )],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

fn make_limitbuy_keyboard(st: &BotState) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", st.buy_tip_fee), "cycle_buy_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", st.buy_prio_fee), "cycle_buy_prio"),
        ],
        vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 90%", "none")],
        vec![
            InlineKeyboardButton::callback("Side | BUY", "none"),
            InlineKeyboardButton::callback("Dip", "none"),
        ],
        vec![
            InlineKeyboardButton::callback("Activation | Instant", "none"),
            InlineKeyboardButton::callback(format!("💰 {:.2} $", st.buy_amount_usd), "none"),
        ],
        vec![InlineKeyboardButton::callback(
            format!("🎯 Target | {}", st.buy_target_mcap),
            "none",
        )],
        vec![InlineKeyboardButton::callback("📥 Place Order 📥", "place_limit_buy")],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

fn make_history_keyboard(orders: &[LimitOrder]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    if orders.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback(
            "📭 Belum ada order",
            "none",
        )]);
    } else {
        for o in orders.iter() {
            let short_token = format!("{}...{}", &o.token[..4], &o.token[o.token.len()-4..]);
            rows.push(vec![InlineKeyboardButton::callback(
                format!("#{} | {} | {} | ${:.2}", o.id, short_token, o.target_mcap, o.amount_usd),
                format!("order_view_{}", o.id),
            )]);
        }
    }

    rows.push(vec![InlineKeyboardButton::callback("<< Back", "menu_main")]);
    InlineKeyboardMarkup::new(rows)
}

fn make_order_detail_keyboard(order: &LimitOrder) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", order.tip_fee), format!("edit_tip_{}", order.id)),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", order.prio_fee), format!("edit_prio_{}", order.id)),
        ],
        vec![InlineKeyboardButton::callback(
            format!("💰 Jumlah | {:.2} $", order.amount_usd),
            format!("edit_amount_{}", order.id),
        )],
        vec![InlineKeyboardButton::callback(
            format!("🎯 Target | {}", order.target_mcap),
            format!("edit_mcap_{}", order.id),
        )],
        vec![
            InlineKeyboardButton::callback("🗑 Hapus Order", format!("delete_order_{}", order.id)),
            InlineKeyboardButton::callback("<< Back", "menu_history"),
        ],
    ])
}

// ─── Send helpers ─────────────────────────────────────────────────────────────

async fn send_limitbuy_menu(bot: &Bot, chat_id: ChatId, token: &str, st: &BotState) -> ResponseResult<()> {
    let short = format!("{}...{}", &token[..6], &token[token.len()-4..]);
    let text = format!(
        "🏦 **Token:** `{}`\n\n*Ketik `5$` → ubah nominal beli*\n*Ketik `1000 mcap` → ubah target*\n*Ketik `0.002 sol` → ubah Tip & P.Fee*",
        short
    );
    bot.send_message(chat_id, text)
        .reply_markup(make_limitbuy_keyboard(st))
        .await?;
    Ok(())
}

fn order_detail_text(order: &LimitOrder) -> String {
    let short = format!("{}...{}", &order.token[..6], &order.token[order.token.len()-4..]);
    format!(
        "📋 **Detail Order #{}**\n\nToken: `{}`\nFull: `{}`\n\n💰 Jumlah Beli: ${:.2}\n🎯 Target Mcap: {}\n⚡ Tip: {} SOL\n⛽ P.Fee: {} SOL\n🏄‍♂️ Slippage: 90%\n\nTekan tombol di bawah untuk **Edit** atau **Hapus**.\n*Untuk ubah nilai, ketik setelah memilih field, misal: `5$` atau `200 mcap` atau `0.002 sol`*",
        order.id, short, order.token, order.amount_usd, order.target_mcap, order.tip_fee, order.prio_fee
    )
}

// ─── Command Handler ──────────────────────────────────────────────────────────

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
            let text = format!(
                "👋 **Selamat datang di Custom Aura Bot!**\nMode saat ini: `{}`\n\nSilakan pilih menu atau **Paste Address Token** untuk Limit Buy.",
                mode_str
            );
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

// ─── Text Message Handler ─────────────────────────────────────────────────────

async fn handle_text_message(
    bot: Bot,
    msg: Message,
    state: Arc<Mutex<BotState>>,
) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        let mut st = state.lock().await;
        st.limiter.until_ready().await;
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // ── Format n$ → ubah jumlah beli ────────────────────────────────
        if lower.ends_with('$') {
            if let Ok(amount) = lower.trim_end_matches('$').trim().parse::<f64>() {
                // Jika ada order yang sedang di-edit
                if let (Some(oid), EditField::Amount) = (st.editing_order_id, &st.edit_field) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == oid) {
                        o.amount_usd = amount;
                    }
                    let order = st.orders.iter().find(|o| o.id == oid).cloned();
                    st.edit_field = EditField::None;
                    st.editing_order_id = None;
                    if let Some(o) = order {
                        bot.send_message(msg.chat.id, order_detail_text(&o))
                            .reply_markup(make_order_detail_keyboard(&o))
                            .await?;
                    }
                } else {
                    // Mode buat order baru
                    st.buy_amount_usd = amount;
                    if let Some(token) = st.active_token.clone() {
                        send_limitbuy_menu(&bot, msg.chat.id, &token, &st).await?;
                    } else {
                        bot.send_message(msg.chat.id, "❌ Paste address token dulu!").await?;
                    }
                }
            }
            return Ok(());
        }

        // ── Format n mcap / nk mcap → ubah target ───────────────────────
        if lower.contains("mcap") {
            let target = trimmed.to_string();
            if let (Some(oid), EditField::Mcap) = (st.editing_order_id, &st.edit_field) {
                if let Some(o) = st.orders.iter_mut().find(|o| o.id == oid) {
                    o.target_mcap = target;
                }
                let order = st.orders.iter().find(|o| o.id == oid).cloned();
                st.edit_field = EditField::None;
                st.editing_order_id = None;
                if let Some(o) = order {
                    bot.send_message(msg.chat.id, order_detail_text(&o))
                        .reply_markup(make_order_detail_keyboard(&o))
                        .await?;
                }
            } else {
                st.buy_target_mcap = target;
                if let Some(token) = st.active_token.clone() {
                    send_limitbuy_menu(&bot, msg.chat.id, &token, &st).await?;
                } else {
                    bot.send_message(msg.chat.id, "❌ Paste address token dulu!").await?;
                }
            }
            return Ok(());
        }

        // ── Format n sol → ubah fee ──────────────────────────────────────
        if lower.ends_with("sol") {
            if let Ok(fee) = lower.trim_end_matches("sol").trim().parse::<f64>() {
                if let Some(oid) = st.editing_order_id {
                    match &st.edit_field {
                        EditField::Tip => {
                            if let Some(o) = st.orders.iter_mut().find(|o| o.id == oid) {
                                o.tip_fee = fee;
                            }
                        }
                        EditField::Prio => {
                            if let Some(o) = st.orders.iter_mut().find(|o| o.id == oid) {
                                o.prio_fee = fee;
                            }
                        }
                        _ => {}
                    }
                    let order = st.orders.iter().find(|o| o.id == oid).cloned();
                    st.edit_field = EditField::None;
                    st.editing_order_id = None;
                    if let Some(o) = order {
                        bot.send_message(msg.chat.id, order_detail_text(&o))
                            .reply_markup(make_order_detail_keyboard(&o))
                            .await?;
                    }
                } else {
                    st.buy_tip_fee = fee;
                    st.buy_prio_fee = fee;
                    if let Some(token) = st.active_token.clone() {
                        send_limitbuy_menu(&bot, msg.chat.id, &token, &st).await?;
                    } else {
                        bot.send_message(msg.chat.id, "❌ Paste address token dulu!").await?;
                    }
                }
            }
            return Ok(());
        }

        // ── Format n%pnl → ubah target PNL Auto Limit ────────────────────────
        if lower.contains("%pnl") || lower.ends_with("%") {
            if st.edit_field == EditField::LimitPnl {
                // Bersihkan format jadi canonical misal "50%pnl" -> "50%"
                let clean = lower.replace("%pnl", "%").trim().to_uppercase();
                st.limit_target_pnl = clean.clone();
                st.edit_field = EditField::None;
                let tip = st.limit_tip_fee;
                let prio = st.limit_prio_fee;
                let active = st.auto_limit_active;
                let act_time = st.limit_act_time.clone();
                let pnl = st.limit_target_pnl.clone();
                bot.send_message(msg.chat.id, format!("⚙️ **Pengaturan Auto Limit Sell**\nTarget PNL diubah ke **{}**", clean))
                    .reply_markup(make_autolimit_keyboard(active, tip, prio, &act_time, &pnl))
                    .await?;
            }
            return Ok(());
        }

        // ── Deteksi Solana Address ───────────────────────────────────────
        let is_base58 = trimmed.chars().all(|c| c.is_alphanumeric());
        if trimmed.len() >= 32 && trimmed.len() <= 50 && !trimmed.contains(' ') && is_base58 {
            st.active_token = Some(trimmed.to_string());
            // Reset buy settings untuk token baru
            st.buy_amount_usd = 2.0;
            st.buy_target_mcap = "50 Mcap".to_string();
            st.buy_tip_fee = 0.001;
            st.buy_prio_fee = 0.001;
            st.editing_order_id = None;
            st.edit_field = EditField::None;
            let token = trimmed.to_string();
            send_limitbuy_menu(&bot, msg.chat.id, &token, &st).await?;
        }
    }
    Ok(())
}

// ─── Callback Handler ─────────────────────────────────────────────────────────

async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<Mutex<BotState>>,
) -> ResponseResult<()> {
    if let Some(data) = q.data {
        let mut st = state.lock().await;
        st.limiter.until_ready().await;

        let chat_id = if let Some(msg) = &q.message { msg.chat().id } else { return Ok(()); };
        let msg_id  = if let Some(msg) = &q.message { msg.id() }      else { return Ok(()); };

        // ── Prefix-based routing untuk order actions ─────────────────────
        if data.starts_with("order_view_") {
            let id: usize = data.trim_start_matches("order_view_").parse().unwrap_or(0);
            if let Some(order) = st.orders.iter().find(|o| o.id == id).cloned() {
                bot.edit_message_text(chat_id, msg_id, order_detail_text(&order))
                    .reply_markup(make_order_detail_keyboard(&order))
                    .await?;
            }
            bot.answer_callback_query(q.id).await?;
            return Ok(());
        }

        if data.starts_with("delete_order_") {
            let id: usize = data.trim_start_matches("delete_order_").parse().unwrap_or(0);
            st.orders.retain(|o| o.id != id);
            let text = format!("📋 **Limit Order History**\nTotal: {} order\n\nKlik order untuk detail, edit, atau hapus.", st.orders.len());
            bot.edit_message_text(chat_id, msg_id, text)
                .reply_markup(make_history_keyboard(&st.orders))
                .await?;
            bot.answer_callback_query(q.id).text(format!("Order #{} dihapus!", id)).await?;
            return Ok(());
        }

        if data.starts_with("edit_amount_") {
            let id: usize = data.trim_start_matches("edit_amount_").parse().unwrap_or(0);
            st.editing_order_id = Some(id);
            st.edit_field = EditField::Amount;
            bot.answer_callback_query(q.id).text("Ketik jumlah baru, contoh: 5$").await?;
            bot.send_message(chat_id, "✏️ Ketik jumlah beli baru (contoh: `5$`, `10$`, `25$`)").await?;
            return Ok(());
        }

        if data.starts_with("edit_mcap_") {
            let id: usize = data.trim_start_matches("edit_mcap_").parse().unwrap_or(0);
            st.editing_order_id = Some(id);
            st.edit_field = EditField::Mcap;
            bot.answer_callback_query(q.id).text("Ketik target baru, contoh: 100 mcap").await?;
            bot.send_message(chat_id, "✏️ Ketik target mcap baru (contoh: `100 mcap`, `1k mcap`, `500 mcap`)").await?;
            return Ok(());
        }

        if data.starts_with("edit_tip_") {
            let id: usize = data.trim_start_matches("edit_tip_").parse().unwrap_or(0);
            st.editing_order_id = Some(id);
            st.edit_field = EditField::Tip;
            bot.answer_callback_query(q.id).text("Ketik Tip baru, contoh: 0.002 sol").await?;
            bot.send_message(chat_id, "✏️ Ketik nilai Tip baru (contoh: `0.002 sol`, `0.001 sol`)").await?;
            return Ok(());
        }

        if data.starts_with("edit_prio_") {
            let id: usize = data.trim_start_matches("edit_prio_").parse().unwrap_or(0);
            st.editing_order_id = Some(id);
            st.edit_field = EditField::Prio;
            bot.answer_callback_query(q.id).text("Ketik P.Fee baru, contoh: 0.002 sol").await?;
            bot.send_message(chat_id, "✏️ Ketik nilai P.Fee baru (contoh: `0.002 sol`, `0.001 sol`)").await?;
            return Ok(());
        }

        // ── Static callbacks ─────────────────────────────────────────────
        match data.as_str() {
            "menu_main" => {
                let mode_str = if st.mode == AppMode::Mainnet { "MAINNET" } else { "SIMULASI" };
                let text = format!(
                    "👋 **Selamat datang di Custom Aura Bot!**\nMode saat ini: `{}`\n\nSilakan pilih menu atau **Paste Address Token** untuk Limit Buy.",
                    mode_str
                );
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_main_menu_keyboard())
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_swapsell" => {
                let text = "⚙️ **Pengaturan Swap Sell**\nSemua pengaturan di bawah ini sudah **Fixed (Terkunci)**.";
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_swapsell_keyboard())
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_autolimit" => {
                let text = "⚙️ **Pengaturan Auto Limit Sell**\n\nSlippage, Side, Activation dikunci otomatis.\n\n*Klik 🎯 Target PNL untuk ubah, lalu ketik mis. `50%pnl`*";
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(
                        st.auto_limit_active,
                        st.limit_tip_fee,
                        st.limit_prio_fee,
                        &st.limit_act_time.clone(),
                        &st.limit_target_pnl.clone(),
                    ))
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "menu_history" => {
                let text = format!(
                    "📋 **Limit Order History**\nTotal: {} order\n\nKlik order untuk detail, edit, atau hapus.",
                    st.orders.len()
                );
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_history_keyboard(&st.orders))
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "toggle_autolimit" => {
                st.auto_limit_active = !st.auto_limit_active;
                let text = "⚙️ **Pengaturan Auto Limit Sell**\n\nSlippage, Side, Activation dikunci otomatis.";
                bot.edit_message_text(chat_id, msg_id, text)
                    .reply_markup(make_autolimit_keyboard(
                        st.auto_limit_active,
                        st.limit_tip_fee,
                        st.limit_prio_fee,
                        &st.limit_act_time.clone(),
                        &st.limit_target_pnl.clone(),
                    ))
                    .await?;
                bot.answer_callback_query(q.id).text("Status Auto Limit diperbarui!").await?;
            }
            "cycle_limit_tip" => {
                st.limit_tip_fee = cycle_fee(st.limit_tip_fee);
                bot.edit_message_reply_markup(chat_id, msg_id)
                    .reply_markup(make_autolimit_keyboard(
                        st.auto_limit_active, st.limit_tip_fee, st.limit_prio_fee,
                        &st.limit_act_time.clone(), &st.limit_target_pnl.clone(),
                    ))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("Tip → {} SOL", st.limit_tip_fee)).await?;
            }
            "cycle_limit_prio" => {
                st.limit_prio_fee = cycle_fee(st.limit_prio_fee);
                bot.edit_message_reply_markup(chat_id, msg_id)
                    .reply_markup(make_autolimit_keyboard(
                        st.auto_limit_active, st.limit_tip_fee, st.limit_prio_fee,
                        &st.limit_act_time.clone(), &st.limit_target_pnl.clone(),
                    ))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("P.Fee → {} SOL", st.limit_prio_fee)).await?;
            }
            "cycle_limit_acttime" => {
                let times = ["0s", "5s", "10s", "30s", "60s"];
                let cur = st.limit_act_time.clone();
                let mut next = times[0];
                for (i, &t) in times.iter().enumerate() {
                    if cur == t {
                        next = times[(i + 1) % times.len()];
                        break;
                    }
                }
                st.limit_act_time = next.to_string();
                bot.edit_message_reply_markup(chat_id, msg_id)
                    .reply_markup(make_autolimit_keyboard(
                        st.auto_limit_active, st.limit_tip_fee, st.limit_prio_fee,
                        &st.limit_act_time.clone(), &st.limit_target_pnl.clone(),
                    ))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("Act.Time → {}", next)).await?;
            }
            "set_limit_pnl" => {
                st.edit_field = EditField::LimitPnl;
                bot.answer_callback_query(q.id).text("Ketik target PNL, contoh: 50%pnl").await?;
                bot.send_message(chat_id, "✏️ **Ketik target PNL untuk Auto Limit Sell:**\nContoh: `50%pnl`, `100%pnl`, `200%pnl`").await?;
            }
            "cycle_buy_tip" => {
                st.buy_tip_fee = cycle_fee(st.buy_tip_fee);
                bot.edit_message_reply_markup(chat_id, msg_id)
                    .reply_markup(make_limitbuy_keyboard(&st))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("Buy Tip → {} SOL", st.buy_tip_fee)).await?;
            }
            "cycle_buy_prio" => {
                st.buy_prio_fee = cycle_fee(st.buy_prio_fee);
                bot.edit_message_reply_markup(chat_id, msg_id)
                    .reply_markup(make_limitbuy_keyboard(&st))
                    .await?;
                bot.answer_callback_query(q.id).text(format!("Buy P.Fee → {} SOL", st.buy_prio_fee)).await?;
            }
            "place_limit_buy" => {
                if st.active_token.is_none() {
                    bot.answer_callback_query(q.id).text("Error: Token tidak aktif!").await?;
                    return Ok(());
                }
                let token = st.active_token.clone().unwrap();
                let order = LimitOrder {
                    id: st.next_order_id,
                    token: token.clone(),
                    amount_usd: st.buy_amount_usd,
                    target_mcap: st.buy_target_mcap.clone(),
                    tip_fee: st.buy_tip_fee,
                    prio_fee: st.buy_prio_fee,
                };
                let oid = order.id;
                st.orders.push(order);
                st.next_order_id += 1;

                let is_mainnet = st.mode == AppMode::Mainnet;
                let short = format!("{}...{}", &token[..4], &token[token.len()-4..]);
                let response_text = if is_mainnet {
                    format!("⚠️ [MAINNET] Limit Buy Order #{} dikirim!\nToken: {}\nTarget: {}\nJumlah: ${:.2}", oid, short, st.buy_target_mcap, st.buy_amount_usd)
                } else {
                    format!("🟢 [SIMULASI] Limit Buy Order #{} disimpan!\nToken: {}\nTarget: {}\nJumlah: ${:.2}\n\nCek di menu 📋 Limit Order History.", oid, short, st.buy_target_mcap, st.buy_amount_usd)
                };

                bot.send_message(chat_id, response_text).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "execute_swap_sell" => {
                let is_mainnet = st.mode == AppMode::Mainnet;
                let response_text = if is_mainnet {
                    "⚠️ [MAINNET] Mengirim perintah SWAP SELL!\nPriority Fee: 0.0015 SOL\nTip: 0.0015 SOL\nSlippage: 95%".to_string()
                } else {
                    "🟢 [SIMULASI] Swap Sell diproses!\nPriority Fee: 0.0015 SOL\nTip: 0.0015 SOL\nSlippage: 95%\n(Aman, tidak ada transaksi sungguhan).".to_string()
                };
                bot.send_message(chat_id, response_text).await?;
                bot.answer_callback_query(q.id).await?;
            }
            "refresh_pnl" => {
                let is_mainnet = st.mode == AppMode::Mainnet;
                let dummy_pnl = if is_mainnet { "+25.0% (Mainnet)" } else { "+12.5% (Simulated)" };
                let updated_text = format!(
                    "🟢 **Limit Buy Terpicu!**\nToken: $AURA\nHarga Beli: $0.15\n\n📊 PNL: {}\n\nApa yang ingin Anda lakukan?",
                    dummy_pnl
                );
                let keyboard = InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::callback("🔴 Confirm Swap Sell", "execute_swap_sell")],
                    vec![InlineKeyboardButton::callback("🔄 Refresh PNL", "refresh_pnl")],
                ]);
                bot.edit_message_text(chat_id, msg_id, updated_text)
                    .reply_markup(keyboard)
                    .await?;
                bot.answer_callback_query(q.id).text("PNL diperbarui!").await?;
            }
            "none" => {
                bot.answer_callback_query(q.id).text("Tombol ini dikunci (Fixed Setting).").await?;
            }
            _ => {}
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn cycle_fee(current: f64) -> f64 {
    let vals = [0.001, 0.0015, 0.002, 0.003, 0.005];
    for (i, &v) in vals.iter().enumerate() {
        if (current - v).abs() < 1e-9 {
            return vals[(i + 1) % vals.len()];
        }
    }
    vals[0]
}
