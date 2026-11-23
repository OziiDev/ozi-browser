# Ozi Browser

Rust ile yazılmış, hafif bir masaüstü tarayıcı. **Chrome (MV2/MV3) ve Firefox WebExtension** paketlerini yükleyebilir.
Linux'ta WebKitGTK, Windows'ta WebView2 üzerinde çalışır; arayüz, sekmeler, arama, uzantı çalışma zamanı ve depolama Rust'tır.

- **Arama:** varsayılan **DuckDuckGo**, ikinci sırada **Google**. Adres çubuğunda `g kelime` → Google, `d kelime` → DuckDuckGo, `b` → Brave, `bing` → Bing.
- **Uzantılar:** `.crx`, `.xpi`, `.zip` ya da açılmış klasör. Ayrıntılar aşağıda.
- **Küçük ve hızlı açılış:** Electron/Chromium gömmez; sistemin web motorunu kullanır. Release profili `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip`.
- **Windows:** imzasız `.exe` (GitHub Actions ile derlenir). SmartScreen uyarısı normaldir.

> Durum: **v0.1, erken aşama.** Aşağıdaki "Sınırlar" bölümünü okuyun.

## Kurulum

### Linux (Arch)

```bash
sudo pacman -S --needed webkit2gtk-4.1 gtk3 base-devel
git clone https://github.com/OziiDev/ozi-browser && cd ozi-browser
cargo build --release
./install.sh        # ~/.local/bin + uygulama menüsü (sudo gerekmez)
ozi-browser
```

Debian/Ubuntu için: `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev build-essential pkg-config`.

### Windows

