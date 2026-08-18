use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use teloxide::{prelude::*, utils::command::BotCommands};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use log::{info, error};

use tonic::transport::Endpoint;
use tonic::{Request, Status};
use tokio_stream::StreamExt;

// aura_api_client
use aura_api_client::client::AuraClients;
use aura_api_client::client_ext::UserCtxInterceptor;
use aura_api_client::types::UserActionEventSub;

/// A no-op UserCtxInterceptor: all per-call context is handled by the
/// connection-level `auth_interceptor` function, so we don't need a per-call payload.
#[derive(Clone, Copy)]
struct NoCtx;

impl UserCtxInterceptor for NoCtx {
    type Payload = ();
    fn intercept<T>(_payload: (), _req: &mut tonic::Request<T>) -> Result<(), tonic::Status> {
        Ok(())
    }
}

mod db;
use db::{init_db, load_settings, load_chats};

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
    #[command(description = "Bersihkan pesan sebelumnya untuk tampilan lebih rapi.")]
    Clear,
    #[command(description = "Tampilkan kembali panel Swap Sell jika tertumpuk.")]
    Panel,
}

#[derive(Clone)]
struct LimitOrder {
    id: usize,
    token: String,
    amount_usd: f64,
    target: String,       // unified target: mcap / price-usd / persen-change
    tip_fee: f64,
    prio_fee: f64,
}

/// Preset tip/prio untuk quick-set (Kecil/Sedang/Besar/Mega)
#[derive(Clone)]
struct TxPreset {
    pub label: String,
    pub tip: f64,
    pub prio: f64,
}

impl TxPreset {
    fn new(label: &str, tip: f64, prio: f64) -> Self {
        TxPreset { label: label.to_string(), tip, prio }
    }
}

/// Index preset aktif untuk quick-set buy (None = tidak ada yang aktif)
#[derive(Clone, PartialEq)]
enum ActivePreset {
    None,
    Idx(usize),
}

#[derive(Clone, PartialEq)]
#[allow(dead_code)]
enum EditField {
    None,
    // Limit Buy (Baru)
    BuyAmount,
    BuyTarget,            // unified: mcap / price-usd / persen-change
    BuyTip,
    BuyPrio,
    // Quick preset editing (idx preset, field: 0=tip 1=prio)
    BuyPresetTip(usize),
    BuyPresetPrio(usize),
    // Auto Limit
    AutoTip,
    AutoPrio,
    AutoActTime,
    AutoPnl,
    // Swap Sell Panel
    SellTip,
    SellPrio,
    SellSlippage,
    // History edit (by DB id)
    HistTarget(i64),
    HistTipById(i64),
    HistPrioById(i64),
    // Limit Order Setup preset fields (idx: 0=kecil,1=sedang,2=besar,3=mega)
    SetupPresetTip(usize),
    SetupPresetPrio(usize),
    // Legacy (kept for compat)
    HistAmount(usize),
    HistMcap(usize),
    HistTip(usize),
    HistPrio(usize),
}

type InterceptorFn = fn(Request<()>) -> Result<Request<()>, Status>;

struct BotState {
    aura_client: Option<AuraClients<InterceptorFn, NoCtx>>,
    #[allow(dead_code)]
    aura_api_key: String,
    mode: AppMode,
    limiter: Arc<governor::DefaultDirectRateLimiter>,
    db_conn: Arc<Mutex<rusqlite::Connection>>,
    edit_field: EditField,

    // Auto Limit Sell Settings
    auto_limit_active: bool,
    limit_tip_fee: f64,
    limit_prio_fee: f64,
    limit_act_time: String,
    limit_target_pnl: String,

    // Swap Sell Settings
    sell_tip_fee: f64,
    sell_prio_fee: f64,
    sell_slippage: String,

    // Manual Limit Buy Settings (mode pembuatan order baru)
    active_token: Option<String>,
    buy_amount_usd: f64,
    buy_target: String,          // unified target: mcap / price-usd / persen-change
    buy_tip_fee: f64,
    buy_prio_fee: f64,

    // Quick-set presets untuk Tip & Prio (Kecil/Sedang/Besar/Mega)
    buy_presets: Vec<TxPreset>,
    buy_active_preset: ActivePreset,

    // Preset values (from setup menu)
    preset_kecil_tip: f64,
    preset_kecil_prio: f64,
    preset_sedang_tip: f64,
    preset_sedang_prio: f64,
    preset_besar_tip: f64,
    preset_besar_prio: f64,
    preset_mega_tip: f64,
    preset_mega_prio: f64,

    // History of limit orders (runtime cache, source of truth is db)
    orders: Vec<LimitOrder>,
    next_order_id: usize,

    // Active chats for notifications
    active_chats: std::collections::HashSet<ChatId>,

    // Track panel swap sell (chat_id, msg_id) agar bisa di-edit saat auto limit terpenuhi
    swap_panel_msgs: Vec<(ChatId, teloxide::types::MessageId)>,

    // Deduplikasi tx signature dari stream
    processed_signatures: std::collections::HashSet<String>,
}

impl BotState {
    pub fn get_db_settings(&self) -> db::DbSettings {
        db::DbSettings {
            auto_limit_active: self.auto_limit_active,
            limit_tip_fee: self.limit_tip_fee,
            limit_prio_fee: self.limit_prio_fee,
            limit_act_time: self.limit_act_time.clone(),
            limit_target_pnl: self.limit_target_pnl.clone(),
            sell_tip_fee: self.sell_tip_fee,
            sell_prio_fee: self.sell_prio_fee,
            sell_slippage: self.sell_slippage.clone(),
            preset_kecil_tip: self.preset_kecil_tip,
            preset_kecil_prio: self.preset_kecil_prio,
            preset_sedang_tip: self.preset_sedang_tip,
            preset_sedang_prio: self.preset_sedang_prio,
            preset_besar_tip: self.preset_besar_tip,
            preset_besar_prio: self.preset_besar_prio,
            preset_mega_tip: self.preset_mega_tip,
            preset_mega_prio: self.preset_mega_prio,
        }
    }

    pub fn save_db(&self) {
        if let Ok(conn) = self.db_conn.try_lock() {
            let set = self.get_db_settings();
            let _ = db::save_settings(&conn, &set);
        }
    }

    /// Sync buy_presets vec from the preset_* fields
    pub fn sync_presets(&mut self) {
        self.buy_presets = vec![
            TxPreset::new("Kecil", self.preset_kecil_tip, self.preset_kecil_prio),
            TxPreset::new("Sedang", self.preset_sedang_tip, self.preset_sedang_prio),
            TxPreset::new("Besar", self.preset_besar_tip, self.preset_besar_prio),
            TxPreset::new("Mega", self.preset_mega_tip, self.preset_mega_prio),
        ];
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
    pretty_env_logger::init();
    info!("Memulai Aura Custom Bot...");

    let bot = Bot::from_env();
    let api_key = env::var("AURA_API_KEY").unwrap_or_else(|_| "DUMMY_KEY".to_string()).trim().to_string();
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
        let endpoint = Endpoint::from_static("http://trade.aura.rehab:40051")
            .http2_keep_alive_interval(std::time::Duration::from_secs(30))
            .keep_alive_timeout(std::time::Duration::from_secs(10))
            .keep_alive_while_idle(true);
        match endpoint.connect().await {
            Ok(channel) => {
                let interceptor: fn(Request<()>) -> Result<Request<()>, Status> = auth_interceptor;
                let clients = AuraClients::<_, NoCtx>::new(channel, interceptor);
                aura_clients_opt = Some(clients);
                info!("Berhasil terhubung ke Aura gRPC (trade.aura.rehab:40051)");
            }
            Err(e) => {
                error!("Gagal koneksi ke gRPC Aura: {:?}", e);
            }
        }
    }

    // Initialize Database
    let conn = init_db().expect("Gagal inisialisasi SQLite database");
    let loaded_settings = load_settings(&conn).unwrap_or_default();
    
    // Pre-populate active_chats dari env var TELEGRAM_CHAT_ID dan Database
    let mut initial_chats = load_chats(&conn).unwrap_or_default();
    if let Ok(chat_id_str) = env::var("TELEGRAM_CHAT_ID") {
        for part in chat_id_str.split(',') {
            if let Ok(id) = part.trim().parse::<i64>() {
                initial_chats.insert(teloxide::types::ChatId(id));
                let _ = db::save_chat(&conn, teloxide::types::ChatId(id));
                info!("Chat ID {} ditambahkan dari env TELEGRAM_CHAT_ID", id);
            }
        }
    }

