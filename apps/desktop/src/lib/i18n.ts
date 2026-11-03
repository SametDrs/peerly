const en = {
  tagline: "Send files straight to another device. No upload, no account.",
  send: "Send",
  sendDesc: "Share files or a folder",
  receive: "Receive",
  receiveDesc: "Enter a code you were given",
  back: "Back",
  dropHere: "Drop files or a folder here",
  chooseFiles: "Choose files",
  chooseFolder: "Choose folder",
  selected: "{n} selected",
  items: "{n} item(s)",
  filesCount: "{n} file(s)",
  clear: "Clear",
  start: "Start",
  yourCode: "Your code",
  copy: "Copy",
  copied: "Copied",
  waiting: "Waiting for the receiver…",
  shareCode: "Tell the receiver this code. It works only once.",
  waitingHint: "The code works on the same Wi-Fi or over the internet.",
  connectingHint: "Looking on your local network and the internet. This can take a few seconds.",
  routeLan: "Local network",
  routeInternet: "Internet",
  enterCode: "Enter the code",
  codePlaceholder: "4821-river-candle",
  saveTo: "Save to",
  change: "Change",
  connect: "Connect",
  connecting: "Connecting…",
  connected: "Connected",
  incoming: "Incoming files",
  accept: "Accept",
  decline: "Decline",
  from: "Someone wants to send you",
  sending: "Sending",
  receiving: "Receiving",
  speed: "Speed",
  eta: "Time left",
  cancel: "Cancel",
  done: "All done!",
  sentSummary: "{files} file(s), {size} sent in {time}",
  receivedSummary: "{files} file(s), {size} received in {time}",
  openFolder: "Open folder",
  sendAnother: "Send another",
  receiveAnother: "Receive another",
  home: "Home",
  errorTitle: "Something went wrong",
  tryAgain: "Try again",
  details: "Details",
  errWrongCode: "That code did not work. Check it and try again. Each code can only be used once.",
  errCancelled: "The transfer was cancelled.",
  err_wrong_code: "That code did not match. Check it and try again.",
  err_code_not_found: "This code does not exist or has expired.",
  err_code_in_use: "This code is already in use. Start again for a new one.",
  err_invalid_code: "That does not look like a valid code (e.g. 4821-river-candle).",
  err_rendezvous_unreachable: "Could not reach the Peerly server. Check your internet connection.",
  err_peer_unreachable: "Could not make a direct connection to the other device.",
  err_peer_left: "The other device disconnected.",
  err_timeout: "Timed out waiting for the other device.",
  err_rejected: "The receiver declined the transfer.",
  err_peer_cancelled: "The other side cancelled the transfer.",
  err_disk_full: "Not enough free disk space for these files.",
  err_hash_mismatch: "A file arrived damaged. Please try again.",
  hintPeer: "Check the code, and make sure both devices are online. On Windows, allow Peerly through the firewall when asked. Some networks (corporate Wi-Fi, mobile hotspots) block direct connections.",
  err_other: "The transfer could not be completed.",
  unknown: "—",
};