Releases sayfasındaki `ozi-browser.exe` dosyasını çalıştırın (WebView2 çalışma zamanı Windows 10/11'de yüklü gelir).
Kaynaktan derlemek için: `cargo build --release`.

## Kullanım

```bash
ozi-browser                                  # yeni sekme sayfası
ozi-browser https://example.com "rust dili"  # URL'ler ve aramalar sekme olarak açılır
ozi-browser --install ./uzanti-klasoru.xpi   # uzantı kur, sonra aç
```

Kısayollar (arayüz çubuğuna odaklıyken): `Ctrl+T` yeni sekme, `Ctrl+W` kapat, `Ctrl+L` adres çubuğu, `Ctrl+R`/`F5` yenile, `Alt+←/→` geri/ileri.
Sayfadayken `Ctrl +/-` yakınlaştırma çalışır. Uzantı yönetimi: `ozi://extensions` (🧩 düğmesi).

Veriler: Linux `~/.local/share/ozi-browser` (profil, uzantılar, uzantı depolaması) ve `~/.config/ozi-browser/settings.json` (arama motorları sırası, ana sayfa). Windows'ta `%LOCALAPPDATA%` ve `%APPDATA%`.

## Uzantı desteği

| Özellik | Durum |
|---|---|
| `manifest.json` MV2 + MV3, Firefox `browser_specific_settings`/`applications` | ✅ |
| Paket biçimleri: klasör, `.zip`, `.xpi`, `.crx` (v2/v3) | ✅ (zip-slip ve zip-bomb korumalı) |
| İçerik betikleri (`matches`, `exclude_matches`, `include/exclude_globs`, `run_at`, `css`) | ✅ |
| Arka plan: MV2 `scripts`/`page`, MV3 `service_worker` (gizli sayfada, kalıcı) | ✅ |
| `runtime.sendMessage/onMessage`, `runtime.getURL/getManifest/id` | ✅ |
| `storage.local/sync/session` + `onChanged` (disk destekli, `sync` yereldir) | ✅ |
| `tabs.query/get/create/update/remove/reload/sendMessage/executeScript`, `scripting.executeScript/insertCSS` | ✅ (temel) |
| `i18n` (`_locales`, `__MSG_*__`), `alarms`, `action/browserAction` (popup, `onClicked`) | ✅ |
| `chrome.*` (callback) ve `browser.*` (Promise) biçimlerinin ikisi de | ✅ |
| `webRequest`, `declarativeNetRequest`, `contextMenus`, `cookies`, `notifications`, `runtime.connect` portları | ⚠️ taslak: çökmez ama **işlevsizdir** |

Bu yüzden reklam engelleyiciler (uBlock Origin, AdGuard…) ve ağ isteğini değiştiren uzantılar **henüz çalışmaz**; betik enjekte eden, mesajlaşan, depolama kullanan uzantılar çalışır.

## Mimari

```
crates/
  ozi-core/     arayüzden bağımsız mantık (test edilebilir, WebKit gerektirmez)
    search.rs     arama motorları + adres çubuğu çözümleme (DuckDuckGo → Google sırası)
    manifest.rs   Chrome/Firefox manifest modeli
    matcher.rs    match pattern + glob eşleştirme
    store.rs      uzantı kurulumu (klasör/zip/xpi/crx), etkin/devre dışı durumu
    extension.rs  güvenli dosya okuma, _locales
    storage.rs    chrome.storage deposu
    settings.rs   ayarlar ve disk konumları
  ozi-browser/  uygulama
    main.rs       pencere, sekmeler, IPC, uzantı işlemleri
    ext.rs        enjeksiyon betikleri, arka plan sayfası, ozi-ext:// sunucusu
    ext_shim.js   chrome.* / browser.* API şimi
    ui.html       sekme + adres çubuğu arayüzü
    pages.rs      ozi://newtab, ozi://extensions
```

Her sekme ayrı bir webview'dur; üstteki arayüz çubuğu da (HTML) ayrı bir webview'dur ve Rust ile IPC üzerinden konuşur.

## Güvenlik modeli ve sınırlar

- **Sayfa dünyasında içerik betikleri.** wry izole JavaScript dünyası sunmadığı için içerik betikleri sayfanın kendi JS ortamında çalışır (Chrome'da ayrı bir "isolated world"dür). Bunu hafifletmek için uzantı API'si kalıcı bir global'de tutulmaz; Rust↔sayfa iletişimi webview başına rastgele nonce ve rastgele adlı giriş noktasıyla yapılır; ayrıcalıklı işlemler (`tabs.*` vb.) yalnızca uzantının kendi sayfası/arka planı için açılır. Ancak sayfa betiği, `Array`/`Promise` gibi yerleşikleri değiştirerek içerik betiğine müdahale edebilir. Güvenmediğiniz sitelerde parola yöneticisi gibi hassas uzantılar kullanmayın.
- **Alt çerçeveler.** `window.ipc` yalnızca ana çerçevede vardır; `all_frames` içerik betikleri çalışır ama arka plana mesaj gönderemez.
- **Henüz yok:** geçmiş, yer imleri, parola yöneticisi, gizli sekme, kısayolların sayfa odaklıyken çalışması, istek engelleme. Test kapsamı: çekirdek mantık birim testleriyle, uçtan uca uzantı akışı (içerik betiği → arka plan → depolama) Linux'ta elle doğrulandı. **Windows derlemesi yalnızca CI'da derlenir, elle denenmemiştir.**
- Eşleşmeyen bir uzantı API'si çağrıldığında sessizce no-op döner; bu uyumluluğu artırır ama hataları gizleyebilir.

## "Kendi motorum" sorusu

Sıfırdan bir web motoru (HTML/CSS/JS, ağ, güvenlik) yıllarca süren bir iştir; bu proje sistem webview'unu kullanır. Rust'ta motor yazma yönünde ilgi çekici işler var (ör. [Servo](https://servo.org), [Ladybird](https://ladybird.org)'nin ayrı bir yolu); Servo'yu isteğe bağlı ikinci bir arka uç olarak eklemek yol haritasındadır.

## Yol haritası

- [ ] `declarativeNetRequest` (MV3 reklam engelleme) için istek filtreleme katmanı
- [ ] Geçmiş, yer imleri, indirme yöneticisi arayüzü
- [ ] Tuş kısayollarının sayfa odaklıyken çalışması (uygulama menüsü/hızlandırıcılar)
- [ ] Arama motorlarını arayüzden düzenleme
- [ ] Alt çerçeveler için uzantı mesajlaşması
- [ ] İsteğe bağlı Servo arka ucu

## Lisans

MIT — bkz. [LICENSE](LICENSE).