    let mut st = BotState {
        aura_api_key: api_key,
        mode: initial_mode,
        aura_client: aura_clients_opt.clone(),
        limiter,
        db_conn: Arc::new(Mutex::new(conn)),
        edit_field: EditField::None,
        auto_limit_active: loaded_settings.auto_limit_active,
        limit_tip_fee: loaded_settings.limit_tip_fee,
        limit_prio_fee: loaded_settings.limit_prio_fee,
        limit_act_time: loaded_settings.limit_act_time,
        limit_target_pnl: loaded_settings.limit_target_pnl,
        sell_tip_fee: loaded_settings.sell_tip_fee,
        sell_prio_fee: loaded_settings.sell_prio_fee,
        sell_slippage: loaded_settings.sell_slippage,
        preset_kecil_tip: loaded_settings.preset_kecil_tip,
        preset_kecil_prio: loaded_settings.preset_kecil_prio,
        preset_sedang_tip: loaded_settings.preset_sedang_tip,
        preset_sedang_prio: loaded_settings.preset_sedang_prio,
        preset_besar_tip: loaded_settings.preset_besar_tip,
        preset_besar_prio: loaded_settings.preset_besar_prio,
        preset_mega_tip: loaded_settings.preset_mega_tip,
        preset_mega_prio: loaded_settings.preset_mega_prio,
        active_token: None,
        buy_amount_usd: 2.0,
        buy_target: "50 Mcap".to_string(),
        buy_tip_fee: 0.001,
        buy_prio_fee: 0.001,
        buy_presets: Vec::new(),
        buy_active_preset: ActivePreset::None,
        orders: Vec::new(),
        next_order_id: 1,
        active_chats: initial_chats,
        swap_panel_msgs: Vec::new(),
        processed_signatures: std::collections::HashSet::new(),
    };
    st.sync_presets();
    let state = Arc::new(Mutex::new(st));

