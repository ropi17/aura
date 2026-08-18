use rusqlite::{Connection, Result, params};
use std::collections::HashSet;
use teloxide::types::ChatId;

// ─── Settings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DbSettings {
    pub auto_limit_active: bool,
    pub limit_tip_fee: f64,
    pub limit_prio_fee: f64,
    pub limit_act_time: String,
    pub limit_target_pnl: String,
    pub sell_tip_fee: f64,
    pub sell_prio_fee: f64,
    pub sell_slippage: String,
    // Preset sizes (kecil/sedang/besar/mega) — tip & prio
    pub preset_kecil_tip: f64,
    pub preset_kecil_prio: f64,
    pub preset_sedang_tip: f64,
    pub preset_sedang_prio: f64,
    pub preset_besar_tip: f64,
    pub preset_besar_prio: f64,
    pub preset_mega_tip: f64,
    pub preset_mega_prio: f64,
}

impl Default for DbSettings {
    fn default() -> Self {
        Self {
            auto_limit_active: false,
            limit_tip_fee: 0.0025,
            limit_prio_fee: 0.002,
            limit_act_time: "0s".to_string(),
            limit_target_pnl: "50%".to_string(),
            sell_tip_fee: 0.0015,
            sell_prio_fee: 0.0015,
            sell_slippage: "95%".to_string(),
            preset_kecil_tip: 0.0005,
            preset_kecil_prio: 0.0005,
            preset_sedang_tip: 0.001,
            preset_sedang_prio: 0.001,
            preset_besar_tip: 0.002,
            preset_besar_prio: 0.002,
            preset_mega_tip: 0.005,
            preset_mega_prio: 0.005,
        }
    }
}

// ─── Limit Order History (persistent) ─────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DbLimitOrder {
    pub id: i64,
    pub order_type: String, // "BUY" or "SELL"
    pub token: String,
    pub target: String,
    pub tip_fee: f64,
    pub prio_fee: f64,
    pub created_at: String,
}

// ─── Error Logs ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DbErrorLog {
    pub id: i64,
    pub order_id: i64,
    pub token: String,
    pub error_msg: String,
    pub created_at: String,
}

// ─── Init ──────────────────────────────────────────────────────────────────────

pub fn init_db() -> Result<Connection> {
    let conn = Connection::open("bot_data.db")?;

    // Settings table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY,
            auto_limit_active INTEGER DEFAULT 0,
            limit_tip_fee REAL DEFAULT 0.0015,
            limit_prio_fee REAL DEFAULT 0.0015,
            limit_act_time TEXT DEFAULT '0s',
            limit_target_pnl TEXT DEFAULT '50%',
            sell_tip_fee REAL DEFAULT 0.0015,
            sell_prio_fee REAL DEFAULT 0.0015,
            sell_slippage TEXT DEFAULT '95%',
            preset_kecil_tip REAL DEFAULT 0.0005,
            preset_kecil_prio REAL DEFAULT 0.0005,
            preset_sedang_tip REAL DEFAULT 0.001,
            preset_sedang_prio REAL DEFAULT 0.001,
            preset_besar_tip REAL DEFAULT 0.002,
            preset_besar_prio REAL DEFAULT 0.002,
            preset_mega_tip REAL DEFAULT 0.005,
            preset_mega_prio REAL DEFAULT 0.005
        )",
        [],
    )?;

    // Chats table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS chats (
            chat_id INTEGER PRIMARY KEY
        )",
        [],
    )?;

    // Limit orders history table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS limit_orders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            order_type TEXT NOT NULL,
            token TEXT NOT NULL,
            target TEXT NOT NULL,
            tip_fee REAL NOT NULL,
            prio_fee REAL NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;

    // Error logs table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS error_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            order_id INTEGER DEFAULT 0,
            token TEXT NOT NULL,
            error_msg TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;

    // Insert default settings row if not present
    let count: i64 = conn.query_row("SELECT count(*) FROM settings WHERE id = 1", [], |row| row.get(0))?;
    if count == 0 {
        let def = DbSettings::default();
        conn.execute(
            "INSERT INTO settings (id, auto_limit_active, limit_tip_fee, limit_prio_fee, limit_act_time, limit_target_pnl,
             sell_tip_fee, sell_prio_fee, sell_slippage,
             preset_kecil_tip, preset_kecil_prio, preset_sedang_tip, preset_sedang_prio,
             preset_besar_tip, preset_besar_prio, preset_mega_tip, preset_mega_prio)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                def.auto_limit_active as i32,
                def.limit_tip_fee,
                def.limit_prio_fee,
                def.limit_act_time,
                def.limit_target_pnl,
                def.sell_tip_fee,
                def.sell_prio_fee,
                def.sell_slippage,
                def.preset_kecil_tip,
                def.preset_kecil_prio,
                def.preset_sedang_tip,
                def.preset_sedang_prio,
                def.preset_besar_tip,
                def.preset_besar_prio,
                def.preset_mega_tip,
                def.preset_mega_prio,
            ],
        )?;
    }

    Ok(conn)
}

// ─── Settings CRUD ─────────────────────────────────────────────────────────────

