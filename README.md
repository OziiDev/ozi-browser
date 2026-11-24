# Ozi Browser

Rust ile yazılmış, hafif bir masaüstü tarayıcı. **Chrome (MV2/MV3) ve Firefox WebExtension** paketlerini yükleyebilir.
Linux'ta WebKitGTK, Windows'ta WebView2 üzerinde çalışır; arayüz, sekmeler, arama, uzantı çalışma zamanı ve depolama Rust'tır.

- **Arama:** varsayılan **DuckDuckGo**, ikinci sırada **Google**. Adres çubuğunda `g kelime` → Google, `d kelime` → DuckDuckGo, `b` → Brave, `bing` → Bing.
- **Uzantılar:** `.crx`, `.xpi`, `.zip` ya da açılmış klasör. Ayrıntılar aşağıda.
- **Reklam/izleyici engelleme (Linux):** yerleşik alan adı listesi + uzantıların `declarativeNetRequest` statik kuralları, WebKit içerik filtresi olarak uygulanır.
- **Geçmiş, yer imleri, indirmeler, gizli sekme, sayfada ara, ayarlar** (arama motorlarını düzenleme dahil). Pencere boyutu hatırlanır; istenirse açılışta önceki sekmeler geri yüklenir (Ayarlar). Kapatılan sekme `Ctrl+Shift+T` ile geri açılır.
- **Akıcılık:** WebKit yumuşak kaydırma, sayfa önbelleği (anında geri/ileri) ve tarayıcı önbellek modu açık.
- **Arayüz:** açık/koyu tema (sistemi izler), sekme açılış/kapanış animasyonları, favicon'lar, yükleme çubuğu, adres çubuğunda açılır öneri listesi (yer imleri + geçmiş, ok tuşlarıyla gezinme), satır içi tamamlama ve "hangi motorla aranacak" çipi, hareketli yeni sekme sayfası (saat, en çok ziyaret edilen siteler). `prefers-reduced-motion` açıksa animasyonlar kapanır.
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
ozi-browser --private https://example.com   # gizli sekmede aç
ozi-browser https://example.com "rust dili"  # URL'ler ve aramalar sekme olarak açılır
ozi-browser --install ./uzanti-klasoru.xpi   # uzantı kur, sonra aç
```

Kısayollar (Linux'ta sayfa odaktayken de çalışır; Windows'ta yalnızca arayüz çubuğuna odaklıyken):

| Tuş | İşlev | Tuş | İşlev |
|---|---|---|---|
| `Ctrl+T` | yeni sekme | `Ctrl+Shift+N` | gizli sekme |
| `Ctrl+W` | sekmeyi kapat | `Ctrl+Tab` / `Ctrl+Shift+Tab` | sonraki / önceki sekme |
| `Ctrl+1…8`, `Ctrl+9` | n. sekme / son sekme | `Ctrl+L` | adres çubuğu |
| `Ctrl+R`, `F5` | yenile | `Alt+←/→` | geri / ileri |
| `Ctrl+D` | yer imi ekle/kaldır | `Ctrl+F` | sayfada ara (`Enter`/`Shift+Enter`, `Esc`) |
| `Ctrl+H` | geçmiş | `Ctrl+J` | indirmeler |
| `F11` | tam ekran | `F12` | geliştirici araçları |
| `Ctrl+Shift+T` | kapatılan sekmeyi geri aç | `Ctrl+Shift+PgUp/PgDn` | sekmeyi sola/sağa taşı |

`Ctrl +/-` yakınlaştırma sayfa tarafından yönetilir. İç sayfalar: `ozi://history`, `bookmarks`, `downloads`, `extensions`, `settings`.

Veriler: Linux `~/.local/share/ozi-browser` (profil, uzantılar, geçmiş/yer imleri/indirmeler, uzantı depolaması) ve `~/.config/ozi-browser/settings.json` (arama motorları, ana sayfa, reklam engelleme). Windows'ta `%LOCALAPPDATA%` ve `%APPDATA%`.

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
| `declarativeNetRequest` statik kural dosyaları (`block`, `allow`, `upgradeScheme`; `urlFilter`, `resourceTypes`, `domainType`, `initiatorDomains`) | ✅ Linux (WebKit içerik filtresi). `redirect`, `modifyHeaders`, dinamik kurallar, `requestDomains`: ❌ |
| `webRequest`, `contextMenus`, `cookies`, `notifications`, `runtime.connect` portları | ⚠️ taslak: çökmez ama **işlevsizdir** |