    // Start UserActivity Stream and Ping if client is available
    if let Some(clients) = aura_clients_opt {
        let clients_ping = clients.clone();

        // Shared flag: ping dimulai HANYA setelah stream user_activity tersambung
        let stream_ready = Arc::new(AtomicBool::new(false));
        let stream_ready_ping = stream_ready.clone();

        // 1. Ping Loop — WAJIB setiap 10 detik per README Aura agar stream tetap hidup
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            use aura_api_client::types::Ping as AuraPing;
            let mut ping_count: u64 = 0;
            let mut consecutive_failures: u32 = 0;
            loop {
                interval.tick().await;

                // Tunggu sampai stream user_activity sudah tersambung
                if !stream_ready_ping.load(Ordering::Relaxed) {
                    info!("⏳ Menunggu UserActivity stream tersambung sebelum ping...");
                    continue;
                }

                ping_count += 1;
                let req = Request::new(AuraPing { count: ping_count });
                match clients_ping.aura().user_ping((), req).await {
                    Ok(_) => {
                        consecutive_failures = 0;
                        if ping_count == 1 {
                            info!("✅ Ping pertama ke server Aura berhasil! Stream aktif.");
                        }
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        error!("❌ Gagal ping Aura (#{ping_count}): {:?}", e);
                        // Jika gagal >3x berturut, reset flag stream agar listener bisa reconnect
                        if consecutive_failures >= 3 {
                            error!("⚠️ Ping gagal {} kali berturut — reset stream_ready flag", consecutive_failures);
                            stream_ready_ping.store(false, Ordering::Relaxed);
                            consecutive_failures = 0;
                        }
                    }
                }
            }
        });

        // 2. UserActivity Stream Listener

        let st_clone = state.clone();
        let bot_clone = bot.clone();
        let stream_ready_listener = stream_ready.clone();
        tokio::spawn(async move {
            loop {
                info!("Menyambungkan UserActivity Stream...");
                let req = Request::new(UserActionEventSub {});
                match clients.aura().user_activity((), req).await {
                    Ok(resp) => {
                        let mut stream = resp.into_inner();
                        info!("UserActivity Stream tersambung!");
                        // Aktifkan flag → ping loop bisa mulai
                        stream_ready_listener.store(true, Ordering::Relaxed);
                        while let Some(msg) = stream.next().await {
                            match msg {
                                Ok(action) => {
                                    use aura_api_client::types::{
                                        UserAction, TradeStateUpdate, TxnConfirmState,
                                        ParsedTradeUi,
                                    };

                                    // ── Pong/Ping: log saja, skip ──
                                    if matches!(action, UserAction::Pong(_) | UserAction::Ping(_)) {
                                        continue;
                                    }

                                    // ── SELALU log ke server (bukan Telegram) ──
                                    info!("[Aura Stream] {:?}", action);

                                    // ── Deteksi SELL on-chain (auto limit terpenuhi) ──
                                    // Ini harus dicek SEBELUM BUY agar tidak tertukar
                                    if let UserAction::TradeCallback(TradeStateUpdate::Landed {
                                        state: TxnConfirmState::Confirmed { trades, .. }, ..
                                    }) = &action {
                                        let has_sell = trades.iter().any(|t| matches!(t, ParsedTradeUi::Sell { .. }));
                                        if has_sell {
                                            // Ambil detail sell dari trade pertama yang Sell
                                            let sell_info = trades.iter().find_map(|t| {
                                                if let ParsedTradeUi::Sell { mint, ticker, quote, pnl, .. } = t {
                                                    Some((format!("{}", mint), ticker.clone(), quote.clone(), pnl.clone()))
                                                } else {
                                                    None
                                                }
                                            });

                                            if let Some((mint_str, ticker, quote_val, pnl_val)) = sell_info {
                                                info!("[Aura Stream] Terdeteksi SELL berhasil on-chain! Token={}", mint_str);

                                                let (panel_msgs, chats) = {
                                                    let st = st_clone.lock().await;
                                                    (st.swap_panel_msgs.clone(), st.active_chats.clone())
                                                };

                                                let pnl_str = pnl_val
                                                    .map(|p| format!("{:.2}", p))
                                                    .unwrap_or_else(|| "N/A".to_string());

                                                // quote adalah QuoteLamports (lamports token quote / SOL)
                                                // format sebagai SOL (÷ 1e9)
                                                let sol_received = {
                                                    let raw: u64 = quote_val.into();
                                                    raw as f64 / 1e9
                                                };

                                                let ticker_display = if ticker.is_empty() {
                                                    let s = &mint_str;
                                                    format!("{}...{}", &s[..6.min(s.len())], &s[s.len().saturating_sub(4)..])
                                                } else {
                                                    ticker.clone()
                                                };

                                                let sold_text = format!(
                                                    "✅ *Token Terjual via Auto Limit!*\n\n\
                                                    🏦 Token: `{}` ({})\n\
                                                    💰 SOL Diterima: `{:.6}` SOL\n\
                                                    📈 PNL: `{}%`\n\n\
                                                    _Auto Limit Sell telah dieksekusi otomatis oleh Aura._",
                                                    ticker_display, mint_str, sol_received, pnl_str
                                                );

                                                // Edit semua panel swap sell yang tersimpan
                                                for (chat_id, msg_id) in &panel_msgs {
                                                    let _ = bot_clone
                                                        .edit_message_text(*chat_id, *msg_id, &sold_text)
                                                        .await;
                                                }

                                                // Jika tidak ada panel tersimpan, kirim pesan baru
                                                if panel_msgs.is_empty() {
                                                    for chat in &chats {
                                                        let _ = bot_clone.send_message(*chat, &sold_text).await;
                                                    }
                                                }

                                                // Clear panel msgs setelah terjual
                                                {
                                                    let mut st = st_clone.lock().await;
                                                    st.swap_panel_msgs.clear();
                                                }
                                                continue; // skip ke event berikutnya
                                            }
                                        }
                                    }

                                    // ── Deteksi event BUY yang presisi berdasarkan enum proto ──
                                    // Hanya menggunakan TradeCallback::Landed agar terpicu 1x saat tx sukses on-chain
                                    let (is_buy_confirmed, mint_from_event, txn_sig_opt) = match &action {
                                        UserAction::TradeCallback(update) => {
                                            match update {
                                                TradeStateUpdate::Landed { signature, state } => {
                                                    match state {
                                                        TxnConfirmState::Confirmed { trades, .. } => {
                                                            let has_buy = trades.iter().any(|t| matches!(t, ParsedTradeUi::Buy { .. }));
                                                            let mint_str = trades.iter().find_map(|t| {
                                                                if let ParsedTradeUi::Buy { mint, .. } = t {
                                                                    Some(format!("{}", mint))
                                                                } else {
                                                                    None
                                                                }
                                                            });
                                                            (has_buy, mint_str, Some(format!("{}", signature)))
                                                        }
                                                        _ => (false, None, None),
                                                    }
                                                }
                                                TradeStateUpdate::Lost { .. } => (false, None, None),
                                            }
                                        }
                                        _ => (false, None, None),
                                    };

                                    if is_buy_confirmed {
                                        let sig = txn_sig_opt.unwrap_or_else(|| "unknown".to_string());
                                        // Deduplikasi berdasarkan signature
                                        {
                                            let mut st = st_clone.lock().await;
                                            if st.processed_signatures.contains(&sig) {
                                                continue;
                                            }
                                            st.processed_signatures.insert(sig.clone());
                                        }
                                        
                                        info!("[Aura Stream] Terdeteksi transaksi BUY berhasil! Sig: {}", sig);

                                        // Ambil settings dari state
                                        let (active_token, auto_limit_active, limit_tip, limit_prio, limit_pnl, _limit_time, chats) = {
                                            let mut st = st_clone.lock().await;
                                            if let Some(ref mint) = mint_from_event {
                                                if mint.len() >= 32 {
                                                    st.active_token = Some(mint.clone());
                                                }
                                            }
                                            (
                                                st.active_token.clone(),
                                                st.auto_limit_active,
                                                st.limit_tip_fee,
                                                st.limit_prio_fee,
                                                st.limit_target_pnl.clone(),
                                                st.limit_act_time.clone(),
                                                st.active_chats.clone(),
                                            )
                                        };

                                        let token_display = active_token.as_deref().unwrap_or("Unknown Token");

                                        // Persiapkan Teks Swap Panel (belum dikirim)
                                        let auto_info = if auto_limit_active {
                                            format!("🤖 Auto Limit Sell aktif — target PNL {}%\n\n", limit_pnl)
                                        } else {
                                            String::new()
                                        };
                                        let _swap_text = format!(
                                            "🟢 *Pembelian Terdeteksi!*\n\n🏦 Token: `{}`\n\n{}*Klik tombol di bawah untuk Swap Sell atau lihat PNL:*",
                                            token_display, auto_info
                                        );
                                        let _swap_keyboard = teloxide::types::InlineKeyboardMarkup::new(vec![
                                            vec![teloxide::types::InlineKeyboardButton::callback(
                                                "🔴 Confirm Swap Sell", "execute_swap_sell",
                                            )],
                                            vec![teloxide::types::InlineKeyboardButton::callback(
                                                "🔄 Refresh PNL", "refresh_pnl",
                                            )],
                                        ]);

                                        // ── Auto Limit Sell: tempatkan order ke Aura gRPC jika aktif ──
                                        if auto_limit_active {
                                            if let Some(ref token) = active_token {
                                                use aura_api_client::types::{
                                                    ApiLimitOrder, ApiOrders, Direction, OrderEventTrigger,
                                                    OrderState, RawOrder, SwapAmount, Target, TxnProcessors,
                                                    UpdateTokenLimitOrders, UserNonceStrategy,
                                                };
                                                use std::str::FromStr;

                                                let mut st = st_clone.lock().await;
                                                let order_id = st.next_order_id;
                                                let sell_slippage = st.sell_slippage.clone();
                                                let new_order = LimitOrder {
                                                    id: order_id,
                                                    token: token.clone(),
                                                    amount_usd: 100.0,
                                                    target: format!("{}%", limit_pnl),
                                                    tip_fee: limit_tip,
                                                    prio_fee: limit_prio,
                                                };
                                                st.orders.push(new_order.clone());
                                                st.next_order_id += 1;
                                                // Save to SQLite
                                                let db_conn_clone = st.db_conn.clone();
                                                if let Ok(conn) = db_conn_clone.try_lock() {
                                                    let _ = db::insert_limit_order(&conn, "SELL", token, &format!("PNL {}%", limit_pnl), limit_tip, limit_prio);
                                                }
                                                let client_opt = st.aura_client.clone();
                                                let db_conn_for_err = st.db_conn.clone();
                                                drop(st);

                                                info!(
                                                    "[Auto Limit] Order #{} akan dikirim ke Aura: Token={}, PNL={}%, Tip={}, Prio={}",
                                                    order_id, token, limit_pnl, limit_tip, limit_prio
                                                );

                                                // Kirim ke Aura gRPC
                                                if let Some(client) = client_opt {
                                                    if let Ok(mint_addr) = solana_address::Address::from_str(token) {
                                                        let tip_lam = (limit_tip * 1e9) as u64;
                                                        let prio_lam = (limit_prio * 1e9) as u64;

                                                        let pnl_f64 = limit_pnl.parse::<f64>().unwrap_or(100.0);
                                                        let pnl_scaled = ((1.0 + pnl_f64 / 100.0) * 1_000_000f64) as u64;
                                                        let profit_perc = fastnum::UD128::from(pnl_scaled) / fastnum::UD128::from(1_000_000u64);

                                                        let slippage_f64 = sell_slippage.replace("%", "").trim().parse::<f64>().unwrap_or(90.0);
                                                        let slippage_scaled = (slippage_f64 / 100.0 * 1_000_000.0) as u64;
                                                        let slippage_val = fastnum::UD128::from(slippage_scaled) / fastnum::UD128::from(1_000_000u64);

                                                        let api_order = ApiLimitOrder {
                                                            state: OrderState::Api {
                                                                id: None,
                                                                expire_dur: None,
                                                                activate_dur: None,
                                                            },
                                                            order: RawOrder {
                                                                slippage: slippage_val,
                                                                tip: decisol::Lamports::from(tip_lam),
                                                                fee: decisol::Lamports::from(prio_lam),
                                                                target: Target::Profit {
                                                                    init_profit_perc: profit_perc,
                                                                    recalced_profit: None,
                                                                    direction: Direction::Above,
                                                                },
                                                                amount: SwapAmount::SellPerc { amount: fastnum::udec128!(1) },
                                                                procs: TxnProcessors {
                                                                    jito_validators: false,
                                                                    jito_bundled: false,
                                                                    aura: true,
                                                                    bloxroute: false,
                                                                    nozomi: false,
                                                                    next_block: false,
                                                                    slot0: false,
                                                                    astra: false,
                                                                    block_razor: false,
                                                                    node1: false,
                                                                    tpu_penetrator: false,
                                                                    helius: true,
                                                                    stellium: true,
                                                                    soyas: true,
                                                                    falcon: true,
                                                                    raiden: true,
                                                                    circular: true,
                                                                    flash_block: true,
                                                                    moon: true,
                                                                    blocksprint: true,
                                                                    aura_revert: false,
                                                                    landx: true,
                                                                    manka: true,
                                                                    blockrush: true,
                                                                },
                                                                nonce: UserNonceStrategy::Hybrid,
                                                                slot_latency: 0,
                                                            },
                                                            trigger: OrderEventTrigger::Immediate,
                                                            wallet: mint_addr,
                                                        };

                                                        let req = tonic::Request::new(UpdateTokenLimitOrders {
                                                            mint: mint_addr,
                                                            orders: ApiOrders { orders: vec![api_order] },
                                                        });

                                                        let short = if token.len() >= 10 {
                                                            format!("{}...{}", &token[..6], &token[token.len()-4..])
                                                        } else {
                                                            token.clone()
                                                        };

                                                        match client.limit_orders().place_limit_orders((), req).await {
                                                            Ok(_) => {
                                                                let auto_text = format!(
                                                                    "🤖 *Auto Limit Sell Ditempatkan ke Aura!*\n\n🏦 Token: `{}`\n🎯 Target PNL: {}%\n⚡ Tip: {} SOL\n⛽ P.Fee: {} SOL\n📋 Order ID: #{}\n\n_Bot akan otomatis menjual ketika target terpenuhi._",
                                                                    short, limit_pnl, limit_tip, limit_prio, order_id
                                                                );
                                                                for chat in &chats {
                                                                    let _ = bot_clone.send_message(*chat, &auto_text).await;
                                                                }
                                                            }
                                                            Err(e) => {
                                                                let err_text = format!(
                                                                    "❌ Gagal menempatkan Auto Limit Sell ke Aura!\n🏦 Token: `{}`\n🔴 Error: {}",
                                                                    short, e.message()
                                                                );
                                                                // Save error log to SQLite
                                                                if let Ok(conn) = db_conn_for_err.try_lock() {
                                                                    let _ = db::insert_error_log(&conn, order_id as i64, &token, e.message());
                                                                }
                                                                for chat in &chats {
                                                                    let _ = bot_clone.send_message(*chat, &err_text).await;
                                                                }
                                                                error!("[Auto Limit] Gagal place_limit_orders: {:?}", e);
                                                            }
                                                        }
                                                    } else {
                                                        error!("[Auto Limit] Mint address '{}' tidak valid", token);
                                                    }
                                                } else {
                                                    error!("[Auto Limit] Tidak ada koneksi Aura gRPC aktif");
                                                }
                                            }
                                        }

                                        // ── Kirim Swap Sell Panel ke Telegram & simpan msg_id ──
                                        let auto_info = if auto_limit_active {
                                            format!("🤖 Auto Limit Sell aktif — target PNL {}%\n\n", limit_pnl)
                                        } else {
                                            String::new()
                                        };
                                        let swap_text = format!(
                                            "🟢 *Pembelian Terdeteksi!*\n\n🏦 Token: `{}`\n\n{}*Klik tombol di bawah untuk Swap Sell atau lihat PNL:*",
                                            token_display, auto_info
                                        );
                                        let swap_keyboard = teloxide::types::InlineKeyboardMarkup::new(vec![
                                            vec![teloxide::types::InlineKeyboardButton::callback(
                                                "🔴 Confirm Swap Sell", "execute_swap_sell",
                                            )],
                                            vec![teloxide::types::InlineKeyboardButton::callback(
                                                "🔄 Refresh PNL", "refresh_pnl",
                                            )],
                                        ]);

                                        // Kirim panel dan simpan msg_id
                                        let mut new_panels = Vec::new();
                                        for chat in &chats {
                                            if let Ok(sent) = bot_clone
                                                .send_message(*chat, &swap_text)
                                                .reply_markup(swap_keyboard.clone())
                                                .await
                                            {
                                                new_panels.push((*chat, sent.id));
                                            }
                                        }
                                        // Simpan ke state
                                        {
                                            let mut st = st_clone.lock().await;
                                            st.swap_panel_msgs.extend(new_panels);
                                        }
                                    }
                                    // ── Semua event lain: hanya log, TIDAK kirim ke Telegram ──
                                }
                                Err(e) => {
                                    error!("Stream message error: {:?}", e);
                                    break; // keluar untuk reconnect
                                }
                            }
                        }
                        // Stream terputus — nonaktifkan flag agar ping tidak jalan saat reconnect
                        stream_ready_listener.store(false, Ordering::Relaxed);
                        info!("UserActivity Stream terputus, akan reconnect dalam 5 detik...");
                    }
                    Err(e) => {
                        error!("Gagal subscribe UserActivity: {:?}", e);
                        stream_ready_listener.store(false, Ordering::Relaxed);
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
        vec![
            InlineKeyboardButton::callback("📋 Limit Order History", "menu_history"),
            InlineKeyboardButton::callback("⚙️ Limit Order Setup", "menu_lo_setup"),
        ],
        vec![InlineKeyboardButton::callback("📜 Limit Order Logs", "menu_lo_logs")],
    ])
}

fn make_swapsell_keyboard(st: &BotState) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(format!("⚡ Tip | {} SOL", st.sell_tip_fee), "edit_sell_tip"),
            InlineKeyboardButton::callback(format!("⛽ P.Fee | {} SOL", st.sell_prio_fee), "edit_sell_prio"),
        ],
        vec![
            InlineKeyboardButton::callback(format!("🏄‍♂️ Slippage | {}", st.sell_slippage), "edit_sell_slippage"),
            InlineKeyboardButton::callback("🔄 Refresh PNL", "menu_swapsell"),
        ],
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
        vec![InlineKeyboardButton::callback(format!("🏄‍♂️ Slippage | {}", st.sell_slippage), "edit_sell_slippage")],
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
    // ── Tip & Prio aktif (dari preset aktif atau manual) ────────────────────────
    let tip_label = format!("⚡ Tip | {} SOL", st.buy_tip_fee);
    let prio_label = format!("⛽ P.Fee | {} SOL", st.buy_prio_fee);

    // ── Target unified label ─────────────────────────────────────────────────────
    let target_label = if st.buy_target.is_empty() {
        "🎯 Target | (belum diset)".to_string()
    } else {
        format!("🎯 Target | {}", format_target_display(&st.buy_target))
    };

    let mut rows: Vec<Vec<InlineKeyboardButton>> = vec![];

    // Row manual Tip & Prio
    rows.push(vec![
        InlineKeyboardButton::callback(tip_label, "edit_buy_tip"),
        InlineKeyboardButton::callback(prio_label, "edit_buy_prio"),
    ]);

    // ── Row preset Kecil/Sedang/Besar/Mega: 4 tombol sejajar 1 baris, 2 baris tiap tombol ──
    // Telegram tidak support newline di button text, jadi format: "Kecil\nT:x P:y"
    // Kita bikin 4 tombol 1 baris saja
    let mut preset_row: Vec<InlineKeyboardButton> = vec![];
    for (i, p) in st.buy_presets.iter().enumerate() {
        let active = st.buy_active_preset == ActivePreset::Idx(i);
        let check = if active { "✅" } else { "" };
        let label = format!("{}{}\nT:{} P:{}", check, p.label, p.tip, p.prio);
        preset_row.push(InlineKeyboardButton::callback(label, format!("preset_select_{}", i)));
    }
    rows.push(preset_row);

    // ── Row edit nilai preset yang aktif (Tip & Prio-nya) ───────────────────────
    if let ActivePreset::Idx(idx) = &st.buy_active_preset {
        let p = &st.buy_presets[*idx];
        rows.push(vec![
            InlineKeyboardButton::callback(
                format!("✏️ {} Tip | {} SOL", p.label, p.tip),
                format!("preset_edit_tip_{}", idx),
            ),
            InlineKeyboardButton::callback(
                format!("✏️ {} Prio | {} SOL", p.label, p.prio),
                format!("preset_edit_prio_{}", idx),
            ),
        ]);
    }

    // Row Slippage
    rows.push(vec![InlineKeyboardButton::callback("🏄‍♂️ Slippage | 90%", "none")]);
    // Row Side & Dip
    rows.push(vec![
        InlineKeyboardButton::callback("Side | BUY", "none"),
        InlineKeyboardButton::callback("Dip", "none"),
    ]);
    // Row Activation & Amount
    rows.push(vec![
        InlineKeyboardButton::callback("Activation | Instant", "none"),
        InlineKeyboardButton::callback(format!("💰 {:.2} $", st.buy_amount_usd), "edit_buy_amount"),
    ]);
    // Row Target (satu tombol unified)
    rows.push(vec![InlineKeyboardButton::callback(target_label, "edit_buy_target")]);
    // Row Place Order
    rows.push(vec![InlineKeyboardButton::callback("📥 Place Order 📥", "place_limit_buy")]);
    // Row Back
    rows.push(vec![InlineKeyboardButton::callback("<< Back", "menu_main")]);

    InlineKeyboardMarkup::new(rows)
}

