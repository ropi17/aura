# Aura Telegram Bot 🚀

Bot Telegram berbasis Rust untuk melakukan transaksi (Swap Buy/Sell & Limit Order) di jaringan Solana menggunakan API dari Aura. Bot ini menggunakan SQLite untuk menyimpan data riwayat limit order, pengaturan bot, dan error log.

## 📌 Prasyarat Sistem

Jika Anda mendeploy bot ini di server/VPS baru (Ubuntu/Debian), ikuti langkah-langkah di bawah ini dari awal sampai bot berjalan.

### 1. Update Server & Install Dependencies
Buka terminal VPS Anda dan jalankan perintah ini:
```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y build-essential pkg-config libssl-dev git sqlite3 tmux
```

### 2. Install Rust 🦀
Bot ini ditulis dalam bahasa Rust, jadi Anda perlu menginstall compiler Rust:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
*Tekan `1` lalu `Enter` saat diminta konfirmasi instalasi.*

Setelah selesai, muat ulang environment Rust:
```bash
source $HOME/.cargo/env
```

### 3. Clone Repository
Clone repository bot ini ke dalam server:
```bash
git clone https://github.com/ropi17/aura.git
cd aura
```

### 4. Konfigurasi Environment Variables (`.env`)
Buat file konfigurasi `.env`:
```bash
nano .env
```
Isi file tersebut dengan data berikut (ganti dengan milik Anda):
```env
# Token Bot Telegram dari @BotFather
TELOXIDE_TOKEN=123456789:ABCDefghIJKLmnopQRSTuvwxyz

# API Key untuk Aura
AURA_API_KEY=api_key_aura_anda_disini

# (Opsional) ID Telegram Anda, agar bot bisa memuat ID Anda saat pertama kali menyala
TELEGRAM_CHAT_ID=1234567890
```
Simpan dan keluar (tekan `Ctrl+X`, lalu `Y`, lalu `Enter`).

### 5. Build Bot (Kompilasi)
Kompilasi kode bot ke dalam mode release agar berjalan cepat dan ringan:
```bash
cargo build --release
```
*(Proses ini mungkin memakan waktu beberapa menit karena mengunduh dan mengkompilasi library).*

---

## 🚀 Cara Menjalankan Bot

Ada dua cara untuk menjalankan bot agar tetap hidup meskipun Anda menutup terminal (SSH).

### Opsi A: Menggunakan `tmux` (Paling Mudah)
1. Buat sesi terminal baru:
   ```bash
   tmux new -s aurabot
   ```
2. Jalankan bot:
   ```bash
   ./target/release/aura_bot
   ```
3. Bot sudah berjalan! Anda bisa menekan `Ctrl+B`, lalu tekan `D` untuk keluar dari sesi tmux tanpa mematikan bot.
4. Jika ingin melihat log bot lagi, ketik: `tmux attach -t aurabot`.

### Opsi B: Menggunakan `systemd` (Disarankan untuk Produksi)
Agar bot otomatis menyala saat VPS direstart:
1. Buat file service:
   ```bash
   sudo nano /etc/systemd/system/aurabot.service
   ```
2. Isi dengan script berikut (pastikan path `/home/ubuntu/aura` sesuai dengan lokasi folder Anda, ganti `ubuntu` dengan username VPS Anda jika berbeda):
   ```ini
   [Unit]
   Description=Aura Telegram Bot
   After=network.target

   [Service]
   User=ubuntu
   WorkingDirectory=/home/ubuntu/aura
   ExecStart=/home/ubuntu/aura/target/release/aura_bot
   Restart=always
   EnvironmentFile=/home/ubuntu/aura/.env

   [Install]
   WantedBy=multi-user.target
   ```
3. Simpan, lalu jalankan perintah berikut untuk mengaktifkan dan memulai bot:
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl enable aurabot
   sudo systemctl start aurabot
   ```
4. Cek status bot:
   ```bash
   sudo systemctl status aurabot
   ```
5. Lihat log bot secara realtime:
   ```bash
   sudo journalctl -u aurabot -f
   ```

---

## 📂 Struktur Database

Bot menggunakan SQLite dan akan otomatis membuat file `aura.db` ketika dijalankan.
- **Settings**: Menyimpan konfigurasi Setup Preset (Tip & Priority Fee).
- **Limit Orders**: Menyimpan history limit order pengguna.
- **Error Logs**: Menyimpan log saat limit order gagal dieksekusi (bisa dilihat via menu *Limit Order Logs*).
- **Chats**: Menyimpan ID pengguna yang pernah berinteraksi agar bot bisa mengirim notifikasi auto-sell secara otomatis.

---
*Bot siap digunakan! Buka Telegram, cari bot Anda, dan ketik `/start`.*