const tr: typeof en = {
  tagline: "Dosyaları doğrudan başka bir cihaza gönderin. Yükleme yok, hesap yok.",
  send: "Gönder",
  sendDesc: "Dosya veya klasör paylaş",
  receive: "Al",
  receiveDesc: "Size verilen kodu girin",
  back: "Geri",
  dropHere: "Dosya veya klasörü buraya bırakın",
  chooseFiles: "Dosya seç",
  chooseFolder: "Klasör seç",
  selected: "{n} seçildi",
  items: "{n} öğe",
  filesCount: "{n} dosya",
  clear: "Temizle",
  start: "Başlat",
  yourCode: "Kodunuz",
  copy: "Kopyala",
  copied: "Kopyalandı",
  waiting: "Alıcı bekleniyor…",
  shareCode: "Bu kodu alıcıya söyleyin. Yalnızca bir kez çalışır.",
  waitingHint: "Kod aynı Wi-Fi ağında da internet üzerinden de çalışır.",
  connectingHint: "Yerel ağda ve internette aranıyor. Bu birkaç saniye sürebilir.",
  routeLan: "Yerel ağ",
  routeInternet: "İnternet",
  enterCode: "Kodu girin",
  codePlaceholder: "4821-nehir-mum",
  saveTo: "Kaydedilecek yer",
  change: "Değiştir",
  connect: "Bağlan",
  connecting: "Bağlanılıyor…",
  connected: "Bağlandı",
  incoming: "Gelen dosyalar",
  accept: "Kabul et",
  decline: "Reddet",
  from: "Birisi size göndermek istiyor",
  sending: "Gönderiliyor",
  receiving: "Alınıyor",
  speed: "Hız",
  eta: "Kalan süre",
  cancel: "İptal",
  done: "Tamamlandı!",
  sentSummary: "{files} dosya, {size}, {time} içinde gönderildi",
  receivedSummary: "{files} dosya, {size}, {time} içinde alındı",
  openFolder: "Klasörü aç",
  sendAnother: "Yeni gönderim",
  receiveAnother: "Yeni alım",
  home: "Ana sayfa",
  errorTitle: "Bir şeyler ters gitti",
  tryAgain: "Tekrar dene",
  details: "Ayrıntılar",
  errWrongCode: "Bu kod çalışmadı. Kontrol edip tekrar deneyin. Her kod yalnızca bir kez kullanılabilir.",
  errCancelled: "Aktarım iptal edildi.",
  err_wrong_code: "Kod eşleşmedi. Kontrol edip tekrar deneyin.",
  err_code_not_found: "Bu kod yok ya da süresi dolmuş.",
  err_code_in_use: "Bu kod zaten kullanımda. Yeni bir kod için baştan başlayın.",
  err_invalid_code: "Bu geçerli bir koda benzemiyor (örn. 4821-nehir-mum).",
  err_rendezvous_unreachable: "Peerly sunucusuna ulaşılamadı. İnternet bağlantınızı kontrol edin.",
  err_peer_unreachable: "Diğer cihazla doğrudan bağlantı kurulamadı.",
  err_peer_left: "Diğer cihazın bağlantısı koptu.",
  err_timeout: "Diğer cihaz beklenirken zaman aşımı oldu.",
  err_rejected: "Alıcı aktarımı reddetti.",
  err_peer_cancelled: "Karşı taraf aktarımı iptal etti.",
  err_disk_full: "Bu dosyalar için diskte yeterli boş alan yok.",
  err_hash_mismatch: "Bir dosya bozuk ulaştı. Lütfen tekrar deneyin.",
  hintPeer: "Kodu kontrol edin ve iki cihazın da çevrimiçi olduğundan emin olun. Windows'ta sorulursa Peerly'ye güvenlik duvarında izin verin. Bazı ağlar (kurumsal Wi-Fi, mobil hotspot) doğrudan bağlantıyı engeller.",
  err_other: "Aktarım tamamlanamadı.",
  unknown: "—",
};

export const PEER_HINT_CODES = ["code_not_found", "peer_unreachable", "timeout", "wrong_code", "rendezvous_unreachable"];

export const ERR_CODES = ["wrong_code", "code_not_found", "code_in_use", "invalid_code", "rendezvous_unreachable", "peer_unreachable", "peer_left", "timeout", "rejected", "peer_cancelled", "disk_full", "hash_mismatch", "other"];

export type Key = keyof typeof en;
const lang = (navigator.language || "en").toLowerCase().startsWith("tr") ? "tr" : "en";
const dict = lang === "tr" ? tr : en;

export function t(key: Key, params: Record<string, string | number> = {}): string {
  return dict[key].replace(/\{(\w+)\}/g, (_, k) => String(params[k] ?? ""));
}

export function formatBytes(n: number): string {
  if (n < 1000) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n;
  let i = -1;
  do {
    v /= 1000;
    i++;
  } while (v >= 1000 && i < units.length - 1);
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return t("unknown");
  const s = Math.round(sec);
  if (s < 60) return `${s} ${lang === "tr" ? "sn" : "s"}`;
  const m = Math.floor(s / 60);
  const r = s % 60;
  if (m < 60) return `${m} ${lang === "tr" ? "dk" : "min"} ${r} ${lang === "tr" ? "sn" : "s"}`;
  return `${Math.floor(m / 60)} ${lang === "tr" ? "sa" : "h"} ${m % 60} ${lang === "tr" ? "dk" : "min"}`;
}