/// Format harga untuk tampilan singkat (subscript zeros):
/// 0.0000005$ → 0.0₆5$  (subscript digit menunjukkan jumlah nol)
fn format_price_short(price_str: &str) -> String {
    let s = price_str.trim().trim_end_matches('$').trim();
    if let Ok(val) = s.parse::<f64>() {
        if val > 0.0 && val < 1.0 {
            let after_dot = s.splitn(2, '.').nth(1).unwrap_or("");
            let leading_zeros = after_dot.chars().take_while(|&c| c == '0').count();
            if leading_zeros >= 2 {
                let sig = after_dot.trim_start_matches('0');
                let sub = to_subscript(leading_zeros);
                return format!("0.0{}{}$", sub, sig);
            }
        }
        return format!("{}$", s);
    }
    price_str.to_string()
}

/// Format target unified: detect apakah mcap, price, atau persen
fn format_target_display(target: &str) -> String {
    let s = target.trim();
    let lower = s.to_lowercase();

    // Persen change: mengandung '%'
    if s.contains('%') {
        return s.to_string();
    }

    // McAp: mengandung "mcap", "k", "m", dsb – atau tidak ada '$' dan tidak bisa di-parse sebagai angka kecil
    if lower.contains("mcap") || lower.contains("cap") {
        return s.to_string();
    }

    // Coba parse sebagai price (USD)
    let stripped = s.trim_start_matches('$').trim_end_matches('$').trim();
    if let Ok(val) = stripped.parse::<f64>() {
        if val < 1.0 && val > 0.0 {
            return format_price_short(stripped);
        }
        return format!("{}$", stripped);
    }

    // Fallback: tampilkan apa adanya
    s.to_string()
}

/// Konversi angka ke subscript unicode (untuk notasi jumlah nol)
fn to_subscript(n: usize) -> String {
    let digits = ['₀','₁','₂','₃','₄','₅','₆','₇','₈','₉'];
    n.to_string().chars().map(|c| {
        let d = c.to_digit(10).unwrap_or(0) as usize;
        digits[d]
    }).collect()
}

/// Parse input target universal dari user:
/// - McAp: "100000 mcap" | "30K Mcap" | "11M mcap" | "2.35M mcap"
/// - Price USD: "0.001$" | "$1" | "0.000005$"
/// - Persen: "80%" | "-20%" | "%80"
/// Mengembalikan (target_normalized, display_label) atau None jika tidak valid
fn parse_target_input(input: &str) -> Option<String> {
    let s = input.trim();
    let lower = s.to_lowercase();

    // Persen: mengandung '%'
    if lower.contains('%') {
        // normalize: hilangkan spasi, pastikan % ada di akhir atau awal
        let clean = s.replace('%', "").trim().to_string();
        if let Ok(val) = clean.parse::<f64>() {
            if val == val { // valid number
                // kembalikan dengan tanda % di akhir
                return Some(format!("{}%", val));
            }
        }
        // Coba handle format "-20%" atau "80%"
        let num_part: String = s.chars().filter(|&c| c == '-' || c == '.' || c.is_ascii_digit()).collect();
        if let Ok(val) = num_part.parse::<f64>() {
            return Some(format!("{}%", val));
        }
        return None;
    }

    // Price USD: mengandung '$' atau bisa diparse sebagai angka kecil/besar
    let stripped_dollar = s.trim_start_matches('$').trim_end_matches('$').trim();
    if s.contains('$') {
        if let Ok(val) = stripped_dollar.parse::<f64>() {
            if val > 0.0 {
                return Some(format!("{}", val));
            }
        }
        return None;
    }

    // McAp: mengandung "mcap", "k", "m", dsb
    if lower.contains("mcap") || lower.contains("cap") {
        return Some(s.to_string());
    }

    // Angka dengan suffix K/M untuk mcap
    if lower.ends_with('k') || lower.ends_with('m') {
        return Some(s.to_string());
    }

    // Coba parse sebagai angka murni: jika < 1 → price, jika besar → mcap
    if let Ok(val) = s.parse::<f64>() {
        if val <= 0.0 { return None; }
        if val < 100.0 {
            // Kemungkinan price USD
            return Some(format!("{}", val));
        } else {
            // Kemungkinan mcap (angka besar)
            return Some(format!("{} Mcap", val));
        }
    }

    None
}