MV3 tarzı, statik kurallı engelleyiciler (uBlock Origin Lite, AdGuard MV3 gibi) kurallarının desteklenen kısmıyla çalışabilir; `webRequest` ile engelleyen eski MV2 uzantılar (uBlock Origin) **çalışmaz**. Betik enjekte eden, mesajlaşan, depolama kullanan uzantılar çalışır.

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
    userdata.rs   geçmiş, yer imleri, indirme kayıtları
    blocklist.rs  yerleşik liste + declarativeNetRequest -> WebKit kuralları
    settings.rs   ayarlar ve disk konumları
  ozi-browser/  uygulama
    main.rs       pencere, sekmeler, IPC, uzantı işlemleri
    ext.rs        enjeksiyon betikleri, arka plan sayfası, ozi-ext:// sunucusu
    ext_shim.js   chrome.* / browser.* API şimi
    blocking.rs   WebKit içerik filtresi (FFI, Linux)
    keys.rs       pencere düzeyinde kısayollar (Linux)
    ui.html       sekme + adres çubuğu arayüzü
    pages.rs      ozi:// iç sayfaları (yeni sekme, uzantılar, geçmiş, yer imleri, indirmeler, ayarlar)
```

Her sekme ayrı bir webview'dur; üstteki arayüz çubuğu da (HTML) ayrı bir webview'dur ve Rust ile IPC üzerinden konuşur.

## Güvenlik modeli ve sınırlar

- **Sayfa dünyasında içerik betikleri.** wry izole JavaScript dünyası sunmadığı için içerik betikleri sayfanın kendi JS ortamında çalışır (Chrome'da ayrı bir "isolated world"dür). Bunu hafifletmek için uzantı API'si kalıcı bir global'de tutulmaz; Rust↔sayfa iletişimi webview başına rastgele nonce ve rastgele adlı giriş noktasıyla yapılır; ayrıcalıklı işlemler (`tabs.*` vb.) yalnızca uzantının kendi sayfası/arka planı için açılır. Ancak sayfa betiği, `Array`/`Promise` gibi yerleşikleri değiştirerek içerik betiğine müdahale edebilir. Güvenmediğiniz sitelerde parola yöneticisi gibi hassas uzantılar kullanmayın.
- **Alt çerçeveler.** `window.ipc` yalnızca ana çerçevede vardır; `all_frames` içerik betikleri çalışır ama arka plana mesaj gönderemez.
- **Gizli sekme:** çerez/önbellek kalıcı değildir, geçmişe yazılmaz ve uzantılar çalışmaz. Reklam engelleme gizli sekmede de geçerlidir.
- **Reklam engelleme yalnızca Linux'ta.** Windows'ta (WebView2) istek filtreleme henüz yok; yerleşik liste ve `declarativeNetRequest` orada uygulanmaz. Yerleşik liste yalnızca *üçüncü taraf* isteklerini engeller ve kısa bir listedir; uBlock gibi kapsamlı filtre listeleri değildir. YouTube gibi sitelerin kendi reklamları engellenmez.
- **Öneri listesi** ayrı bir üst katman webview'udur; GTK'de en üstte olduğu doğrulandı ama ekranda çizimini ben göremedim (yerel pencere ekran görüntüsü alınamıyor). Sorun görürseniz bildirin.
- **Henüz yok:** senkronizasyon, parola yöneticisi, yazdırma arayüzü, istek *değiştirme*. Test kapsamı: çekirdek mantık birim testleriyle; uçtan uca uzantı akışı, reklam engelleme (yerel sunucuyla), geçmiş ve gizli mod Linux'ta elle doğrulandı. Kısayol tuşları ve arayüz düğmeleri elle gözden geçirilmedi. **Windows derlemesi yalnızca CI'da derlenir, elle denenmemiştir.**
- Eşleşmeyen bir uzantı API'si çağrıldığında sessizce no-op döner; bu uyumluluğu artırır ama hataları gizleyebilir.

## "Kendi motorum" sorusu

Sıfırdan bir web motoru (HTML/CSS/JS, ağ, güvenlik) yıllarca süren bir iştir; bu proje sistem webview'unu kullanır. Rust'ta motor yazma yönünde ilgi çekici işler var (ör. [Servo](https://servo.org), [Ladybird](https://ladybird.org)'nin ayrı bir yolu); Servo'yu isteğe bağlı ikinci bir arka uç olarak eklemek yol haritasındadır.

## Yol haritası

- [ ] Windows'ta istek filtreleme (WebView2 `WebResourceRequested`)
- [ ] Dinamik `declarativeNetRequest` kuralları ve `redirect`
- [ ] Alt çerçeveler için uzantı mesajlaşması (wry'nin `window.ipc`'si yalnızca ana çerçevede)
- [ ] Yer imi klasörleri / yer imi çubuğu
- [ ] İsteğe bağlı Servo arka ucu

## Lisans

MIT — bkz. [LICENSE](LICENSE).