pub fn load_settings(conn: &Connection) -> Result<DbSettings> {
    conn.query_row(
        "SELECT auto_limit_active, limit_tip_fee, limit_prio_fee, limit_act_time, limit_target_pnl,
                sell_tip_fee, sell_prio_fee, sell_slippage,
                preset_kecil_tip, preset_kecil_prio, preset_sedang_tip, preset_sedang_prio,
                preset_besar_tip, preset_besar_prio, preset_mega_tip, preset_mega_prio
         FROM settings WHERE id = 1",
        [],
        |row| {
            Ok(DbSettings {
                auto_limit_active: row.get::<_, i32>(0)? != 0,
                limit_tip_fee: row.get(1)?,
                limit_prio_fee: row.get(2)?,
                limit_act_time: row.get(3)?,
                limit_target_pnl: row.get(4)?,
                sell_tip_fee: row.get(5)?,
                sell_prio_fee: row.get(6)?,
                sell_slippage: row.get(7)?,
                preset_kecil_tip: row.get(8)?,
                preset_kecil_prio: row.get(9)?,
                preset_sedang_tip: row.get(10)?,
                preset_sedang_prio: row.get(11)?,
                preset_besar_tip: row.get(12)?,
                preset_besar_prio: row.get(13)?,
                preset_mega_tip: row.get(14)?,
                preset_mega_prio: row.get(15)?,
            })
        },
    )
}

pub fn save_settings(conn: &Connection, set: &DbSettings) -> Result<()> {
    conn.execute(
        "UPDATE settings SET
            auto_limit_active = ?1,
            limit_tip_fee = ?2,
            limit_prio_fee = ?3,
            limit_act_time = ?4,
            limit_target_pnl = ?5,
            sell_tip_fee = ?6,
            sell_prio_fee = ?7,
            sell_slippage = ?8,
            preset_kecil_tip = ?9,
            preset_kecil_prio = ?10,
            preset_sedang_tip = ?11,
            preset_sedang_prio = ?12,
            preset_besar_tip = ?13,
            preset_besar_prio = ?14,
            preset_mega_tip = ?15,
            preset_mega_prio = ?16
         WHERE id = 1",
        params![
            set.auto_limit_active as i32,
            set.limit_tip_fee,
            set.limit_prio_fee,
            set.limit_act_time,
            set.limit_target_pnl,
            set.sell_tip_fee,
            set.sell_prio_fee,
            set.sell_slippage,
            set.preset_kecil_tip,
            set.preset_kecil_prio,
            set.preset_sedang_tip,
            set.preset_sedang_prio,
            set.preset_besar_tip,
            set.preset_besar_prio,
            set.preset_mega_tip,
            set.preset_mega_prio,
        ],
    )?;
    Ok(())
}

// ─── Chats CRUD ────────────────────────────────────────────────────────────────

pub fn load_chats(conn: &Connection) -> Result<HashSet<ChatId>> {
    let mut stmt = conn.prepare("SELECT chat_id FROM chats")?;
    let chat_iter = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        Ok(ChatId(id))
    })?;
    let mut chats = HashSet::new();
    for c in chat_iter {
        chats.insert(c?);
    }
    Ok(chats)
}

pub fn save_chat(conn: &Connection, chat_id: ChatId) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO chats (chat_id) VALUES (?1)", [chat_id.0])?;
    Ok(())
}

// ─── Limit Order History CRUD ─────────────────────────────────────────────────

pub fn insert_limit_order(conn: &Connection, order_type: &str, token: &str, target: &str, tip: f64, prio: f64) -> Result<i64> {
    conn.execute(
        "INSERT INTO limit_orders (order_type, token, target, tip_fee, prio_fee) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![order_type, token, target, tip, prio],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn load_limit_orders(conn: &Connection) -> Result<Vec<DbLimitOrder>> {
    let mut stmt = conn.prepare(
        "SELECT id, order_type, token, target, tip_fee, prio_fee, created_at FROM limit_orders ORDER BY id ASC"
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(DbLimitOrder {
            id: row.get(0)?,
            order_type: row.get(1)?,
            token: row.get(2)?,
            target: row.get(3)?,
            tip_fee: row.get(4)?,
            prio_fee: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    let mut orders = Vec::new();
    for o in iter { orders.push(o?); }
    Ok(orders)
}

pub fn update_limit_order(conn: &Connection, id: i64, target: &str, tip: f64, prio: f64) -> Result<()> {
    conn.execute(
        "UPDATE limit_orders SET target = ?1, tip_fee = ?2, prio_fee = ?3 WHERE id = ?4",
        params![target, tip, prio, id],
    )?;
    Ok(())
}

pub fn delete_limit_order(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM limit_orders WHERE id = ?1", [id])?;
    Ok(())
}

// ─── Error Log CRUD ────────────────────────────────────────────────────────────

pub fn insert_error_log(conn: &Connection, order_id: i64, token: &str, error_msg: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO error_logs (order_id, token, error_msg) VALUES (?1, ?2, ?3)",
        params![order_id, token, error_msg],
    )?;
    Ok(())
}

pub fn load_error_logs(conn: &Connection) -> Result<Vec<DbErrorLog>> {
    let mut stmt = conn.prepare(
        "SELECT id, order_id, token, error_msg, created_at FROM error_logs ORDER BY id DESC LIMIT 50"
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(DbErrorLog {
            id: row.get(0)?,
            order_id: row.get(1)?,
            token: row.get(2)?,
            error_msg: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut logs = Vec::new();
    for l in iter { logs.push(l?); }
    Ok(logs)
}

pub fn clear_error_logs(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM error_logs", [])?;
    Ok(())
}