fn make_order_inline_keyboard(id: i64, st: &BotState) -> InlineKeyboardMarkup {
    // 4 preset tombol sejajar 1 baris, format 2 baris: "Kecil\nT:x P:y"
    let p0 = format!("Kecil\nT:{} P:{}", st.preset_kecil_tip, st.preset_kecil_prio);
    let p1 = format!("Sedang\nT:{} P:{}", st.preset_sedang_tip, st.preset_sedang_prio);
    let p2 = format!("Besar\nT:{} P:{}", st.preset_besar_tip, st.preset_besar_prio);
    let p3 = format!("Mega\nT:{} P:{}", st.preset_mega_tip, st.preset_mega_prio);

    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(p0, format!("hist_preset_0_{}", id)),
            InlineKeyboardButton::callback(p1, format!("hist_preset_1_{}", id)),
            InlineKeyboardButton::callback(p2, format!("hist_preset_2_{}", id)),
            InlineKeyboardButton::callback(p3, format!("hist_preset_3_{}", id)),
        ],
        vec![InlineKeyboardButton::callback("🎯 TARGET", format!("edit_hist_target_{}", id))],
        vec![InlineKeyboardButton::callback("🗑 HAPUS", format!("delete_order_{}", id))],
    ])
}

fn make_setup_keyboard(st: &BotState) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(format!("Kecil T: {} P: {}", st.preset_kecil_tip, st.preset_kecil_prio), "setup_preset_0")],
        vec![InlineKeyboardButton::callback(format!("Sedang T: {} P: {}", st.preset_sedang_tip, st.preset_sedang_prio), "setup_preset_1")],
        vec![InlineKeyboardButton::callback(format!("Besar T: {} P: {}", st.preset_besar_tip, st.preset_besar_prio), "setup_preset_2")],
        vec![InlineKeyboardButton::callback(format!("Mega T: {} P: {}", st.preset_mega_tip, st.preset_mega_prio), "setup_preset_3")],
        vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
    ])
}

// ─── Messages & Formatters ────────────────────────────────────────────────────

async fn send_limitbuy_menu(bot: &Bot, chat_id: ChatId, st: &BotState) -> ResponseResult<()> {
    if let Some(token) = &st.active_token {
        let short = format!("{}...{}", &token[..6], &token[token.len()-4..]);
        let target_info = if !st.buy_target.is_empty() {
            format!("\n🎯 Target: `{}`", format_target_display(&st.buy_target))
        } else {
            String::new()
        };
        let text = format!("🏦 **Token:** `{}`{}\n\n*Silakan klik tombol yang ingin diubah, lalu ketik nilainya.*", short, target_info);
        bot.send_message(chat_id, text).reply_markup(make_limitbuy_keyboard(st)).await?;
    }
    Ok(())
}

fn order_detail_text(o: &LimitOrder) -> String {
    let short = format!("{}...{}", &o.token[..6], &o.token[o.token.len()-4..]);
    format!(
        "📋 **Detail Order #{}**\n\nToken: `{}`\nFull: `{}`\n\n💰 Jumlah Beli: ${:.2}\n🎯 Target: {}\
\n⚡ Tip: {} SOL\n⛽ P.Fee: {} SOL\n🏄‍♂️ Slippage: 90%\n\n*Klik tombol yang ingin diedit, lalu balas dengan nominal barunya.*",
        o.id, short, o.token, o.amount_usd, format_target_display(&o.target), o.tip_fee, o.prio_fee
    )
}


