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
  enterCode: "Enter the code",
  codePlaceholder: "42-river-candle",
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
  errDeclined: "The transfer was declined.",
  errNoPeer: "Could not reach the other device. Make sure both devices are online.",
  errGeneric: "The transfer could not be completed.",
  errCancelled: "The transfer was cancelled.",
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
  enterCode: "Kodu girin",
  codePlaceholder: "42-nehir-mum",
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
  errDeclined: "Aktarım reddedildi.",
  errNoPeer: "Diğer cihaza ulaşılamadı. İki cihazın da çevrimiçi olduğundan emin olun.",
  errGeneric: "Aktarım tamamlanamadı.",
  errCancelled: "Aktarım iptal edildi.",
  unknown: "—",
};

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