fn make_order_detail_keyboard(o: &LimitOrder, st: &BotState) -> InlineKeyboardMarkup {
    make_order_inline_keyboard(o.id as i64, st)
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
            let mut st = state.lock().await;
            st.limiter.until_ready().await;
            st.active_chats.insert(msg.chat.id);
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
        Command::Clear => {
            // Hapus tombol-tombol yang tidak terpakai dari state jika ada
            {
                let mut st = state.lock().await;
                st.swap_panel_msgs.clear();
            }
            bot.send_message(msg.chat.id, "🧹 Layar dibersihkan.\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n✅ Selesai.").await?;
        }
        Command::Panel => {
            let st = state.lock().await;
            send_limitbuy_menu(&bot, msg.chat.id, &st).await?;
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
            EditField::BuyTarget => {
                // Parse universal target: mcap / price-usd / persen-change
                // Contoh: "100000 mcap" | "30K Mcap" | "0.000005$" | "80%" | "-20%"
                if let Some(parsed) = parse_target_input(text_trim) {
                    st.buy_target = parsed;
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                } else {
                    bot.send_message(chat_id,
                        "❌ Format tidak valid.\n\n\
                        Gunakan salah satu:\n\
                        • *McAp*: `100000 mcap` | `30K Mcap` | `11M mcap`\n\
                        • *Price USD*: `0.000005$` | `$1` | `0.001$`\n\
                        • *Persen*: `80%` | `-20%` | `%80`"
                    ).await?;
                }
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
            EditField::BuyPresetTip(idx) => {
                if let Some(val) = parse_number(&lower) {
                    if idx < st.buy_presets.len() {
                        st.buy_presets[idx].tip = val;
                        // Jika preset ini aktif, apply ke buy_tip_fee juga
                        if st.buy_active_preset == ActivePreset::Idx(idx) {
                            st.buy_tip_fee = val;
                        }
                    }
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                }
            }
            EditField::BuyPresetPrio(idx) => {
                if let Some(val) = parse_number(&lower) {
                    if idx < st.buy_presets.len() {
                        st.buy_presets[idx].prio = val;
                        // Jika preset ini aktif, apply ke buy_prio_fee juga
                        if st.buy_active_preset == ActivePreset::Idx(idx) {
                            st.buy_prio_fee = val;
                        }
                    }
                    st.edit_field = EditField::None;
                    send_limitbuy_menu(&bot, chat_id, &st).await?;
                }
            }
            EditField::SellTip => {
                if let Some(val) = parse_number(&lower) {
                    st.sell_tip_fee = val;
                    st.edit_field = EditField::None;
                    bot.send_message(chat_id, format!("Tip Swap Sell diubah ke {} SOL", val))
                        .reply_markup(make_swapsell_keyboard(&st)).await?;
                }
            }
            EditField::SellPrio => {
                if let Some(val) = parse_number(&lower) {
                    st.sell_prio_fee = val;
                    st.edit_field = EditField::None;
                    bot.send_message(chat_id, format!("P.Fee Swap Sell diubah ke {} SOL", val))
                        .reply_markup(make_swapsell_keyboard(&st)).await?;
                }
            }
            EditField::SellSlippage => {
                st.sell_slippage = text_trim.to_string();
                st.edit_field = EditField::None;
                bot.send_message(chat_id, format!("Slippage Swap Sell diubah ke {}", st.sell_slippage))
                    .reply_markup(make_swapsell_keyboard(&st)).await?;
            }
            EditField::AutoTip => {
                if lower.contains("tip") || lower.contains("prio") {
                    let mut tip_updated = false;
                    let mut prio_updated = false;
                    let tokens: Vec<&str> = lower.split_whitespace().collect();
                    for i in 0..tokens.len() {
                        if tokens[i] == "tip" && i + 1 < tokens.len() {
                            if let Some(val) = parse_number(tokens[i+1]) {
                                st.limit_tip_fee = val;
                                tip_updated = true;
                            }
                        } else if (tokens[i] == "prio" || tokens[i] == "priority") && i + 1 < tokens.len() {
                            if let Some(val) = parse_number(tokens[i+1]) {
                                st.limit_prio_fee = val;
                                prio_updated = true;
                            }
                        }
                    }
                    if tip_updated || prio_updated {
                        st.edit_field = EditField::None;
                        st.save_db();
                        bot.send_message(chat_id, "✅ Auto Limit Tip/Prio berhasil diperbarui!")
                            .reply_markup(make_autolimit_keyboard(&st)).await?;
                        return Ok(());
                    }
                }
                
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
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o, &st)).await?;
                    }
                }
            }
            EditField::HistMcap(id) => {
                if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.target = text_trim.to_string(); }
                st.edit_field = EditField::None;
                if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                    bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o, &st)).await?;
                }
            }
            EditField::HistTip(id) => {
                if let Some(val) = parse_number(&lower) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.tip_fee = val; }
                    st.edit_field = EditField::None;
                    if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o, &st)).await?;
                    }
                }
            }
            EditField::HistPrio(id) => {
                if let Some(val) = parse_number(&lower) {
                    if let Some(o) = st.orders.iter_mut().find(|o| o.id == id) { o.prio_fee = val; }
                    st.edit_field = EditField::None;
                    if let Some(o) = st.orders.iter().find(|o| o.id == id).cloned() {
                        bot.send_message(chat_id, order_detail_text(&o)).reply_markup(make_order_detail_keyboard(&o, &st)).await?;
                    }
                }
            }
            EditField::HistTarget(id) => {
                // Validasi dan normalisasi target input
                let normalized = parse_target_input(text_trim).unwrap_or_else(|| text_trim.to_string());
                let update_result = if let Ok(conn) = st.db_conn.try_lock() {
                    let orders = db::load_limit_orders(&conn).unwrap_or_default();
                    if let Some(o) = orders.iter().find(|o| o.id == id) {
                        let _ = db::update_limit_order(&conn, id, &normalized, o.tip_fee, o.prio_fee);
                        let short_token = if o.token.len() >= 10 {
                            format!("{}...{}", &o.token[..6], &o.token[o.token.len()-4..])
                        } else {
                            o.token.clone()
                        };
                        Some(format!(
                            "#{} LIMIT ORDER | {}\nToken: {}\n🎯 Target | {}",
                            o.id, o.order_type, short_token, format_target_display(&normalized)
                        ))
                    } else { None }
                } else { None };
                st.edit_field = EditField::None;
                if let Some(txt) = update_result {
                    bot.send_message(chat_id, txt).reply_markup(make_order_inline_keyboard(id, &st)).await?;
                }
            }
            EditField::SetupPresetTip(idx) => {
                if lower.contains("tip") || lower.contains("prio") {
                    let mut tip_updated = false;
                    let mut prio_updated = false;
                    let tokens: Vec<&str> = lower.split_whitespace().collect();
                    for i in 0..tokens.len() {
                        if tokens[i] == "tip" && i + 1 < tokens.len() {
                            if let Some(val) = parse_number(tokens[i+1]) {
                                match idx {
                                    0 => st.preset_kecil_tip = val,
                                    1 => st.preset_sedang_tip = val,
                                    2 => st.preset_besar_tip = val,
                                    _ => st.preset_mega_tip = val,
                                }
                                tip_updated = true;
                            }
                        } else if (tokens[i] == "prio" || tokens[i] == "priority") && i + 1 < tokens.len() {
                            if let Some(val) = parse_number(tokens[i+1]) {
                                match idx {
                                    0 => st.preset_kecil_prio = val,
                                    1 => st.preset_sedang_prio = val,
                                    2 => st.preset_besar_prio = val,
                                    _ => st.preset_mega_prio = val,
                                }
                                prio_updated = true;
                            }
                        }
                    }
                    if tip_updated || prio_updated {
                        st.sync_presets();
                        st.save_db();
                        st.edit_field = EditField::None;
                        let label = match idx { 0 => "Kecil", 1 => "Sedang", 2 => "Besar", _ => "Mega" };
                        bot.send_message(chat_id, format!("✅ Preset **{}** berhasil diperbarui!", label))
                            .reply_markup(make_setup_keyboard(&st)).await?;
                        return Ok(());
                    }
                }
                
                // Fallback (sequential)
                if let Some(val) = parse_number(&lower) {
                    match idx {
                        0 => st.preset_kecil_tip = val,
                        1 => st.preset_sedang_tip = val,
                        2 => st.preset_besar_tip = val,
                        _ => st.preset_mega_tip = val,
                    }
                    st.edit_field = EditField::SetupPresetPrio(idx);
                    let label = match idx { 0 => "Kecil", 1 => "Sedang", 2 => "Besar", _ => "Mega" };
                    bot.send_message(chat_id, format!("✅ Tip disimpan! Sekarang ketik *Priority Fee* untuk preset **{}** (contoh: `0.002`)", label)).await?;
                }
            }
            EditField::SetupPresetPrio(idx) => {
                if let Some(val) = parse_number(&lower) {
                    match idx {
                        0 => st.preset_kecil_prio = val,
                        1 => st.preset_sedang_prio = val,
                        2 => st.preset_besar_prio = val,
                        _ => st.preset_mega_prio = val,
                    }
                    st.sync_presets();
                    st.save_db();
                    st.edit_field = EditField::None;
                    let label = match idx { 0 => "Kecil", 1 => "Sedang", 2 => "Besar", _ => "Mega" };
                    bot.send_message(chat_id, format!("✅ Preset **{}** berhasil diperbarui!", label))
                        .reply_markup(make_setup_keyboard(&st)).await?;
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
                    st.buy_target = text_trim.to_string();
                    if st.active_token.is_some() { send_limitbuy_menu(&bot, chat_id, &st).await?; }
                } else if lower.ends_with("sol") {
                    if let Some(val) = parse_number(&lower) {
                        st.buy_tip_fee = val;
                        st.buy_prio_fee = val;
                        if st.active_token.is_some() { send_limitbuy_menu(&bot, chat_id, &st).await?; }
                    }
                }
            }
            _ => {
                // handle other edit fields
                st.edit_field = EditField::None;
            }
        }
        st.save_db();
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
        if data.starts_with("delete_order_") {
            let id: i64 = data.trim_start_matches("delete_order_").parse().unwrap_or(0);
            if let Ok(conn) = st.db_conn.try_lock() {
                let _ = db::delete_limit_order(&conn, id);
            }
            bot.edit_message_text(chat_id, msg_id, "Order dihapus.").await?;
            bot.answer_callback_query(q.id.clone()).await?;
            return Ok(());
        }

        if data.starts_with("hist_preset_") {
            // hist_preset_0_123
            let parts: Vec<&str> = data.split('_').collect();
            if parts.len() == 4 {
                let preset_idx: usize = parts[2].parse().unwrap_or(0);
                let order_id: i64 = parts[3].parse().unwrap_or(0);
                let (tip, prio, preset_label) = match preset_idx {
                    0 => (st.preset_kecil_tip, st.preset_kecil_prio, "Kecil"),
                    1 => (st.preset_sedang_tip, st.preset_sedang_prio, "Sedang"),
                    2 => (st.preset_besar_tip, st.preset_besar_prio, "Besar"),
                    _ => (st.preset_mega_tip, st.preset_mega_prio, "Mega"),
                };
                if let Ok(conn) = st.db_conn.try_lock() {
                    let orders = db::load_limit_orders(&conn).unwrap_or_default();
                    if let Some(o) = orders.iter().find(|o| o.id == order_id) {
                        let _ = db::update_limit_order(&conn, order_id, &o.target, tip, prio);
                        bot.answer_callback_query(q.id.clone()).text(format!("✅ Preset {} diaplikasikan!", preset_label)).await?;
                        // update msg - format baru
                        let short_token = if o.token.len() >= 10 {
                            format!("{}...{}", &o.token[..6], &o.token[o.token.len()-4..])
                        } else {
                            o.token.clone()
                        };
                        let target_display = format_target_display(&o.target);
                        let text = format!(
                            "#{} LIMIT ORDER | {}\nToken: {}\n🎯 Target | {}\n⚡ T:{} SOL ⛽ P:{} SOL",
                            o.id, o.order_type, short_token, target_display, tip, prio
                        );
                        bot.edit_message_text(chat_id, msg_id, text).reply_markup(make_order_inline_keyboard(o.id, &st)).await?;
                        return Ok(());
                    }
                }
            }
        }
        
        if data.starts_with("edit_hist_target_") {
            let id: i64 = data.trim_start_matches("edit_hist_target_").parse().unwrap_or(0);
            st.edit_field = EditField::HistTarget(id);
            bot.answer_callback_query(q.id.clone()).text("Aim & Slay 🌞 Ketik target!").await?;
            bot.send_message(chat_id,
                "🎯 Ketik target baru:\n\n\
                • McAp: `100K mcap` | `11M mcap` | `2.35M mcap`\n\
                • Price: `0.000005$` | `$1`\n\
                • Persen: `80%` | `-20%`"
            ).await?;
            return Ok(());
        }

        // ── Prefix routing untuk Quick-set Preset ────────────────────────────────
        // preset_select_<idx>: user memilih preset, apply ke buy_tip_fee & buy_prio_fee
        if data.starts_with("preset_select_") {
            let idx: usize = data.trim_start_matches("preset_select_").parse().unwrap_or(0);
            if idx < st.buy_presets.len() {
                // Toggle: jika sudah aktif, deaktifkan; jika belum, aktifkan
                if st.buy_active_preset == ActivePreset::Idx(idx) {
                    let label = st.buy_presets[idx].label.clone();
                    st.buy_active_preset = ActivePreset::None;
                    bot.answer_callback_query(q.id.clone()).text(format!("Preset {} dinonaktifkan", label)).await?;
                } else {
                    // Clone nilai dulu sebelum mutable borrow
                    let (tip, prio, label) = {
                        let p = &st.buy_presets[idx];
                        (p.tip, p.prio, p.label.clone())
                    };
                    st.buy_tip_fee = tip;
                    st.buy_prio_fee = prio;
                    st.buy_active_preset = ActivePreset::Idx(idx);
                    bot.answer_callback_query(q.id.clone()).text(format!("✅ Preset {} aktif! Tip={} Prio={}", label, tip, prio)).await?;
                }
                // Refresh keyboard
                let keyboard = make_limitbuy_keyboard(&st);
                let _ = bot.edit_message_reply_markup(chat_id, msg_id).reply_markup(keyboard).await;
            }
            return Ok(());
        }
        // preset_edit_tip_<idx>: edit nilai tip preset tertentu
        if data.starts_with("preset_edit_tip_") {
            let idx: usize = data.trim_start_matches("preset_edit_tip_").parse().unwrap_or(0);
            if idx < st.buy_presets.len() {
                let label = st.buy_presets[idx].label.clone();
                st.edit_field = EditField::BuyPresetTip(idx);
                bot.answer_callback_query(q.id.clone()).text(format!("Ketik Tip baru untuk preset {}", label)).await?;
                bot.send_message(chat_id, format!("✏️ Ketik nilai Tip untuk preset **{}** (contoh: 0.005)", label)).await?;
            }
            return Ok(());
        }
        // preset_edit_prio_<idx>: edit nilai prio preset tertentu
        if data.starts_with("preset_edit_prio_") {
            let idx: usize = data.trim_start_matches("preset_edit_prio_").parse().unwrap_or(0);
            if idx < st.buy_presets.len() {
                let label = st.buy_presets[idx].label.clone();
                st.edit_field = EditField::BuyPresetPrio(idx);
                bot.answer_callback_query(q.id.clone()).text(format!("Ketik Prio baru untuk preset {}", label)).await?;
                bot.send_message(chat_id, format!("✏️ Ketik nilai P.Fee untuk preset **{}** (contoh: 0.005)", label)).await?;
            }
            return Ok(());
        }

        // Exact match routing
        match data.as_str() {
            "menu_main" => {
                bot.edit_message_text(chat_id, msg_id, "👋 **Menu Utama**").reply_markup(make_main_menu_keyboard()).await?;
                bot.answer_callback_query(q.id.clone()).await?;
            }
            "menu_swapsell" => {
                let token = st.active_token.clone().unwrap_or_else(|| "Tidak ada token".to_string());
                let bot_clone = bot.clone();
                let client_opt = st.aura_client.clone();
                let _q_id = q.id.clone();
                let keyboard = make_swapsell_keyboard(&st);
                tokio::spawn(async move {
                    let mut pnl_text = format!("Token: `{}`\nPNL: 0.00%\nAmount SOL: 0", token);
                    if token != "Tidak ada token" {
                        if let Some(client) = client_opt {
                            let req = tonic::Request::new(aura_api_client::types::TokenPositionsUiReq { mint: None });
                            if let Ok(resp) = client.aura().get_token_positions_ui((), req).await {
                                let ui = resp.into_inner();
                                for pos in ui.positions {
                                    let mint_str = format!("{:?}", pos.mint);
                                    if mint_str.contains(&token) {
                                        let pnl_str = if let Some(p) = pos.pnl {
                                            format!("{:.2}", p)
                                        } else {
                                            "0.00".to_string()
                                        };
                                        let amount_sol = format!("{}", pos.quote_value);
                                        pnl_text = format!("Token: `{}`\nPNL: {}%\nAmount SOL: {}", token, pnl_str, amount_sol);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    let _ = bot_clone.edit_message_text(chat_id, msg_id, pnl_text)
                        .reply_markup(keyboard)
                        .await;
                });
                bot.answer_callback_query(q.id.clone()).text("Memuat data token...").await?;
            }
            "refresh_pnl" => {
                let token = st.active_token.clone().unwrap_or_else(|| "Tidak ada token".to_string());
                let bot_clone = bot.clone();
                let client_opt = st.aura_client.clone();
                let _q_id = q.id.clone();
                // Gunakan keyboard panel saja, jangan setting
                let panel_keyboard = teloxide::types::InlineKeyboardMarkup::new(vec![
                    vec![teloxide::types::InlineKeyboardButton::callback(
                        "🔴 Confirm Swap Sell", "execute_swap_sell",
                    )],
                    vec![teloxide::types::InlineKeyboardButton::callback(
                        "🔄 Refresh PNL", "refresh_pnl",
                    )],
                ]);
                tokio::spawn(async move {
                    let mut pnl_text = format!("Token: `{}`\nPNL: 0.00%\nAmount SOL: 0", token);
                    if token != "Tidak ada token" {
                        if let Some(client) = client_opt {
                            let req = tonic::Request::new(aura_api_client::types::TokenPositionsUiReq { mint: None });
                            if let Ok(resp) = client.aura().get_token_positions_ui((), req).await {
                                let ui = resp.into_inner();
                                for pos in ui.positions {
                                    let mint_str = format!("{:?}", pos.mint);
                                    if mint_str.contains(&token) {
                                        let pnl_str = if let Some(p) = pos.pnl {
                                            format!("{:.2}", p)
                                        } else {
                                            "0.00".to_string()
                                        };
                                        let amount_sol = format!("{}", pos.quote_value);
                                        pnl_text = format!("Token: `{}`\nPNL: {}%\nAmount SOL: {}", token, pnl_str, amount_sol);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    let _ = bot_clone.edit_message_text(chat_id, msg_id, pnl_text)
                        .reply_markup(panel_keyboard)
                        .await;
                });
                bot.answer_callback_query(q.id.clone()).text("Memuat data token...").await?;
            }
            "menu_autolimit" => {
                bot.edit_message_text(chat_id, msg_id, "⚙️ **Auto Limit Sell**\nKlik tombol lalu ketik nilainya.").reply_markup(make_autolimit_keyboard(&st)).await?;
                bot.answer_callback_query(q.id.clone()).await?;
            }
            "menu_history" => {
                drop(st); // release lock before await
                let st2 = state.lock().await;
                bot.answer_callback_query(q.id.clone()).await?;
                bot.send_message(chat_id, "📋 *Limit Order History*").await?;
                send_history_orders(&bot, chat_id, &st2).await?;
                return Ok(());
            }
            "menu_lo_setup" => {
                bot.send_message(chat_id, "⚙️ *Limit Order Setup*\n\nTap salah satu preset untuk mengatur Tip & Priority Fee.\nNilai ini digunakan saat Anda memilih preset di history order.")
                    .reply_markup(make_setup_keyboard(&st)).await?;
                bot.answer_callback_query(q.id.clone()).await?;
            }
            "menu_lo_logs" => {
                drop(st);
                let st2 = state.lock().await;
                bot.answer_callback_query(q.id.clone()).await?;
                send_error_logs(&bot, chat_id, &st2).await?;
                return Ok(());
            }
            "clear_error_logs" => {
                if let Ok(conn) = st.db_conn.try_lock() {
                    let _ = db::clear_error_logs(&conn);
                }
                bot.edit_message_text(chat_id, msg_id, "✅ Semua log error telah dihapus.").await?;
                bot.answer_callback_query(q.id.clone()).await?;
            }
            "toggle_autolimit" => {
                st.auto_limit_active = !st.auto_limit_active;
                st.save_db();
                bot.edit_message_reply_markup(chat_id, msg_id).reply_markup(make_autolimit_keyboard(&st)).await?;
                bot.answer_callback_query(q.id.clone()).await?;
            }
            // Limit Order Setup preset handlers
            "setup_preset_0" | "setup_preset_1" | "setup_preset_2" | "setup_preset_3" => {
                let idx: usize = data.trim_start_matches("setup_preset_").parse().unwrap_or(0);
                let label = match idx { 0 => "Kecil", 1 => "Sedang", 2 => "Besar", _ => "Mega" };
                st.edit_field = EditField::SetupPresetTip(idx);
                bot.answer_callback_query(q.id.clone()).text(format!("Edit {} Tip & Prio", label)).await?;
                bot.send_message(chat_id, format!("✏️ Ketik *Tip* untuk preset **{}** (contoh: `0.002`)", label))
                    .await?;
            }
            // Auto Limit Edits
            "edit_sell_tip" => {
                st.edit_field = EditField::SellTip;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input tip...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Tip Swap Sell (contoh: 0.005)").await?;
            }
            "edit_sell_prio" => {
                st.edit_field = EditField::SellPrio;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input p.fee...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai P.Fee Swap Sell (contoh: 0.005)").await?;
            }
            "edit_sell_slippage" => {
                st.edit_field = EditField::SellSlippage;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input slippage...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Slippage Swap Sell (contoh: 95%)").await?;
            }
            // Auto Limit Edits
            "edit_auto_tip" => {
                st.edit_field = EditField::AutoTip;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input tip...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Tip baru (contoh: 0.005)").await?;
            }
            "edit_auto_prio" => {
                st.edit_field = EditField::AutoPrio;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input p.fee...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai P.Fee baru (contoh: 0.005)").await?;
            }
            "edit_auto_acttime" => {
                st.edit_field = EditField::AutoActTime;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input act.time...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Act.Time baru (contoh: 5s, 10s)").await?;
            }
            "edit_auto_pnl" => {
                st.edit_field = EditField::AutoPnl;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input target PNL...").await?;
                bot.send_message(chat_id, "✏️ Ketik Target PNL baru (contoh: 100%)").await?;
            }
            // Buy Limit Edits
            "edit_buy_amount" => {
                st.edit_field = EditField::BuyAmount;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input jumlah...").await?;
                bot.send_message(chat_id, "✏️ Ketik jumlah USD beli baru (contoh: 5)").await?;
            }
            "edit_buy_target" => {
                st.edit_field = EditField::BuyTarget;
                bot.answer_callback_query(q.id.clone()).text("Aim & Slay 🌞 Ketik target...").await?;
                bot.send_message(chat_id,
                    "🎯 *Aim \\& Slay* 🌞\n\
                    Plug in your numbers\\. Your set \\- your rules\\.\n\n\
                    〽️ *Market Cap in USD*\n\
                    `100000 mcap` \\| `30K Mcap` \\| `11M mcap` \\| `2\\.35M mcap`\n\n\
                    💸 *Price in USD*\n\
                    `0\\.001$` \\| `$1` \\| `0\\.0000005$`\n\n\
                    💹 *Price Percentage Change*\n\
                    `80%` \\| `\\-20%` \\| `%80`"
                ).await?;
            }
            "edit_buy_tip" => {
                st.edit_field = EditField::BuyTip;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input tip...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai Tip baru (contoh: 0.005)").await?;
            }
            "edit_buy_prio" => {
                st.edit_field = EditField::BuyPrio;
                bot.answer_callback_query(q.id.clone()).text("Menunggu input p.fee...").await?;
                bot.send_message(chat_id, "✏️ Ketik nilai P.Fee baru (contoh: 0.005)").await?;
            }
            "place_limit_buy" => {
                if let Some(token) = &st.active_token {
                    let target = st.buy_target.clone();
                    let o = LimitOrder {
                        id: st.next_order_id,
                        token: token.clone(),
                        amount_usd: st.buy_amount_usd,
                        target: target.clone(),
                        tip_fee: st.buy_tip_fee,
                        prio_fee: st.buy_prio_fee,
                    };
                    // Save to SQLite
                    if let Ok(conn) = st.db_conn.try_lock() {
                        let _ = db::insert_limit_order(&conn, "BUY", token, &target, st.buy_tip_fee, st.buy_prio_fee);
                    }
                    st.orders.push(o);
                    st.next_order_id += 1;
                    bot.answer_callback_query(q.id.clone()).text("Order disimpan!").await?;
                    bot.send_message(chat_id, "🟢 Limit Buy Order disimpan ke History.\n\n📋 Buka *Limit Order History* untuk melihat.").await?;
                }
            }
            "execute_swap_sell" => {
                if let Some(client) = st.aura_client.clone() {
                    if let Some(token) = st.active_token.clone() {
                        let bot_clone = bot.clone();
                        let tip_fee = st.sell_tip_fee;
                        let prio_fee = st.sell_prio_fee;
                        let slippage_str = st.sell_slippage.clone();
                        let chat_id_clone = chat_id;
                        // Ambil daftar panel yg perlu diedit setelah sell berhasil
                        let panel_msgs = st.swap_panel_msgs.clone();
                        st.swap_panel_msgs.clear(); // clear dulu agar tidak diedit ulang
                        
                        tokio::spawn(async move {
                            use aura_api_client::types::{MarketTrade, SwapAmount, UserNonceStrategy, ApiOrders, TradeFilters};
                            use std::str::FromStr;
                            
                            let _ = bot_clone.send_message(chat_id_clone, "⏳ Memproses Swap Sell Manual 100% ke Aura...").await;
                            
                            if let Ok(mint_addr) = solana_address::Address::from_str(&token) {
                                let tip_lamports = (tip_fee * 1e9) as u64;
                                let prio_lamports = (prio_fee * 1e9) as u64;
                                
                                let slippage_f64 = slippage_str.replace("%", "").trim().parse::<f64>().unwrap_or(15.0);
                                let slippage_scaled = (slippage_f64 / 100.0 * 1_000_000.0) as u64;
                                let slippage_val = fastnum::UD128::from(slippage_scaled) / fastnum::UD128::from(1_000_000u64);
                                
                                let req = tonic::Request::new(MarketTrade {
                                    wallet: None,
                                    amount: SwapAmount::SellPerc { amount: fastnum::udec128!(1) },
                                    mint: mint_addr,
                                    slippage: slippage_val,
                                    tip: decisol::Lamports::from(tip_lamports),
                                    priority_fee: decisol::Lamports::from(prio_lamports),
                                    procs: None,
                                    nonce: UserNonceStrategy::Hybrid,
                                    slot_latency: None,
                                    expire_at: None,
                                    rpc_nonce: None,
                                    max_price_impact: None,
                                    limit_orders: ApiOrders { orders: vec![] },
                                    filters: TradeFilters { min_mcap: None, max_mcap: None },
                                });

                                match client.aura().trade((), req).await {
                                    Ok(_resp) => {
                                        let short = if token.len() >= 10 {
                                            format!("{}...{}", &token[..6], &token[token.len()-4..])
                                        } else {
                                            token.clone()
                                        };
                                        let sold_text = format!(
                                            "✅ *Terjual via Swap Sell Manual!*\n\n\
                                            🏦 Token: `{}`\n\
                                            💰 100% posisi dijual ke pasar\n\
                                            ⚡ Tip: {} SOL\n\
                                            ⛽ P.Fee: {} SOL\n\n\
                                            _Transaksi dikirim ke Aura gRPC._",
                                            short, tip_fee, prio_fee
                                        );
                                        // Edit panel asli jika ada
                                        if panel_msgs.is_empty() {
                                            let _ = bot_clone.send_message(chat_id_clone, &sold_text).await;
                                        } else {
                                            for (c, m) in &panel_msgs {
                                                let _ = bot_clone.edit_message_text(*c, *m, &sold_text).await;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = bot_clone.send_message(chat_id_clone, format!("❌ Swap Sell Manual Gagal: {}", e.message())).await;
                                    }
                                }
                            } else {
                                let _ = bot_clone.send_message(chat_id_clone, "❌ Alamat token tidak valid!").await;
                            }
                        });
                    } else {
                        bot.send_message(chat_id, "❌ Tidak ada token aktif yang terdeteksi untuk dijual.").await?;
                    }
                } else {
                    bot.send_message(chat_id, "❌ Koneksi ke Aura gRPC tidak tersedia.").await?;
                }
                bot.answer_callback_query(q.id.clone()).await?;
            }

            "none" => {
                bot.answer_callback_query(q.id.clone()).text("Terkunci (Fixed Setting).").await?;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn send_history_orders(bot: &Bot, chat_id: ChatId, st: &BotState) -> ResponseResult<()> {
    if let Ok(conn) = st.db_conn.try_lock() {
        let orders = db::load_limit_orders(&conn).unwrap_or_default();
        if orders.is_empty() {
            bot.send_message(chat_id, "📭 Belum ada limit order yang aktif.").await?;
        } else {
            // Numbering ulang dari 1 berdasarkan urutan aktif, bukan DB id
            for (idx, o) in orders.iter().enumerate() {
                let display_num = idx + 1;
                // Format nama token: gunakan 6 char awal ... 4 char akhir
                let short_token = if o.token.len() >= 10 {
                    format!("{}...{}", &o.token[..6], &o.token[o.token.len()-4..])
                } else {
                    o.token.clone()
                };
                // Format target dengan display method
                let target_display = format_target_display(&o.target);
                let text = format!(
                    "#{} LIMIT ORDER | {}\nToken: {}\n🎯 Target | {}",
                    display_num, o.order_type, short_token, target_display
                );
                bot.send_message(chat_id, text)
                    .reply_markup(make_order_inline_keyboard(o.id, st))
                    .await?;
            }
        }
    } else {
        bot.send_message(chat_id, "❌ Database sedang sibuk, coba lagi.").await?;
    }
    Ok(())
}


async fn send_error_logs(bot: &Bot, chat_id: ChatId, st: &BotState) -> ResponseResult<()> {
    if let Ok(conn) = st.db_conn.try_lock() {
        let logs = db::load_error_logs(&conn).unwrap_or_default();
        if logs.is_empty() {
            bot.send_message(chat_id, "📜 Tidak ada log error saat ini.\n\n_Semua transaksi limit order berjalan normal, atau belum ada limit order yang dieksekusi._").await?;
        } else {
            let mut text = String::from("📜 *Error Logs Limit Order*\n\n");
            for l in logs.iter().take(10) {
                text.push_str(&format!(
                    "`[{}]`\nOrder #{} | Token: `{}`\nError: _{}_\n\n",
                    l.created_at, l.order_id, l.token, l.error_msg
                ));
            }
            if logs.len() > 10 {
                text.push_str(&format!("_(+ {} log lainnya — klik hapus untuk bersihkan semua)_", logs.len() - 10));
            }
            let kb = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback("🗑 Hapus Semua Log", "clear_error_logs")],
                vec![InlineKeyboardButton::callback("<< Back", "menu_main")],
            ]);
            bot.send_message(chat_id, text).reply_markup(kb).await?;
        }
    } else {
        bot.send_message(chat_id, "❌ Database sedang sibuk, coba lagi.").await?;
    }
    Ok(())
}
